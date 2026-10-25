/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! What the Docker client sends a daemon, pinned per phase against a fake Engine that replays the
//! engine-wire captures of API 1.51 and 1.56: the request half of the Go engine-wire test.
//!
//! The requests are compared in a normalised form. Go's test relaxes the moby client v0.6.0 wire
//! against its v28 baseline through the allowances W1–W5; this one starts from v0.6.0's
//! expectations (all five applied) and relaxes them only through [`ALLOWANCES`], R1…, each a
//! difference of the Rust client within Q11. Each phase issues the calls the Go collectors make
//! for these fixtures, directly on the client; the collectors take over that driving as they are
//! ported, and the metrics half of the Go test (the golden file) lands with the wiring.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use swarm_scheduler_exporter::docker::types::Event;
use swarm_scheduler_exporter::docker::{
    Client, ClientEventStream, DockerApi, ErrorKind, EventStream, Filters, StreamErrorKind,
    with_docker_timeout,
};

use support::OrPanic as _;
use support::fake_engine::{
    EventsConnection, FIXTURE_SETS, FakeEngine, RecordedRequest, StreamEnd,
};

/// Bounds a whole test, so a hung stream fails it instead of hanging it.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The Go listener's `eventsSinceMargin`: every event-stream anchor starts this much earlier.
const EVENTS_SINCE_MARGIN: SignedDuration = SignedDuration::from_millis(500);

/// The `timeNano` of the reconnect fixture's event. Its fraction (.2) is below half a second, so
/// the resume point crosses a second boundary (…299.7), which only nanosecond resolution keeps.
const RECONNECT_EVENT_TIME_NANO: i128 = 1_789_902_300_200_000_000;

/// The request properties a client version may change; everything else is compared through the
/// normalised requests.
#[derive(Debug, Clone)]
struct WireExpectations {
    /// W1: the `/vX.YY` prefix of every versioned request.
    version_prefix: &'static str,
    /// W2: the method and path of the first request of a run, the version negotiation.
    first_request: &'static str,
    /// W2: how many pings a run sends.
    pings: usize,
    /// W3: the `Accept` lines of `GET /events`, joined with `, `.
    events_accept: &'static str,
    /// W4: the prefix of the `User-Agent`.
    user_agent_prefix: &'static str,
    /// W5: the query of `GET /services/{id}`.
    service_inspect_query: &'static str,
}

/// What moby client v0.6.0 sends: Go's v28 baseline with W1–W5 applied.
const GO_WIRE: WireExpectations = WireExpectations {
    version_prefix: "/v1.56",
    first_request: "HEAD /_ping",
    pings: 1,
    events_accept: "application/jsonl, application/x-ndjson;q=0.9, application/json-seq;q=0.5",
    user_agent_prefix: "moby-client/",
    service_inspect_query: "",
};

/// One difference of the Rust client from moby client v0.6.0, and why the daemon treats the
/// requests the same.
struct Allowance {
    id: &'static str,
    relaxes: &'static str,
    evidence: &'static str,
    apply: fn(&mut WireExpectations),
}

/// The allowance table.
const ALLOWANCES: [Allowance; 2] = [
    Allowance {
        id: "R1",
        relaxes: "W4",
        evidence: "Q11: the client sends User-Agent swarm-scheduler-exporter/<version> instead of \
                   moby-client/<version> <os>/<arch>, to identify the exporter in daemon and \
                   proxy logs; the daemon only logs it and changes none of these responses on it.",
        apply: |expectations| expectations.user_agent_prefix = "swarm-scheduler-exporter/",
    },
    Allowance {
        id: "R2",
        relaxes: "W2",
        evidence: "Q11: concurrent first requests share one in-flight ping, where moby sends one \
                   per concurrent first request (8 for 8 in S1). This run's requests are \
                   sequential, so both send exactly one HEAD /_ping first and the assertion \
                   stays as it is; src/docker/tests.rs pins the concurrent case.",
        apply: |_| {},
    },
];

fn expectations() -> WireExpectations {
    let mut expectations = GO_WIRE;
    for allowance in &ALLOWANCES {
        assert!(
            !allowance.evidence.is_empty() && allowance.relaxes.starts_with('W'),
            "allowance {} has no evidence or relaxes nothing named",
            allowance.id
        );
        (allowance.apply)(&mut expectations);
    }
    expectations
}

