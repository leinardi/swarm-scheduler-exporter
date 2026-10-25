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

//! What the exporter sends a daemon, and the metrics it computes from the answers, pinned per
//! phase against a fake Engine that replays the engine-wire captures of API 1.51 and 1.56: the
//! port of the Go engine-wire test.
//!
//! The requests are compared in a normalised form. Go's test relaxes the moby client v0.6.0 wire
//! against its v28 baseline through the allowances W1–W5; this one starts from v0.6.0's
//! expectations (all five applied) and relaxes them only through [`ALLOWANCES`], R1…, each a
//! difference of the Rust client within Q11. The collectors are driven as `run` drives them, one
//! phase at a time, and the families the Docker answers feed are compared against the golden
//! file Go's test pins.

mod support;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use swarm_scheduler_exporter::clock::{Clock as _, SystemClock};
use swarm_scheduler_exporter::collector::containers::ContainersConfig;
use swarm_scheduler_exporter::collector::reconciler::{
    BACKOFF_INITIAL_DELAY, ListenerStopped, Reconciler, ReconcilerHandle, listen_swarm_events,
    poll_and_publish_replicas_state,
};
use swarm_scheduler_exporter::collector::types::CustomLabels;
use swarm_scheduler_exporter::docker::{
    Client, ClientEventStream, DockerApi, ErrorKind, EventStream, Filters, StreamErrorKind,
    with_docker_timeout,
};
use swarm_scheduler_exporter::labels::validate_and_sanitize_label_names;
use swarm_scheduler_exporter::logger::{Config as LogConfig, Format, Level, Logger};
use swarm_scheduler_exporter::metrics::{Gathered, GatheredSeries, Registry, Value, text};
use swarm_scheduler_exporter::run::{Metrics, MetricsOptions, register_metrics};

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

/// The custom label exported through `--label`, so the pin covers the custom-label path of every
/// per-service family: Go's `engineWireCustomLabel`.
const CUSTOM_LABEL: &str = "com.example.team";

/// The golden metrics, captured from the Go exporter against the same fixtures.
const GOLDEN_FILE: &str = "tests/testdata/engine_wire/metrics.golden";

/// How long a reconnect gets to show up on the happy path, after the listener's first backoff.
const RECONNECT_MARGIN: Duration = Duration::from_millis(250);

/// The registered families the golden file deliberately does not pin, because their values
/// depend on timing or do not come from the Docker API: Go's `engineWireExcludedFamilies`.
const EXCLUDED_FAMILIES: [(&str, &str); 11] = [
    ("swarm_exporter_poll_duration_seconds", "timing-dependent"),
    (
        "swarm_exporter_polls_total",
        "counts poll cycles, not SDK data",
    ),
    (
        "swarm_exporter_poll_errors_total",
        "counts poll cycles, not SDK data",
    ),
    (
        "swarm_exporter_health",
        "derived from wall-clock poll freshness",
    ),
    ("swarm_exporter_build_info", "ldflags, not SDK data"),
    (
        "swarm_exporter_events_reconnects_total",
        "asserted by the reconnect case instead",
    ),
    (
        "swarm_exporter_events_dropped_total",
        "asserted by the reconciler unit tests instead",
    ),
    (
        "swarm_exporter_poll_rejections_total",
        "asserted by the poll protocol unit tests instead",
    ),
    (
        "swarm_exporter_resyncs_total",
        "asserted by the reconciler unit tests instead",
    ),
    (
        "swarm_exporter_last_poll_success_timestamp_seconds",
        "wall-clock time",
    ),
    (
        "swarm_exporter_last_resync_success_timestamp_seconds",
        "wall-clock time",
    ),
];

fn excluded(family: &str) -> bool {
    EXCLUDED_FAMILIES.iter().any(|(name, _)| *name == family)
}

fn quiet_logger() -> Logger {
    Logger::new(
        LogConfig {
            format: Format::Text,
            level: Level::INFO,
            include_time: false,
        },
        Box::new(std::io::sink()),
        TimeZone::UTC,
        Arc::new(SystemClock),
    )
}

