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

//! What the collectors do with the Docker answers, end to end, against `docker-client`'s fake
//! engine serving the engine-wire captures of API 1.51 and 1.56: the requests they make, in the
//! order the exporter makes them, and the metrics they compute from the answers, against the
//! golden file the Go exporter's test pinned (the port of the Go engine-wire test).
//!
//! The bytes of the requests are not pinned any more: they are `docker-client`'s and bollard's,
//! tested there. What is checked is the calls (which paths, in which order, with which `since`
//! and filters) and the values that come out.

mod support;

#[path = "../src/test_support/openmetrics.rs"]
#[expect(
    dead_code,
    reason = "the reader is shared with the unit tests and tests/metrics.rs, which use more of it"
)]
mod openmetrics;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use clock::{Clock as _, SystemClock};
use docker_client::Config;
use docker_client::test_util::{FakeEngine, Reply};
use prometheus_client::registry::Registry;
use swarm_scheduler_exporter::collector::containers::ContainersConfig;
use swarm_scheduler_exporter::collector::reconciler::{
    ListenerStopped, Reconciler, ReconcilerHandle, listen_swarm_events,
    poll_and_publish_replicas_state,
};
use swarm_scheduler_exporter::collector::types::CustomLabels;
use swarm_scheduler_exporter::docker::{Client, since_parameter};
use swarm_scheduler_exporter::labels::validate_and_sanitize_label_names;
use swarm_scheduler_exporter::run::{Metrics, MetricsOptions, register_metrics};

use support::OrPanic as _;

/// Bounds a whole test, so a hung stream fails it instead of hanging it.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The Go listener's `eventsSinceMargin`: every event-stream anchor starts this much earlier.
const EVENTS_SINCE_MARGIN: SignedDuration = SignedDuration::from_millis(500);

/// The `since` the reconnect fixture's event leads to: its `timeNano` is 1789902300.2 s, so the
/// resume point crosses a second boundary (…299.7), which only nanosecond resolution keeps.
const RECONNECT_SINCE: &str = "1789902299.700000000";

/// The fixture sets: the same cluster in two API versions' shapes.
const FIXTURE_SETS: [&str; 2] = ["api-1.51", "api-1.56"];

fn fixture_path(set: &str, name: &str) -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests/testdata/engine_wire",
        set,
        name,
    ]
    .iter()
    .collect()
}

fn fixture(set: &str, name: &str) -> String {
    std::fs::read_to_string(fixture_path(set, name)).or_panic()
}

/// The engine serving `set`: every list and inspect the collectors make, and `/events` streaming
/// the lines of `events` then closing.
fn engine(set: &str, events: &str) -> FakeEngine {
    let events = fixture(set, events);
    let lines: Vec<&str> = events.lines().filter(|line| !line.is_empty()).collect();
    engine_answering_events(set, Reply::events(&lines))
}

/// The engine of [`engine`] with `/events` answering `events`.
fn engine_answering_events(set: &str, events: Reply) -> FakeEngine {
    let mut routes: Vec<(String, Reply)> = vec![
        (
            "/services".into(),
            Reply::ok(&fixture(set, "services.json")),
        ),
        ("/nodes".into(), Reply::ok(&fixture(set, "nodes.json"))),
        ("/tasks".into(), Reply::ok(&fixture(set, "tasks.json"))),
        (
            "/containers/json".into(),
            Reply::ok(&fixture(set, "containers.json")),
        ),
    ];
    for service in ["svc-agent", "svc-api", "svc-db"] {
        routes.push((
            format!("/services/{service}"),
            Reply::ok(&fixture(set, &format!("service-{service}.json"))),
        ));
    }
    for container in [
        "c-compose-web",
        "c-compose-worker",
        "c-plain",
        "c-swarm-api",
    ] {
        routes.push((
            format!("/containers/{container}/json"),
            Reply::ok(&fixture(set, &format!("container-{container}.json"))),
        ));
    }
    routes.push(("/events".into(), events));
    let routes: Vec<(&str, Reply)> = routes
        .iter()
        .map(|(path, reply)| (path.as_str(), reply.clone()))
        .collect();
    FakeEngine::start(&routes).or_panic()
}

fn client(engine: &FakeEngine) -> Arc<Client> {
    let config = Config::unix(engine.socket().to_string_lossy())
        .api_version("1.51")
        .or_panic();
    Arc::new(Client::new(config).or_panic())
}