fn service_inspect_request(query: &str, service_id: &str) -> String {
    if query.is_empty() {
        format!("GET /services/{service_id}")
    } else {
        format!("GET /services/{service_id}?{query}")
    }
}

fn normalized(entries: &[RecordedRequest]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry.normalized.clone())
        .collect()
}

fn assert_sequence(phase: &str, got: &[RecordedRequest], want: &[&str]) {
    assert_eq!(normalized(got), want, "{phase} requests");
}

fn assert_multiset(phase: &str, got: &[RecordedRequest], want: &[String]) {
    let mut got = normalized(got);
    let mut want = want.to_vec();
    got.sort();
    want.sort();
    assert_eq!(got, want, "{phase} requests (as a multiset)");
}

fn requests_to<'entries>(
    entries: &'entries [RecordedRequest],
    path: &str,
) -> Vec<&'entries RecordedRequest> {
    entries.iter().filter(|entry| entry.path == path).collect()
}

/// The `since` of an `/events` request: `<seconds>` or `<seconds>.<nanoseconds>`.
fn events_since(entry: &RecordedRequest) -> Timestamp {
    let raw = entry.query_value("since").unwrap_or_default();
    let (seconds, nanos) = raw.split_once('.').unwrap_or((raw, "0"));
    Timestamp::new(
        seconds.parse::<i64>().or_panic(),
        nanos.parse::<i32>().or_panic(),
    )
    .or_panic()
}

/// W1, W3, W4 and W5 on every request of the run, W2 on its order and its pings, the `Host`
/// header, and the method: `HEAD` only for the ping, `GET` for everything else.
fn assert_wire_properties(engine: &FakeEngine, entries: &[RecordedRequest]) {
    let expectations = expectations();
    let first = entries
        .first()
        .map(|entry| format!("{} {}", entry.method, entry.path));
    assert_eq!(
        first.as_deref(),
        Some(expectations.first_request),
        "W2: first request"
    );
    let pings = requests_to(entries, "/_ping");
    assert_eq!(pings.len(), expectations.pings, "W2: pings in the run");
    let host = engine.addr().to_string();
    for entry in entries {
        assert_eq!(
            entry.host, host,
            "Host of {} {}",
            entry.method, entry.raw_path
        );
        assert!(
            entry.user_agent.starts_with(expectations.user_agent_prefix),
            "W4: {} {} User-Agent = {:?}",
            entry.method,
            entry.raw_path,
            entry.user_agent
        );
        if entry.path == "/_ping" {
            assert_eq!(
                entry.method, "HEAD",
                "the ping, answered with 200, is never retried as GET"
            );
            assert_eq!(entry.version_prefix, "", "the ping is unversioned");
            continue;
        }
        assert_eq!(entry.method, "GET", "{}", entry.raw_path);
        assert_eq!(
            entry.version_prefix, expectations.version_prefix,
            "W1: {}",
            entry.raw_path
        );
        if entry.path == "/events" {
            assert_eq!(
                entry.accept, expectations.events_accept,
                "W3: GET /events Accept"
            );
        }
        if entry.path.starts_with("/services/") {
            assert_eq!(
                entry.raw_query, expectations.service_inspect_query,
                "W5: {}",
                entry.raw_path
            );
        }
    }
    assert_eq!(engine.unexpected_requests(), Vec::<String>::new());
}

fn swarm_filters() -> Filters {
    Filters::new().add("type", &["service", "node"])
}

fn fixture_connection(engine: &FakeEngine, fixture: &str, end: StreamEnd) -> EventsConnection {
    EventsConnection {
        chunks: vec![engine.fixture(fixture)],
        end,
    }
}

/// Opens the stream from `since`, as the Go listener opens it.
async fn open_events(client: &Client, since: Timestamp) -> ClientEventStream {
    client
        .events(since, &swarm_filters())
        .await
        .unwrap_or_else(|error| panic!("events: {error}"))
}