/// Go's `configureCollectorsLikeMain`: every family registered as `run` registers it with
/// `--containers --containers-include-swarm` and `--label com.example.team`, on a private
/// registry.
fn configure_collectors_like_main(registry: &Registry) -> Metrics {
    let raw = [CUSTOM_LABEL.to_owned()];
    let sanitized = validate_and_sanitize_label_names(&raw).or_panic();
    register_metrics(
        registry,
        &MetricsOptions {
            custom_labels: CustomLabels::new(&raw, &sanitized),
            poll_delay: Duration::from_secs(10),
            containers: ContainersConfig::new(true, true),
            build: ["test".to_owned(), "none".to_owned(), "unknown".to_owned()],
        },
        Arc::new(SystemClock),
        &quiet_logger(),
    )
}

/// Polls `condition` every 10ms until it holds, failing with `what` at the test's deadline.
async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Go's `assertNoSecondEventsConnection`: watches the phase for `window`, failing as soon as a
/// second `GET /events` appears.
async fn assert_no_second_events_connection(engine: &FakeEngine, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let connections = requests_to(&engine.phase_requests(), "/events").len();
        assert!(
            connections <= 1,
            "happy path: the event stream reconnected ({connections} /events requests, want \
             exactly 1)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Go's `runListener`: the listener from `anchor` until the returned stop, which cancels it and
/// requires it to return within 5s.
struct Listener {
    cancel: CancellationToken,
    task: JoinHandle<ListenerStopped>,
}

impl Listener {
    fn start(client: &Arc<Client>, handle: &ReconcilerHandle, anchor: Timestamp) -> Self {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let client = Arc::clone(client);
            let handle = handle.clone();
            let cancel = cancel.clone();
            async move { listen_swarm_events(&*client, &handle, anchor, &cancel).await }
        });
        Self { cancel, task }
    }

    async fn stop(self) {
        self.cancel.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap_or_else(|_| panic!("the listener did not return within 5s of cancellation"))
            .or_panic();
        assert!(
            stopped.to_string().ends_with("context canceled"),
            "{stopped}"
        );
    }
}

/// The value of the `swarm_service_desired_replicas` series of `stack` and `service`.
fn desired_replicas(registry: &Registry, stack: &str, service: &str) -> Option<f64> {
    let families = registry.gather();
    let family = families
        .iter()
        .find(|family| family.name == "swarm_service_desired_replicas")?;
    let label = |series: &GatheredSeries, name: &str| {
        series
            .labels
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    family
        .series
        .iter()
        .find(|series| {
            label(series, "stack").as_deref() == Some(stack)
                && label(series, "service").as_deref() == Some(service)
        })
        .and_then(|series| match series.value {
            Value::Scalar(value) => Some(value),
            Value::Histogram { .. } => None,
        })
}

/// Go's `pinnedFamilies`: every registered `swarm_` family not excluded.
fn pinned_families(registered: &BTreeSet<String>) -> BTreeSet<String> {
    registered
        .iter()
        .filter(|family| family.starts_with("swarm_") && !excluded(family))
        .cloned()
        .collect()
}

fn golden_path() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), GOLDEN_FILE].iter().collect()
}

/// Go's `GatherAndCompare` of the pinned families against the golden file, which holds them in
/// expfmt's text format, as Go's `MetricFamilyToText` wrote them.
fn assert_golden(registry: &Registry, pinned: &BTreeSet<String>) {
    let families: Vec<Gathered> = registry
        .gather()
        .into_iter()
        .filter(|family| pinned.contains(&family.name))
        .collect();
    let mut got = String::new();
    text::encode(&mut got, &families);
    let want = std::fs::read_to_string(golden_path()).or_panic();
    assert!(
        got == want,
        "metrics differ from {GOLDEN_FILE}:\n got:\n{got}\nwant:\n{want}"
    );
}

/// Go's `assertCoverage`: every registered `swarm_` family is excluded on purpose or has at least
/// one series in the golden file, and no golden or exclusion entry names a family that is no
/// longer registered.
fn assert_coverage(registered: &BTreeSet<String>) {
    let golden = std::fs::read_to_string(golden_path()).or_panic();
    let mut series_by_family: HashMap<&str, usize> = HashMap::new();
    let mut golden_families = BTreeSet::new();
    for line in golden.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            golden_families.insert(rest.split_whitespace().next().unwrap_or_default());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split('{').next().unwrap_or_default();
        let name = name.split(' ').next().unwrap_or_default();
        *series_by_family.entry(name).or_default() += 1;
    }
    for family in pinned_families(registered) {
        assert!(
            series_by_family
                .get(family.as_str())
                .copied()
                .unwrap_or_default()
                > 0,
            "coverage: registered family {family} has no series in {GOLDEN_FILE}; extend the \
             fixtures"
        );
    }
    for family in golden_families {
        assert!(
            registered.contains(family),
            "coverage: {GOLDEN_FILE} pins {family}, which is no longer registered"
        );
    }
    for (family, _) in EXCLUDED_FAMILIES {
        assert!(
            registered.contains(family),
            "coverage: exclusion names {family}, which is no longer registered"
        );
    }
}