/// The requests so far as `METHOD target`, oldest first.
fn wire(engine: &FakeEngine) -> Vec<String> {
    engine
        .requests()
        .iter()
        .map(|request| format!("{} {}", request.method, request.target))
        .collect()
}

/// How many requests went to `path`.
fn count(engine: &FakeEngine, path: &str) -> usize {
    engine
        .requests()
        .iter()
        .filter(|request| request.path() == path)
        .count()
}

/// The value of the query parameter `name` of the `index`-th request to `path`, as sent.
fn query_of(engine: &FakeEngine, path: &str, index: usize, name: &str) -> Option<String> {
    let requests = engine.requests();
    let request = requests
        .iter()
        .filter(|request| request.path() == path)
        .nth(index)?;
    request
        .query()
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

/// The custom label exported through `--label`, so the pin covers the custom-label path of every
/// per-service family: Go's `engineWireCustomLabel`.
const CUSTOM_LABEL: &str = "com.example.team";

/// The golden metrics, captured from the Go exporter against the same fixtures.
const GOLDEN_FILE: &str = "tests/testdata/engine_wire/metrics.golden";

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

/// Go's `configureCollectorsLikeMain`: every family registered as `run` registers it with
/// `--containers --containers-include-swarm` and `--label com.example.team`, on a private
/// registry.
fn configure_collectors_like_main(registry: &mut Registry) -> Metrics {
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

/// The registry's exposition, parsed.
fn scrape(registry: &Registry) -> openmetrics::Exposition {
    openmetrics::parse(&swarm_scheduler_exporter::metrics::render(registry))
}

/// The value of the `swarm_service_desired_replicas` series of `stack` and `service`.
fn desired_replicas(registry: &Registry, stack: &str, service: &str) -> Option<f64> {
    scrape(registry)
        .samples
        .iter()
        .find(|sample| {
            let label = |name: &str| {
                sample
                    .labels
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.as_str())
            };
            sample.name == "swarm_service_desired_replicas"
                && label("stack") == Some(stack)
                && label("service") == Some(service)
        })
        .map(|sample| sample.value)
}

/// The registered families, by the names the Go exporter gave them: a counter's carries its
/// `_total`, which OpenMetrics leaves off the family.
fn registered_families(registry: &Registry) -> BTreeSet<String> {
    scrape(registry)
        .types
        .into_iter()
        .map(|(family, kind)| {
            if kind == "counter" {
                format!("{family}_total")
            } else {
                family
            }
        })
        .collect()
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

/// Go's `GatherAndCompare` of the pinned families against the golden file: every series with its
/// value, and each family's type. The golden file is Go's text format; the values and the series
/// are the same in the OpenMetrics text, which is what is compared.
fn assert_golden(registry: &Registry, pinned: &BTreeSet<String>) {
    let got = scrape(registry);
    let want = openmetrics::parse(&std::fs::read_to_string(golden_path()).or_panic());
    let in_pinned = |name: &String| pinned.contains(name);
    let got_series: std::collections::BTreeMap<String, f64> = got
        .series()
        .into_iter()
        .filter(|(series, _)| in_pinned(&series.split('{').next().unwrap_or_default().to_owned()))
        .collect();
    assert!(
        got_series == want.series(),
        "metrics differ from {GOLDEN_FILE}:\n got:\n{got_series:#?}\nwant:\n{:#?}",
        want.series()
    );
    for family in pinned {
        assert_eq!(
            got.types.get(family),
            want.types.get(family),
            "the type of {family}"
        );
    }
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

/// The collectors driven against the fake engine as `run` drives them, one phase at a time,
/// pinning each phase's calls; then the metrics computed from the answers against the golden
/// file.
async fn pins_requests_and_metrics(set: &str) {
    let engine = engine(set, "events-happy.jsonl");
    let client = client(&engine);
    let mut registry = Registry::default();
    let metrics = configure_collectors_like_main(&mut registry);
    let cancel = CancellationToken::new();

    // Phase 1: the reconciler's first resync. Sequential: services, then nodes.
    let reconciler = Reconciler::new(
        Arc::clone(&client),
        metrics.collectors.clone(),
        Arc::new(SystemClock),
    );
    let handle = reconciler.handle();
    // The event-stream anchor, captured before the resync lists anything, as run does.
    let anchor = SystemClock.now();
    let reconciler_task = tokio::spawn(reconciler.run(cancel.clone()));
    handle.ready().await;
    assert_eq!(
        wire(&engine),
        ["GET /v1.51/services", "GET /v1.51/nodes"],
        "first resync"
    );

    // Phase 2: the task poll against the polling snapshot, one unfiltered list (the task of
    // svc-gone, a service the snapshot does not hold, is skipped without an inspect).
    poll_and_publish_replicas_state(&*client, &handle, &cancel)
        .await
        .or_panic();
    assert_eq!(&wire(&engine)[2..], ["GET /v1.51/tasks"], "task poll");

    // Phase 3: the container poll, the list and one inspect per running or exited container, in
    // list order (the paused one is not inspected).
    let containers = metrics
        .containers
        .as_ref()
        .unwrap_or_else(|| panic!("container metrics are enabled"));
    let rows = containers.poll(&*client).await.or_panic();
    containers.update(&rows);
    assert_eq!(
        &wire(&engine)[3..],
        [
            "GET /v1.51/containers/json?all=true&size=false",
            "GET /v1.51/containers/c-compose-web/json",
            "GET /v1.51/containers/c-compose-worker/json",
            "GET /v1.51/containers/c-plain/json",
            "GET /v1.51/containers/c-swarm-api/json",
        ],
        "container poll"
    );

    // Phase 4: the events. One connection streams a service and a node update and closes; the
    // reconciler inspects the updated service and refreshes the nodes, recomputing every service
    // from the cached placement. The listener is stopped as soon as the update shows, well
    // inside its first backoff, so the reconnect's resync does not overwrite it.
    let listener = Listener::start(&client, &handle, anchor);
    wait_for("the events' effects", || {
        count(&engine, "/services/svc-api") == 1
            && count(&engine, "/nodes") == 2
            && desired_replicas(&registry, "web", "api") == Some(5.0)
    })
    .await;
    listener.stop().await;
    assert_eq!(count(&engine, "/events"), 1, "one connection");
    assert_eq!(
        query_of(&engine, "/events", 0, "since"),
        Some(since_parameter(
            anchor.checked_sub(EVENTS_SINCE_MARGIN).or_panic()
        )),
        "the anchor minus the margin, to the ns"
    );
    let filters = query_of(&engine, "/events", 0, "filters").unwrap_or_default();
    assert_eq!(
        filters, "%7B%22type%22%3A%5B%22node%22%2C%22service%22%5D%7D",
        "the swarm types only"
    );

    // The computed output of every Docker-fed family, after the resync, both polls and the
    // events.
    let registered = registered_families(&registry);
    assert_golden(&registry, &pinned_families(&registered));
    assert_coverage(&registered);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), reconciler_task)
        .await
        .or_panic()
        .or_panic();
}

/// A stream the engine closes is reconnected from the margin before its last event, which a
/// resync follows: the connection count, the resume point and the reconnect counter.
async fn reconnects_from_the_last_event(set: &str) {
    let engine = engine(set, "events-reconnect.jsonl");
    let client = client(&engine);
    let mut registry = Registry::default();
    let metrics = configure_collectors_like_main(&mut registry);
    let cancel = CancellationToken::new();
    let reconciler = Reconciler::new(
        Arc::clone(&client),
        metrics.collectors.clone(),
        Arc::new(SystemClock),
    );
    let handle = reconciler.handle();
    let anchor = SystemClock.now();
    let reconciler_task = tokio::spawn(reconciler.run(cancel.clone()));
    handle.ready().await;

    let listener = Listener::start(&client, &handle, anchor);
    wait_for("a second connection and its resync", || {
        count(&engine, "/events") >= 2 && count(&engine, "/services") >= 2
    })
    .await;
    listener.stop().await;

    assert_eq!(
        query_of(&engine, "/events", 0, "since"),
        Some(since_parameter(
            anchor.checked_sub(EVENTS_SINCE_MARGIN).or_panic()
        ))
    );
    assert_eq!(
        query_of(&engine, "/events", 1, "since").as_deref(),
        Some(RECONNECT_SINCE),
        "the margin before the event the first stream carried"
    );
    assert!(
        count(&engine, "/services/svc-db") >= 1,
        "the event's inspect"
    );
    assert!(metrics.collectors.metrics.events_reconnects() >= 1);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), reconciler_task)
        .await
        .or_panic()
        .or_panic();
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_and_metrics_api_1_51() {
    tokio::time::timeout(TEST_TIMEOUT, pins_requests_and_metrics(FIXTURE_SETS[0]))
        .await
        .or_panic();
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_wire_pins_requests_and_metrics_api_1_56() {
    tokio::time::timeout(TEST_TIMEOUT, pins_requests_and_metrics(FIXTURE_SETS[1]))
        .await
        .or_panic();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_event_stream_reconnects_from_the_last_event() {
    tokio::time::timeout(
        TEST_TIMEOUT,
        reconnects_from_the_last_event(FIXTURE_SETS[1]),
    )
    .await
    .or_panic();
}

/// What a listener against an engine with a scripted `/events` leaves behind, once `settle` held.
struct EventsRun {
    engine: FakeEngine,
    metrics: Metrics,
}

/// A reconciler and a listener (events request bounded by `events_timeout`) against an engine
/// whose `/events` answers `events`, until `settle` holds on what they did; then both are stopped.
async fn run_listener_against(
    events: Reply,
    events_timeout: Duration,
    settle: impl FnMut(&EventsRun) -> bool,
) -> EventsRun {
    let engine = engine_answering_events(FIXTURE_SETS[1], events);
    let config = Config::unix(engine.socket().to_string_lossy())
        .api_version("1.51")
        .or_panic();
    let client = Arc::new(
        Client::new(config)
            .or_panic()
            .with_events_timeout(events_timeout),
    );
    let mut registry = Registry::default();
    let metrics = configure_collectors_like_main(&mut registry);
    let cancel = CancellationToken::new();
    let reconciler = Reconciler::new(
        Arc::clone(&client),
        metrics.collectors.clone(),
        Arc::new(SystemClock),
    );
    let handle = reconciler.handle();
    let reconciler_task = tokio::spawn(reconciler.run(cancel.clone()));
    handle.ready().await;
    let listener = Listener::start(&client, &handle, SystemClock.now());
    let run = EventsRun { engine, metrics };
    let mut settle = settle;
    wait_for("the listener to settle", || settle(&run)).await;
    listener.stop().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), reconciler_task)
        .await
        .or_panic()
        .or_panic();
    run
}