/// What the Go reconciler asks for an event: an inspect of the service, or the node list.
async fn react(client: &Client, event: &Event) {
    match event.kind.as_str() {
        "service" => {
            with_docker_timeout(client.service_inspect(&event.actor.id))
                .await
                .or_panic();
        }
        "node" => {
            with_docker_timeout(client.node_list()).await.or_panic();
        }
        other => panic!("unexpected event type {other}"),
    }
}

async fn pins_requests(set_index: usize) {
    let engine = FakeEngine::start(FIXTURE_SETS[set_index], HashMap::new()).await;
    let client = engine.client();
    // The event-stream anchor main captures before the first resync, with a fraction, so the
    // since checks see nanoseconds.
    let anchor: Timestamp = "2026-09-20T11:30:00.123456789Z".parse().or_panic();
    resync_and_polls(&engine, &client).await;
    let since = anchor.checked_sub(EVENTS_SINCE_MARGIN).or_panic();
    events_happy_path(&engine, &client, since).await;
    events_reconnect(&engine, &client, since).await;
    assert_wire_properties(&engine, &engine.all_requests());
}

/// The first resync, the task poll and the container poll.
async fn resync_and_polls(engine: &FakeEngine, client: &Client) {
    // Phase 1: the first resync, sequential: services, then nodes.
    engine.reset();
    let services = with_docker_timeout(client.service_list()).await.or_panic();
    let nodes = with_docker_timeout(client.node_list()).await.or_panic();
    assert_eq!((services.len(), nodes.len()), (4, 4));
    assert_sequence(
        "first resync",
        &engine.phase_requests(),
        &["GET /services", "GET /nodes"],
    );

    // Phase 2: the task poll, one unfiltered list.
    engine.reset();
    assert_eq!(
        with_docker_timeout(client.task_list())
            .await
            .or_panic()
            .len(),
        9
    );
    assert_sequence("task poll", &engine.phase_requests(), &["GET /tasks"]);

    // Phase 3: the container poll, the list, then one inspect per running or exited container,
    // in list order; the paused one is not inspected.
    engine.reset();
    let containers = with_docker_timeout(client.container_list())
        .await
        .or_panic();
    for container in &containers {
        if matches!(container.state.as_str(), "running" | "exited") {
            with_docker_timeout(client.container_inspect(&container.id))
                .await
                .or_panic();
        }
    }
    assert_sequence(
        "container poll",
        &engine.phase_requests(),
        &[
            "GET /containers/json?all=1",
            "GET /containers/c-compose-web/json",
            "GET /containers/c-compose-worker/json",
            "GET /containers/c-plain/json",
            "GET /containers/c-swarm-api/json",
        ],
    );
}

/// Events, happy path: one connection streams a service and a node update, then stays open;
/// each event triggers its request.
async fn events_happy_path(engine: &FakeEngine, client: &Client, since: Timestamp) {
    let expectations = expectations();
    engine.reset();
    engine.script_events(vec![fixture_connection(
        engine,
        "events-happy.jsonl",
        StreamEnd::Hold,
    )]);
    let mut stream = open_events(client, since).await;
    let mut actors = Vec::new();
    for _ in 0..2 {
        let event = stream.next().await.or_panic();
        react(client, &event).await;
        actors.push(event.actor.id);
    }
    assert_eq!(actors, ["svc-api", "node-w1"]);
    drop(stream);
    let event_requests = engine.phase_requests();
    assert_multiset(
        "events",
        &event_requests,
        &[
            "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
            service_inspect_request(expectations.service_inspect_query, "svc-api"),
            "GET /nodes".to_owned(),
        ],
    );
    assert_eq!(
        event_requests[0].path, "/events",
        "the stream is opened first"
    );
    let connections = requests_to(&event_requests, "/events");
    assert_eq!(connections.len(), 1);
    assert_eq!(
        events_since(connections[0]),
        since,
        "the anchor minus the margin, to the ns"
    );
}