/// Go's `testEngineWire`: the collectors driven against the fake engine as `run` drives them,
/// one phase at a time, pinning each phase's requests; the metrics computed from the answers
/// against the golden file; and the wire properties of the whole run.
async fn pins_requests_and_metrics(set_index: usize) {
    let engine = FakeEngine::start(FIXTURE_SETS[set_index], HashMap::new()).await;
    let client = Arc::new(engine.client());
    let registry = Registry::new();
    let metrics = configure_collectors_like_main(&registry);
    let cancel = CancellationToken::new();

    // Phase 1: the reconciler's first resync. Sequential: services, then nodes.
    engine.reset();
    let reconciler = Reconciler::new(
        Arc::clone(&client),
        metrics.collectors.clone(),
        Arc::new(SystemClock),
        quiet_logger(),
    );
    let handle = reconciler.handle();
    // The event-stream anchor, captured before the resync lists anything, as run does.
    let anchor = SystemClock.now();
    let reconciler_task = tokio::spawn(reconciler.run(cancel.clone()));
    handle.ready().await;
    assert_sequence(
        "first resync",
        &engine.phase_requests(),
        &["GET /services", "GET /nodes"],
    );

    polls(&engine, &client, &handle, &metrics, &cancel).await;
    events_happy_path(&engine, &client, &handle, &registry, &metrics, anchor).await;
    events_reconnect(&engine, &client, &handle, &metrics, anchor).await;
    assert_wire_properties(&engine, &engine.all_requests());

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), reconciler_task)
        .await
        .or_panic()
        .or_panic();
}