impl EventsRun {
    fn connected_at(&self) -> i64 {
        self.metrics
            .collectors
            .metrics
            .timestamps()
            .last_events_connect()
    }

    fn reconnects(&self) -> u64 {
        self.metrics.collectors.metrics.events_reconnects()
    }
}

/// An engine that never answers the events request's headers (a proxy that holds it) is not a
/// connection: the listener never reports connected, requests no resync, and retries once the
/// request timed out.
#[tokio::test(flavor = "multi_thread")]
async fn an_events_request_whose_headers_never_arrive_is_not_connected() {
    let run = tokio::time::timeout(
        TEST_TIMEOUT,
        run_listener_against(Reply::Silent, Duration::from_millis(200), |run| {
            count(&run.engine, "/events") >= 2
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out"));
    assert_eq!(run.connected_at(), 0, "never connected");
    assert!(run.reconnects() >= 1, "the failed connect is retried");
    assert_eq!(
        count(&run.engine, "/services"),
        1,
        "no resync was requested"
    );
}

/// An engine that answers the headers and then says nothing for a long while is connected and
/// stays so: the listener does not reconnect (no second request, no reconnect counted), and an
/// event that finally arrives is dispatched, not dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_events_stream_stays_connected_and_delivers_a_late_event() {
    let event = format!(
        "{}\n",
        fixture(FIXTURE_SETS[1], "events-reconnect.jsonl").trim()
    );
    let run = tokio::time::timeout(
        TEST_TIMEOUT,
        run_listener_against(
            Reply::HeadersThenBody {
                body_delay: Duration::from_millis(1500),
                body: event,
            },
            // Far shorter than the silence: the timeout bounds the headers, not the stream.
            Duration::from_millis(300),
            |run| count(&run.engine, "/services/svc-db") >= 1,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out"));
    assert_ne!(run.connected_at(), 0, "connected once the headers came");
    assert_eq!(
        count(&run.engine, "/events"),
        1,
        "no reconnect during the silence"
    );
}

/// A refused events request (a proxy answering 403, an engine answering 404) is a failed
/// connect: not connected, no resync, retried.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_events_request_is_not_connected() {
    for status in [403, 404] {
        let run = tokio::time::timeout(
            TEST_TIMEOUT,
            run_listener_against(
                Reply::error(status, "events are not allowed"),
                Duration::from_secs(5),
                |run| count(&run.engine, "/events") >= 2,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("timed out"));
        assert_eq!(run.connected_at(), 0, "{status}: never connected");
        assert!(run.reconnects() >= 1, "{status}");
        assert_eq!(count(&run.engine, "/services"), 1, "{status}: no resync");
    }
}