/// Events, reconnect: the first connection streams one event and ends cleanly; the second
/// resumes the margin before that event and stays open, and the reconnect resyncs.
async fn events_reconnect(engine: &FakeEngine, client: &Client, since: Timestamp) {
    let expectations = expectations();
    engine.reset();
    engine.script_events(vec![
        fixture_connection(engine, "events-reconnect.jsonl", StreamEnd::Close),
        EventsConnection {
            chunks: Vec::new(),
            end: StreamEnd::Hold,
        },
    ]);
    let mut stream = open_events(client, since).await;
    let event = stream.next().await.or_panic();
    react(client, &event).await;
    assert_eq!(
        event.time_nano,
        i64::try_from(RECONNECT_EVENT_TIME_NANO).or_panic()
    );
    let end = stream.next().await.or_panic_err();
    assert!(
        end.is_eof(),
        "a stream ended by its terminating chunk is a clean EOF: {end}"
    );
    drop(stream);
    let last_event = Timestamp::from_nanosecond(i128::from(event.time_nano)).or_panic();
    let resume = last_event.checked_sub(EVENTS_SINCE_MARGIN).or_panic();
    let held = open_events(client, resume).await;
    with_docker_timeout(client.service_list()).await.or_panic();
    with_docker_timeout(client.node_list()).await.or_panic();
    drop(held);
    let reconnect_requests = engine.phase_requests();
    assert_multiset(
        "events reconnect",
        &reconnect_requests,
        &[
            "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
            service_inspect_request(expectations.service_inspect_query, "svc-db"),
            "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
            "GET /services".to_owned(),
            "GET /nodes".to_owned(),
        ],
    );
    let connections = requests_to(&reconnect_requests, "/events");
    assert_eq!(connections.len(), 2);
    assert_eq!(
        connections[1].query_value("since"),
        Some("1789902299.700000000")
    );
    assert_eq!(events_since(connections[1]), resume);
}

/// Go: `TestEngineWire_PinsRequestsAndMetrics`, the request half (the metrics half lands with
/// the wiring), for API 1.51.
#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_api_1_51() {
    tokio::time::timeout(TEST_TIMEOUT, pins_requests(0))
        .await
        .or_panic();
}

/// Go: `TestEngineWire_PinsRequestsAndMetrics`, the request half, for API 1.56.
#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_api_1_56() {
    tokio::time::timeout(TEST_TIMEOUT, pins_requests(1))
        .await
        .or_panic();
}

/// Socket proxies such as `HAProxy` reject a request line past their buffer (16 KB by default);
/// 8 KB leaves room for the headers that share it.
const MAX_TASK_LIST_REQUEST_URI: usize = 8 << 10;

/// Go: `TestEngineWire_TaskListHasNoFilter`. The Go test fills the service cache with 10001
/// services first, to show that none of them reaches the request; here `task_list` takes no
/// filter at all, so the request cannot grow with the cache, and this pins its wire form.
#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_task_list_has_no_filter() {
    let overrides = HashMap::from([("/tasks".to_owned(), b"[]".to_vec())]);
    let engine = FakeEngine::start(FIXTURE_SETS[0], overrides).await;
    let client = engine.client();
    engine.reset();
    let tasks = tokio::time::timeout(TEST_TIMEOUT, with_docker_timeout(client.task_list()))
        .await
        .or_panic()
        .or_panic();
    assert!(tasks.is_empty());
    let requests = engine.phase_requests();
    assert_eq!(requests.len(), 1, "exactly one GET /tasks");
    assert_eq!(
        (requests[0].method.as_str(), requests[0].path.as_str()),
        ("GET", "/tasks")
    );
    assert_eq!(requests[0].query_value("filters"), None);
    let mut request_uri = requests[0].raw_path.clone();
    if !requests[0].raw_query.is_empty() {
        request_uri.push('?');
        request_uri.push_str(&requests[0].raw_query);
    }
    assert!(
        request_uri.len() < MAX_TASK_LIST_REQUEST_URI,
        "{request_uri}"
    );
    assert_wire_properties(&engine, &engine.all_requests());
}

/// Every event of `stream` until it ends, and how it ended.
async fn drain(stream: &mut ClientEventStream) -> (Vec<String>, StreamErrorKind) {
    let mut actors = Vec::new();
    loop {
        match stream.next().await {
            Ok(event) => actors.push(event.actor.id),
            Err(end) => return (actors, end.kind()),
        }
    }
}

fn record_separated(lines: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines.split_inclusive(|byte| *byte == b'\n') {
        out.push(0x1E);
        out.extend_from_slice(line);
    }
    out
}