/// Phases 2 and 3: the task poll against the reconciler's polling snapshot, one unfiltered list
/// (the task of svc-gone, a service the snapshot does not hold, is skipped without an inspect);
/// then the container poll, the list and one inspect per running or exited container, in list
/// order (the paused one is not inspected).
async fn polls(
    engine: &FakeEngine,
    client: &Arc<Client>,
    handle: &ReconcilerHandle,
    metrics: &Metrics,
    cancel: &CancellationToken,
) {
    engine.reset();
    poll_and_publish_replicas_state(&**client, handle, cancel)
        .await
        .or_panic();
    assert_sequence("task poll", &engine.phase_requests(), &["GET /tasks"]);

    engine.reset();
    let containers = metrics
        .containers
        .as_ref()
        .unwrap_or_else(|| panic!("container metrics are enabled"));
    let rows = containers.poll(&**client).await.or_panic();
    containers.update(&rows);
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

/// Phase 4, events, happy path: one connection streams a service and a node update, then stays
/// open. The reconciler inspects the updated service and refreshes the nodes, recomputing every
/// service from the cached placement; when the two events land in the same cycle is timing, so
/// their requests are compared as a multiset, and only "/events first" is an ordering the code
/// guarantees. Then the metrics of every family the Docker answers feed, against the golden
/// file.
async fn events_happy_path(
    engine: &FakeEngine,
    client: &Arc<Client>,
    handle: &ReconcilerHandle,
    registry: &Registry,
    metrics: &Metrics,
    anchor: Timestamp,
) {
    let expectations = expectations();
    engine.reset();
    engine.script_events(vec![fixture_connection(
        engine,
        "events-happy.jsonl",
        StreamEnd::Hold,
    )]);
    let reconnects = metrics.collectors.metrics.events_reconnects().get();
    let want = [
        "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
        service_inspect_request(expectations.service_inspect_query, "svc-api"),
        "GET /nodes".to_owned(),
    ];
    let listener = Listener::start(client, handle, anchor);
    wait_for(
        "every request the two events trigger, and web api desired replicas = 5",
        || {
            engine.phase_requests().len() >= want.len()
                && desired_replicas(registry, "web", "api") == Some(5.0)
        },
    )
    .await;
    // Nothing signals "the stream will not reconnect", so a reconnect gets one full initial
    // backoff (plus margin) to show up.
    assert_no_second_events_connection(engine, BACKOFF_INITIAL_DELAY + RECONNECT_MARGIN).await;
    listener.stop().await;

    let event_requests = engine.phase_requests();
    assert_multiset("events", &event_requests, &want);
    assert_eq!(
        event_requests[0].path, "/events",
        "the stream is opened first"
    );
    let connections = requests_to(&event_requests, "/events");
    assert_eq!(connections.len(), 1, "happy path: /events requests");
    assert_eq!(
        events_since(connections[0]),
        anchor.checked_sub(EVENTS_SINCE_MARGIN).or_panic(),
        "the anchor minus the margin, to the ns"
    );
    let reconnects_after = metrics.collectors.metrics.events_reconnects().get();
    assert!(
        (reconnects_after - reconnects).abs() < f64::EPSILON,
        "happy path: events_reconnects_total = {reconnects_after}, want {reconnects}"
    );

    // The computed output of every Docker-fed family, after the resync, both polls and the
    // events.
    let registered = registry.described_names();
    assert_golden(registry, &pinned_families(&registered));
    assert_coverage(&registered);
}

/// Phase 5, events, reconnect: the first connection streams one event and ends cleanly; the
/// listener counts a reconnect, resumes the margin before that event on a second connection,
/// which stays open, and requests a resync, which lists services and nodes.
async fn events_reconnect(
    engine: &FakeEngine,
    client: &Arc<Client>,
    handle: &ReconcilerHandle,
    metrics: &Metrics,
    anchor: Timestamp,
) {
    let expectations = expectations();
    engine.reset();
    engine.script_events(vec![
        fixture_connection(engine, "events-reconnect.jsonl", StreamEnd::Close),
        EventsConnection {
            chunks: Vec::new(),
            end: StreamEnd::Hold,
        },
    ]);
    let reconnects = metrics.collectors.metrics.events_reconnects().get();
    let want = [
        "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
        service_inspect_request(expectations.service_inspect_query, "svc-db"),
        "GET /events?filters={type:[node,service]}&since=<time>".to_owned(),
        "GET /services".to_owned(),
        "GET /nodes".to_owned(),
    ];
    let listener = Listener::start(client, handle, anchor);
    wait_for(
        "the event's inspect, a second /events connection and the resync",
        || engine.phase_requests().len() >= want.len(),
    )
    .await;
    listener.stop().await;

    let reconnect_requests = engine.phase_requests();
    assert_multiset("events reconnect", &reconnect_requests, &want);
    let connections = requests_to(&reconnect_requests, "/events");
    assert_eq!(connections.len(), 2, "reconnect: /events requests");
    let last_event = Timestamp::from_nanosecond(RECONNECT_EVENT_TIME_NANO).or_panic();
    let resume = last_event.checked_sub(EVENTS_SINCE_MARGIN).or_panic();
    assert_eq!(
        connections[1].query_value("since"),
        Some("1789902299.700000000")
    );
    assert_eq!(events_since(connections[1]), resume);
    let reconnects_after = metrics.collectors.metrics.events_reconnects().get();
    assert!(
        (reconnects_after - reconnects - 1.0).abs() < f64::EPSILON,
        "reconnect: events_reconnects_total = {reconnects_after}, want {}",
        reconnects + 1.0
    );
}

/// Go: `TestEngineWire_PinsRequestsAndMetrics`, for API 1.51.
#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_and_metrics_api_1_51() {
    tokio::time::timeout(TEST_TIMEOUT, Box::pin(pins_requests_and_metrics(0)))
        .await
        .or_panic();
}

/// Go: `TestEngineWire_PinsRequestsAndMetrics`, for API 1.56.
#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_and_metrics_api_1_56() {
    tokio::time::timeout(TEST_TIMEOUT, Box::pin(pins_requests_and_metrics(1)))
        .await
        .or_panic();
}

/// Socket proxies such as `HAProxy` reject a request line past their buffer (16 KB by default);
/// 8 KB leaves room for the headers that share it.
const MAX_TASK_LIST_REQUEST_URI: usize = 8 << 10;

/// Go: `TestEngineWire_TaskListHasNoFilter`. Go's test fills the service cache with 10001
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