/// A daemon streaming JSON text sequences: the client filters the record separators wherever
/// the chunks split them, and a tail of separators alone is still a clean end.
#[tokio::test(flavor = "multi_thread")]
async fn events_stream_json_seq_end_to_end() {
    let engine =
        FakeEngine::start_with_events_type(FIXTURE_SETS[1], "application/json-seq", HashMap::new())
            .await;
    let body = record_separated(&engine.fixture("events-happy.jsonl"));
    // The second record's separator ends the first chunk, and its JSON is split again.
    let second = body
        .iter()
        .rposition(|byte| *byte == 0x1E)
        .unwrap_or_default();
    let (first, rest) = body.split_at(second + 1);
    let (middle, last) = rest.split_at(15);
    engine.script_events(vec![EventsConnection {
        chunks: vec![
            first.to_vec(),
            middle.to_vec(),
            last.to_vec(),
            vec![0x1E],
            vec![0x1E, b'\n'],
        ],
        end: StreamEnd::Close,
    }]);
    let client = engine.client();
    let mut stream = open_events(&client, Timestamp::UNIX_EPOCH).await;
    assert!(stream.filters_rs());
    let (actors, end) = tokio::time::timeout(TEST_TIMEOUT, drain(&mut stream))
        .await
        .or_panic();
    assert_eq!(actors, ["svc-api", "node-w1"]);
    assert_eq!(end, StreamErrorKind::Eof);
}

/// A connection closed without the terminating chunk, between values or inside one, is never a
/// clean end: what a proxy's timeout or a killed daemon does (S1). Nor is a body that ends
/// inside a value, terminating chunk or not.
#[tokio::test(flavor = "multi_thread")]
async fn events_stream_cut_is_an_unexpected_eof() {
    let engine = FakeEngine::start(FIXTURE_SETS[1], HashMap::new()).await;
    let happy = engine.fixture("events-happy.jsonl");
    engine.script_events(vec![
        EventsConnection {
            chunks: vec![happy.clone()],
            end: StreamEnd::Cut,
        },
        EventsConnection {
            chunks: vec![happy[..40].to_vec()],
            end: StreamEnd::Cut,
        },
        EventsConnection {
            chunks: vec![happy[..40].to_vec()],
            end: StreamEnd::Close,
        },
    ]);
    let client = engine.client();
    for want in [
        (
            vec!["svc-api".to_owned(), "node-w1".to_owned()],
            StreamErrorKind::UnexpectedEof,
        ),
        (Vec::new(), StreamErrorKind::UnexpectedEof),
        (Vec::new(), StreamErrorKind::UnexpectedEof),
    ] {
        let mut stream = open_events(&client, Timestamp::UNIX_EPOCH).await;
        let got = tokio::time::timeout(TEST_TIMEOUT, drain(&mut stream))
            .await
            .or_panic();
        assert_eq!(got, want);
    }
}

/// An events request the daemon refuses yields no stream at all (Q6): the connect failed.
#[tokio::test(flavor = "multi_thread")]
async fn events_refused_yields_no_stream() {
    let engine = FakeEngine::start(FIXTURE_SETS[1], HashMap::new()).await;
    let client = engine.client();
    let error = client
        .events(Timestamp::UNIX_EPOCH, &swarm_filters())
        .await
        .or_panic_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(
        error.to_string(),
        "Error response from daemon: unexpected extra events request"
    );
}

/// The daemon's 404 for a removed service is containerd's `NotFound`, which the reconciler
/// tells apart from other failures.
#[tokio::test(flavor = "multi_thread")]
async fn service_inspect_of_a_removed_service_is_not_found() {
    let engine = FakeEngine::start(FIXTURE_SETS[1], HashMap::new()).await;
    let client = engine.client();
    let error = with_docker_timeout(client.service_inspect("svc-gone"))
        .await
        .or_panic_err();
    assert!(error.is_not_found());
    assert_eq!(
        error.to_string(),
        "Error response from daemon: service svc-gone not found"
    );
    let found = with_docker_timeout(client.service_inspect(" svc-db "))
        .await
        .or_panic();
    assert_eq!(found.id, "svc-db");
    let missing = with_docker_timeout(client.container_inspect("c-gone"))
        .await
        .or_panic_err();
    assert!(missing.is_not_found());
}
