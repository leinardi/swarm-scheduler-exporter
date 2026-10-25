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

//! The Go reconciler tests: of its 35 (the plan counted 36), the 3 on the event listener are
//! ported with it in plan phase 15, and 2 health and 2 exporter-metrics ones live with those
//! modules, which leaves the 28 here.

use std::collections::BTreeMap;
use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};

use hyper::StatusCode;
use jiff::Timestamp;
use tokio::task::JoinHandle;

use super::*;
use crate::clock::FakeClock;
use crate::collector::desired::labels_for_metadata;
use crate::collector::health::Timestamps;
use crate::collector::metrics_ids::{
    RESYNC_RESULT_FAILURE, RESYNC_RESULT_SUCCESS, SERVICE_MODE_GLOBAL, SERVICE_MODE_GLOBAL_JOB,
    SERVICE_MODE_REPLICATED, SERVICE_MODE_REPLICATED_JOB,
};
use crate::collector::poll_snapshot::{PollService, PollSnapshot};
use crate::collector::replicas_state::poll_replicas_state;
use crate::collector::types::{STACK_NAMESPACE_LABEL, display_name};
use crate::docker::types::{
    ContainerInspect, ContainerSummary, JobStatus, Present, ReplicatedJob, ReplicatedService,
    ServiceMode, ServiceSpec, Task, Version,
};
use crate::docker::{DOCKER_REQUEST_TIMEOUT, Error, Filters};
use crate::logger::{Level, MemorySink};
use crate::test_support::{FakeEvents, capture_logs, eventually, schedulable_node};

/// A Docker call the test docker can hold forever, so only the caller's deadline or
/// cancellation ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Call {
    ServiceList,
    NodeList,
    ServiceInspect,
    TaskList,
}

type Hook = Box<dyn FnOnce() + Send>;

/// The state of [`TestDocker`] a test changes while the reconciler runs.
#[derive(Default)]
struct DockerState {
    services: Vec<Service>,
    nodes: Vec<Node>,
    tasks: Vec<Task>,
    service_list_err: Option<Error>,
    node_list_err: Option<Error>,
    inspect_err: HashMap<String, Error>,
    // Runs after the service list built its result, before it returns.
    after_service_list: Option<Hook>,
    blocked: HashSet<Call>,
    panic_on_inspect: bool,
    panic_on_service_list: bool,
    inspect_calls: HashMap<String, usize>,
}

/// Go's `reconcilerDocker`: a service in `services` is also what an inspect returns for its ID;
/// any other ID is not found unless `inspect_err` names it. It also plays Go's blocking and
/// panicking wrappers.
#[derive(Default)]
struct TestDocker {
    state: Mutex<DockerState>,
    service_list_calls: AtomicUsize,
    node_list_calls: AtomicUsize,
    // Signalled on every service list, as Go's blockingListDocker reports each call.
    service_list_called: Notify,
}

impl TestDocker {
    fn new(services: Vec<Service>, nodes: Vec<Node>) -> Self {
        let docker = Self::default();
        {
            let mut state = docker.lock();
            state.services = services;
            state.nodes = nodes;
        }
        docker
    }

    fn lock(&self) -> MutexGuard<'_, DockerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_services(&self, services: Vec<Service>) {
        self.lock().services = services;
    }

    fn set_nodes(&self, nodes: Vec<Node>) {
        self.lock().nodes = nodes;
    }

    fn set_list_errors(&self, service_list: Option<Error>, node_list: Option<Error>) {
        let mut state = self.lock();
        state.service_list_err = service_list;
        state.node_list_err = node_list;
    }

    fn set_inspect_err(&self, service_id: &str, error: Option<Error>) {
        let mut state = self.lock();
        match error {
            Some(error) => state.inspect_err.insert(service_id.to_owned(), error),
            None => state.inspect_err.remove(service_id),
        };
    }

    fn set_blocked(&self, call: Call, blocked: bool) {
        let mut state = self.lock();
        if blocked {
            state.blocked.insert(call);
        } else {
            state.blocked.remove(&call);
        }
    }

    fn service_list_count(&self) -> usize {
        self.service_list_calls.load(Ordering::SeqCst)
    }

    fn inspect_count(&self, service_id: &str) -> usize {
        self.lock()
            .inspect_calls
            .get(service_id)
            .copied()
            .unwrap_or(0)
    }

    fn blocked(&self, call: Call) -> bool {
        self.lock().blocked.contains(&call)
    }
}

async fn hold_if(blocked: bool) {
    if blocked {
        pending::<()>().await;
    }
}

impl DockerApi for TestDocker {
    type Events = FakeEvents;

    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send {
        self.service_list_calls.fetch_add(1, Ordering::SeqCst);
        self.service_list_called.notify_one();
        let (result, hook, blocked, panics) = {
            let mut state = self.lock();
            let result = state
                .service_list_err
                .clone()
                .map_or_else(|| Ok(state.services.clone()), Err);
            (
                result,
                state.after_service_list.take(),
                state.blocked.contains(&Call::ServiceList),
                state.panic_on_service_list,
            )
        };
        async move {
            assert!(!panics, "unexpected service list data");
            hold_if(blocked).await;
            if result.is_ok()
                && let Some(hook) = hook
            {
                hook();
            }
            result
        }
    }

    fn node_list(&self) -> impl Future<Output = Result<Vec<Node>, Error>> + Send {
        self.node_list_calls.fetch_add(1, Ordering::SeqCst);
        let state = self.lock();
        let result = state
            .node_list_err
            .clone()
            .map_or_else(|| Ok(state.nodes.clone()), Err);
        let blocked = state.blocked.contains(&Call::NodeList);
        drop(state);
        async move {
            hold_if(blocked).await;
            result
        }
    }

    fn service_inspect(&self, id: &str) -> impl Future<Output = Result<Service, Error>> + Send {
        let mut state = self.lock();
        *state.inspect_calls.entry(id.to_owned()).or_insert(0) += 1;
        let result = state.inspect_err.get(id).cloned().map_or_else(
            || {
                state
                    .services
                    .iter()
                    .find(|service| service.id == id)
                    .cloned()
                    .ok_or_else(|| {
                        Error::response(StatusCode::NOT_FOUND, format!("service {id} not found"))
                    })
            },
            Err,
        );
        let (blocked, panics) = (
            state.blocked.contains(&Call::ServiceInspect),
            state.panic_on_inspect,
        );
        drop(state);
        async move {
            assert!(!panics, "unexpected service data");
            hold_if(blocked).await;
            result
        }
    }

    fn task_list(&self) -> impl Future<Output = Result<Vec<Task>, Error>> + Send {
        let state = self.lock();
        let tasks = state.tasks.clone();
        let blocked = state.blocked.contains(&Call::TaskList);
        drop(state);
        async move {
            hold_if(blocked).await;
            Ok(tasks)
        }
    }

    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send {
        std::future::ready(Ok(Vec::new()))
    }

    fn container_inspect(
        &self,
        _id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send {
        std::future::ready(Ok(ContainerInspect::default()))
    }

    fn events(
        &self,
        _since: Timestamp,
        _filters: &Filters,
    ) -> impl Future<Output = Result<FakeEvents, Error>> + Send {
        std::future::ready(Ok(FakeEvents { events: None }))
    }
}

fn unavailable() -> Error {
    Error::connection_failed("list unavailable".to_owned())
}

fn service(id: &str, stack: &str, name: &str, mode: ServiceMode) -> Service {
    Service {
        id: id.to_owned(),
        spec: ServiceSpec {
            name: format!("{stack}_{name}"),
            labels: BTreeMap::from([(STACK_NAMESPACE_LABEL.to_owned(), stack.to_owned())]),
            mode,
            ..ServiceSpec::default()
        },
        ..Service::default()
    }
}

/// Go's `makeReplicatedService`.
fn replicated_service(id: &str, stack: &str, name: &str, replicas: u64) -> Service {
    service(
        id,
        stack,
        name,
        ServiceMode {
            replicated: Some(ReplicatedService {
                replicas: Some(replicas),
            }),
            ..ServiceMode::default()
        },
    )
}

/// Go's `makeGlobalService`.
fn global_service(id: &str, stack: &str, name: &str) -> Service {
    service(
        id,
        stack,
        name,
        ServiceMode {
            global: Some(Present {}),
            ..ServiceMode::default()
        },
    )
}

fn with_first_iteration(mut service: Service) -> Service {
    service.job_status = Some(JobStatus {
        job_iteration: Version { index: 1 },
    });
    service
}

/// Go's `makeReplicatedJobService`.
fn replicated_job_service(id: &str, stack: &str, name: &str, completions: u64) -> Service {
    with_first_iteration(service(
        id,
        stack,
        name,
        ServiceMode {
            replicated_job: Some(ReplicatedJob {
                total_completions: Some(completions),
                ..ReplicatedJob::default()
            }),
            ..ServiceMode::default()
        },
    ))
}

/// Go's `makeGlobalJobService`.
fn global_job_service(id: &str, stack: &str, name: &str) -> Service {
    with_first_iteration(service(
        id,
        stack,
        name,
        ServiceMode {
            global_job: Some(Present {}),
            ..ServiceMode::default()
        },
    ))
}

fn nodes(count: usize) -> Vec<Node> {
    (1..=count)
        .map(|index| schedulable_node(&format!("n{index}"), &format!("h{index}")))
        .collect()
}

fn service_event(service_id: &str, action: &str) -> Event {
    Event {
        kind: EVENT_TYPE_SERVICE.to_owned(),
        action: action.to_owned(),
        actor: crate::docker::types::Actor {
            id: service_id.to_owned(),
        },
        ..Event::default()
    }
}

fn node_event(node_id: &str) -> Event {
    Event {
        kind: EVENT_TYPE_NODE.to_owned(),
        ..service_event(node_id, "update")
    }
}

/// Go's `serviceLabels`.
fn service_labels(stack: &str, service: &str, mode: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("stack".to_owned(), stack.to_owned()),
        ("service".to_owned(), service.to_owned()),
        ("service_mode".to_owned(), mode.to_owned()),
        ("display_name".to_owned(), display_name(stack, service)),
    ])
}

/// A reconciler on a [`TestDocker`] with fresh, unregistered families, Go's short test delays,
/// a fixed clock and a log capture.
struct Fixture {
    reconciler: Reconciler<TestDocker>,
    docker: Arc<TestDocker>,
    handle: ReconcilerHandle,
    collectors: Collectors,
    clock: Arc<FakeClock>,
    sink: MemorySink,
}

/// The test policy of Go's `newTestReconciler`.
fn test_policy() -> ReconcilerPolicy {
    ReconcilerPolicy {
        retry_delays: vec![Duration::from_millis(10); 3],
        backoff_initial: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
        ..ReconcilerPolicy::default()
    }
}

/// Go's `newTestReconciler`.
fn fixture(docker: TestDocker) -> Fixture {
    fixture_with(docker, &test_policy())
}

fn fixture_with(docker: TestDocker, policy: &ReconcilerPolicy) -> Fixture {
    let (logger, sink) = capture_logs(Level::INFO);
    let custom = CustomLabels::default();
    let collectors = Collectors {
        custom_labels: custom.clone(),
        replica_gauges: Arc::new(ReplicaGauges::new(&custom)),
        service_update: Arc::new(ServiceUpdateMetrics::new(&custom, logger.clone())),
        nodes_by_state: Arc::new(NodesByState::new(logger.clone())),
        replicas_state: Arc::new(ReplicasState::new(&custom, logger.clone())),
        metrics: Arc::new(ExporterMetrics::new(Arc::new(Timestamps::default()))),
    };
    let clock = Arc::new(FakeClock::new(
        Timestamp::from_second(1_767_225_600).unwrap(),
    ));
    let docker = Arc::new(docker);
    let reconciler = Reconciler::with_policy(
        Arc::clone(&docker),
        collectors.clone(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        logger,
        policy,
    );
    Fixture {
        handle: reconciler.handle(),
        reconciler,
        docker,
        collectors,
        clock,
        sink,
    }
}

impl Fixture {
    /// Go's `completeFirstResync`.
    async fn complete_first_resync(&mut self) {
        self.reconciler.resync(&CancellationToken::new()).await;
        assert!(
            self.handle.resync_state().1 > 0,
            "first resync did not complete"
        );
    }

    async fn cycle(&mut self) {
        self.reconciler.cycle(&CancellationToken::new()).await;
    }

    fn desired_series(&self) -> usize {
        let registry = crate::metrics::Registry::new();
        self.collectors.replica_gauges.register(&registry).unwrap();
        registry
            .gather()
            .iter()
            .filter(|family| family.name == "swarm_service_desired_replicas")
            .map(|family| family.series.len())
            .sum()
    }

    fn desired(&self, labels: &BTreeMap<String, String>) -> Option<f64> {
        self.collectors.replica_gauges.desired().get_with(labels)
    }

    fn schedulable(&self, labels: &BTreeMap<String, String>) -> Option<f64> {
        self.collectors
            .replica_gauges
            .schedulable()
            .get_with(labels)
    }

    fn resyncs(&self) -> (Option<f64>, Option<f64>) {
        let resyncs = self.collectors.metrics.resyncs();
        (
            resyncs.get(&[RESYNC_RESULT_SUCCESS]),
            resyncs.get(&[RESYNC_RESULT_FAILURE]),
        )
    }

    fn set_pending_cap(&self, cap: usize) {
        self.handle.shared.lock().pending_cap = cap;
    }

    fn pending_lengths(&self) -> (usize, usize) {
        let state = self.handle.shared.lock();
        (state.pending_set.len(), state.pending_order.len())
    }

    fn epoch(&self) -> u64 {
        self.handle.shared.lock().epoch
    }
}

/// Go's `runReconciler`: the reconciler running on its own task until stopped.
struct Running {
    cancel: CancellationToken,
    task: JoinHandle<Reconciler<TestDocker>>,
}

impl Running {
    fn start(reconciler: Reconciler<TestDocker>) -> Self {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(reconciler.run(cancel.clone()));
        Self { cancel, task }
    }

    async fn stop(self) -> Reconciler<TestDocker> {
        self.cancel.cancel();
        self.task.await.unwrap()
    }
}

const WAIT: Duration = Duration::from_secs(5);

async fn wait_ready(handle: &ReconcilerHandle, what: &str) {
    tokio::time::timeout(WAIT, handle.ready())
        .await
        .unwrap_or_else(|_| panic!("{what}"));
}

// ---- Startup ----

/// Go: `TestReconciler_FirstResyncSeedsCachesAndSignalsReady`
#[tokio::test]
async fn the_first_resync_seeds_the_caches_and_signals_ready() {
    let fixture = fixture(TestDocker::new(
        vec![
            replicated_service("svc1", "stack", "web", 3),
            global_service("glb1", "stack", "agent"),
        ],
        nodes(2),
    ));
    let handle = fixture.handle.clone();
    assert!(!handle.is_ready());
    let running = Running::start(fixture.reconciler);
    wait_ready(&handle, "Ready was never closed").await;
    assert!(handle.is_ready());
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    let agent = service_labels("stack", "agent", SERVICE_MODE_GLOBAL);
    let gauges = &fixture.collectors.replica_gauges;
    assert_eq!(gauges.desired().get_with(&web), Some(3.0));
    assert_eq!(
        gauges.desired().get_with(&agent),
        Some(2.0),
        "eligible nodes"
    );
    let reconciler = running.stop().await;
    assert_eq!(reconciler.nodes().len(), 2);
}

/// Go: `TestReconciler_FirstResyncRetriesAfterFailure`
#[tokio::test(start_paused = true)]
async fn the_first_resync_retries_after_a_failure() {
    let docker = TestDocker::new(vec![replicated_service("svc1", "stack", "web", 1)], vec![]);
    docker.set_list_errors(Some(unavailable()), None);
    let fixture = fixture(docker);
    let handle = fixture.handle.clone();
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    eventually("a failed first resync", WAIT, || {
        docker.service_list_count() > 0
    })
    .await;
    docker.set_list_errors(None, None);
    wait_ready(
        &handle,
        "the first resync never completed after the list recovered",
    )
    .await;
    let reconciler = running.stop().await;
    assert!(reconciler.metadata().get("svc1").is_some());
}

/// Go: `TestReconciler_KeysWaitForFirstResync`
#[tokio::test]
async fn keys_wait_for_the_first_resync() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    let later = Instant::now() + Duration::from_secs(3600);
    assert!(fixture.reconciler.take_due_keys(later).is_empty());
    fixture.complete_first_resync().await;
    assert_eq!(fixture.reconciler.take_due_keys(Instant::now()).len(), 1);
}

// ---- Ordering ----

/// Go: `TestReconciler_UpdateThenRemove`
#[tokio::test]
async fn update_then_remove() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 2)],
        nodes(1),
    ));
    fixture.complete_first_resync().await;
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "remove"));
    fixture.docker.set_services(vec![]);
    fixture.cycle().await;
    assert!(fixture.reconciler.metadata().get("svc1").is_none());
    assert_eq!(fixture.desired_series(), 0);
}

/// Go: `TestReconciler_RemoveThenLateUpdate`: an update delivered after the remove must not
/// bring the service back.
#[tokio::test]
async fn a_late_update_does_not_bring_a_removed_service_back() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 2)],
        nodes(1),
    ));
    fixture.complete_first_resync().await;
    fixture.docker.set_services(vec![]);
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "remove"));
    fixture.cycle().await;
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.cycle().await;
    assert!(fixture.reconciler.metadata().get("svc1").is_none());
    assert_eq!(fixture.desired_series(), 0);
}

/// Go: `TestReconciler_NodeEventDuringRemove`: the node refresh recomputes the service before
/// its removal is reconciled; nothing may be left.
#[tokio::test]
async fn a_node_event_during_a_remove_leaves_nothing() {
    let mut fixture = fixture(TestDocker::new(
        vec![global_service("glb1", "stack", "agent")],
        nodes(1),
    ));
    fixture.complete_first_resync().await;
    fixture.docker.set_services(vec![]);
    fixture.docker.set_nodes(nodes(2));
    fixture
        .handle
        .enqueue_event(&service_event("glb1", "remove"));
    fixture.handle.enqueue_event(&node_event("n2"));
    fixture.cycle().await;
    assert_eq!(fixture.reconciler.nodes().len(), 2);
    assert!(fixture.reconciler.metadata().get("glb1").is_none());
    assert_eq!(fixture.desired_series(), 0);
}

/// Go: `TestReconciler_BurstCoalesces`
#[tokio::test]
async fn a_burst_coalesces_into_one_inspect() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 2)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    for _ in 0..100 {
        fixture
            .handle
            .enqueue_event(&service_event("svc1", "update"));
    }
    fixture.cycle().await;
    assert_eq!(fixture.docker.inspect_count("svc1"), 1);
}

/// A re-marked key keeps its place in the FIFO.
#[tokio::test]
async fn a_re_marked_key_keeps_its_fifo_position() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    for service_id in ["a", "b", "a"] {
        fixture
            .handle
            .enqueue_event(&service_event(service_id, "update"));
    }
    let taken: Vec<String> = fixture
        .reconciler
        .take_due_keys(Instant::now())
        .into_iter()
        .map(|key| key.service_id)
        .collect();
    assert_eq!(taken, ["a", "b"]);
}

/// At most [`SERVICE_KEYS_PER_CYCLE`] keys a cycle, the rest left in order.
#[tokio::test]
async fn a_cycle_takes_at_most_its_share_of_keys() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    for index in 0..40 {
        fixture
            .handle
            .enqueue_event(&service_event(&format!("svc{index:02}"), "update"));
    }
    let first = fixture.reconciler.take_due_keys(Instant::now());
    assert_eq!(first.len(), SERVICE_KEYS_PER_CYCLE);
    assert_eq!(first[0].service_id, "svc00");
    let rest = fixture.reconciler.take_due_keys(Instant::now());
    assert_eq!(rest.len(), 8);
    assert_eq!(rest[0].service_id, "svc32");
}

/// Go: `TestReconciler_EventDuringStartupSnapshot`: a service created after the service list
/// read the services is reported only by its event.
#[tokio::test]
async fn an_event_during_the_startup_snapshot_is_reconciled() {
    let web = replicated_service("svc1", "stack", "web", 1);
    let late = replicated_service("svc2", "stack", "late", 4);
    let fixture = fixture(TestDocker::new(vec![web.clone()], vec![]));
    let (docker, handle) = (Arc::clone(&fixture.docker), fixture.handle.clone());
    fixture.docker.lock().after_service_list = Some(Box::new(move || {
        docker.set_services(vec![web, late]);
        handle.enqueue_event(&service_event("svc2", "create"));
    }));
    let gauges = Arc::clone(&fixture.collectors.replica_gauges);
    let running = Running::start(fixture.reconciler);
    let labels = service_labels("stack", "late", SERVICE_MODE_REPLICATED);
    eventually("svc2 reconciled from its event", WAIT, || {
        gauges.desired().get_with(&labels).is_some()
    })
    .await;
    let reconciler = running.stop().await;
    let desired = reconciler
        .metadata()
        .get("svc2")
        .map(|metadata| metadata.desired_replicas);
    assert_eq!(desired, Some(4.0));
}

/// Go: `TestReconciler_LabelIdentityChangeDeletesOldSeries`
#[tokio::test]
async fn a_label_identity_change_deletes_the_old_series() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 2)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    fixture
        .docker
        .set_services(vec![replicated_service("svc1", "other", "web", 2)]);
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.cycle().await;
    assert_eq!(
        fixture.desired_series(),
        1,
        "the old stack's series must be deleted"
    );
    let moved = service_labels("other", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.desired(&moved), Some(2.0));
}

/// Go: `TestReconciler_ResyncRemovesServicesNoLongerListed`
#[tokio::test]
async fn a_resync_removes_services_no_longer_listed() {
    let mut fixture = fixture(TestDocker::new(
        vec![
            replicated_service("svc1", "stack", "web", 1),
            replicated_service("svc2", "stack", "db", 1),
        ],
        vec![],
    ));
    fixture.complete_first_resync().await;
    fixture
        .docker
        .set_services(vec![replicated_service("svc1", "stack", "web", 1)]);
    fixture.handle.request_resync("test");
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert!(fixture.reconciler.metadata().get("svc2").is_none());
    assert_eq!(fixture.desired_series(), 1);
}

/// Go: `TestReconciler_JobCounts`: a node event recomputes every service from the new snapshot.
#[tokio::test]
async fn job_counts_follow_the_node_snapshot() {
    let mut fixture = fixture(TestDocker::new(
        vec![
            replicated_job_service("rjob1", "stack", "migrate", 5),
            global_job_service("gjob1", "stack", "prune"),
        ],
        nodes(2),
    ));
    fixture.complete_first_resync().await;
    fixture.docker.set_nodes(nodes(3));
    fixture.handle.enqueue_event(&node_event("n3"));
    fixture.cycle().await;
    let replicated = service_labels("stack", "migrate", SERVICE_MODE_REPLICATED_JOB);
    let global = service_labels("stack", "prune", SERVICE_MODE_GLOBAL_JOB);
    assert_eq!(fixture.desired(&replicated), Some(5.0), "total completions");
    assert_eq!(fixture.schedulable(&replicated), Some(0.0));
    assert_eq!(fixture.desired(&global), Some(3.0), "eligible nodes");
    assert_eq!(fixture.schedulable(&global), Some(0.0));
}

// ---- Bounds ----

/// Go: `TestReconciler_OverflowRequestsResync`
#[tokio::test]
async fn an_overflow_requests_a_resync() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture.set_pending_cap(4);
    let (requested_before, ..) = fixture.handle.resync_state();
    let epoch_before = fixture.epoch();
    for index in 0..5 {
        fixture
            .handle
            .enqueue_event(&service_event(&format!("svc{index}"), "update"));
    }
    assert_eq!(fixture.pending_lengths(), (0, 0));
    let (requested, completed, _) = fixture.handle.resync_state();
    assert_eq!(requested, requested_before + 1);
    assert!(requested > completed, "one more outstanding request");
    assert!(
        fixture.epoch() > epoch_before,
        "the overflow bumps the epoch"
    );
    assert!(
        fixture.sink.contents().contains(
            "level=WARN dirty service keys over the cap; dropping them for a resync cap=4"
        ),
        "{}",
        fixture.sink.contents()
    );
}

/// Go: `TestReconciler_RepeatedOverflowsLeaveNoStaleKeys`
#[tokio::test]
async fn repeated_overflows_leave_no_stale_keys() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture.set_pending_cap(4);
    for round in 0..3 {
        for index in 0..5 {
            fixture
                .handle
                .enqueue_event(&service_event(&format!("r{round}-svc{index}"), "update"));
        }
        assert_eq!(fixture.pending_lengths(), (0, 0), "overflow {round}");
    }
    // Nothing is cached: every generation the dropped keys bumped must be gone with them.
    let generations = fixture.handle.shared.lock().service_generation.len();
    assert!(
        generations <= 4,
        "service generations = {generations}, want at most the cap"
    );
    fixture
        .handle
        .enqueue_event(&service_event("fresh", "update"));
    let taken = fixture.reconciler.take_due_keys(Instant::now());
    assert_eq!(
        taken,
        [DueKey {
            service_id: "fresh".to_owned(),
            attempts: 0
        }]
    );
}

/// Overflow keeps the generations of cached services, and prunes only the others.
#[tokio::test]
async fn an_overflow_keeps_the_generations_of_cached_services() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    fixture.set_pending_cap(2);
    for index in 0..3 {
        fixture
            .handle
            .enqueue_event(&service_event(&format!("burst{index}"), "update"));
    }
    let state = fixture.handle.shared.lock();
    let kept: Vec<&String> = state.service_generation.keys().collect();
    assert_eq!(kept, ["svc1"]);
}

/// Go: `TestReconciler_OverflowDuringResyncRunsAnotherPass`: the overflow lands after the
/// service list returned and before its result is applied.
#[tokio::test]
async fn an_overflow_during_a_resync_runs_another_pass() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    fixture.set_pending_cap(4);
    let handle = fixture.handle.clone();
    fixture.docker.lock().after_service_list = Some(Box::new(move || {
        for index in 0..5 {
            handle.enqueue_event(&service_event(&format!("burst{index}"), "update"));
        }
    }));
    fixture.handle.request_resync("test");
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert!(
        fixture.reconciler.resync_due(Instant::now()),
        "no second pass due after an overflow during the first"
    );
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert_eq!(
        fixture.docker.service_list_count(),
        3,
        "startup, the pass, the second pass"
    );
    let (requested, completed, outstanding_since) = fixture.handle.resync_state();
    assert_eq!(requested, completed);
    assert_eq!(outstanding_since, None);
}

/// Go: `TestReconciler_EmptyActorIDDropped`
#[tokio::test]
async fn an_event_without_actor_id_is_dropped_and_counted() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture.handle.enqueue_event(&service_event("", "update"));
    fixture.handle.enqueue_event(&Event {
        kind: EVENT_TYPE_NODE.to_owned(),
        action: "update".to_owned(),
        ..Event::default()
    });
    assert_eq!(fixture.pending_lengths().0, 0);
    assert!(!fixture.reconciler.nodes_due(Instant::now()));
    assert_eq!(
        Some(fixture.collectors.metrics.events_dropped().get()),
        Some(2.0)
    );
}

/// Events of another type are ignored.
#[tokio::test]
async fn events_of_other_types_are_ignored() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture.handle.enqueue_event(&Event {
        kind: "container".to_owned(),
        ..service_event("c1", "start")
    });
    assert_eq!(fixture.pending_lengths(), (0, 0));
    assert!(!fixture.reconciler.nodes_due(Instant::now()));
    assert_eq!(
        Some(fixture.collectors.metrics.events_dropped().get()),
        Some(0.0)
    );
}

/// Go: `TestReconciler_RetryLimitRequestsResync`
#[tokio::test(start_paused = true)]
async fn the_retry_limit_requests_a_resync() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    fixture.docker.set_inspect_err(
        "svc1",
        Some(Error::connection_failed("inspect unavailable".to_owned())),
    );
    // Keep the resync this ends in from succeeding, so the count below is only the key's.
    fixture.docker.set_list_errors(Some(unavailable()), None);
    let (requested_before, ..) = fixture.handle.resync_state();
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    let handle = fixture.handle.clone();
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    eventually("the retries to run out and request a resync", WAIT, || {
        handle.resync_state().0 > requested_before
    })
    .await;
    let reconciler = running.stop().await;
    assert_eq!(
        docker.inspect_count("svc1"),
        4,
        "one attempt and one per retry delay"
    );
    assert_eq!(handle.shared.lock().pending_set.len(), 0);
    drop(reconciler);
    let logs = fixture.sink.contents();
    assert!(
        logs.contains(
            "level=WARN service inspect keeps failing; leaving it to a resync service_id=svc1 \
             attempts=4 err=\"service inspect svc1: inspect unavailable\""
        ),
        "{logs}"
    );
}

/// Go: `TestReconciler_FreshEventSupersedesRetry`
#[tokio::test]
async fn a_fresh_event_supersedes_a_retry() {
    let mut fixture = fixture_with(
        TestDocker::new(vec![replicated_service("svc1", "stack", "web", 1)], vec![]),
        &ReconcilerPolicy {
            retry_delays: vec![Duration::from_secs(3600)],
            ..test_policy()
        },
    );
    fixture.complete_first_resync().await;
    fixture.docker.set_inspect_err(
        "svc1",
        Some(Error::connection_failed("inspect unavailable".to_owned())),
    );
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.cycle().await;
    assert!(
        fixture.reconciler.take_due_keys(Instant::now()).is_empty(),
        "the failed key was due at once"
    );
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    let taken = fixture.reconciler.take_due_keys(Instant::now());
    assert_eq!(
        taken,
        [DueKey {
            service_id: "svc1".to_owned(),
            attempts: 0
        }],
        "due now with its attempts reset"
    );
}

// ---- Failure ----

/// Go: `TestReconciler_NodeListFailureLeavesStateUnchanged`
#[tokio::test(start_paused = true)]
async fn a_node_list_failure_leaves_the_state_unchanged() {
    let docker = TestDocker::new(vec![replicated_service("svc1", "stack", "web", 1)], vec![]);
    docker.set_list_errors(None, Some(unavailable()));
    let mut fixture = fixture(docker);
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert!(fixture.reconciler.metadata().get("svc1").is_none());
    assert_eq!(fixture.desired_series(), 0);
    assert_eq!(fixture.handle.resync_state().1, 0);
    assert!(
        !fixture.reconciler.resync_due(Instant::now()),
        "the failed resync is backed off"
    );
    fixture.docker.set_list_errors(None, None);
    let handle = fixture.handle.clone();
    let running = Running::start(fixture.reconciler);
    wait_ready(&handle, "the resync was never retried").await;
    running.stop().await;
}

/// A failed node refresh keeps the flag and backs off; the next one succeeds.
#[tokio::test(start_paused = true)]
async fn a_failed_node_refresh_backs_off() {
    let mut fixture = fixture(TestDocker::new(vec![], nodes(1)));
    fixture.complete_first_resync().await;
    fixture.docker.set_list_errors(None, Some(unavailable()));
    fixture.handle.enqueue_event(&node_event("n1"));
    fixture.cycle().await;
    assert!(
        fixture.handle.shared.lock().nodes_dirty,
        "the flag stays set"
    );
    assert!(!fixture.reconciler.nodes_due(Instant::now()), "backed off");
    assert_eq!(
        fixture.reconciler.next_wake_delay(Instant::now()),
        Some(Duration::from_millis(20))
    );
    fixture.docker.set_list_errors(None, None);
    fixture.docker.set_nodes(nodes(2));
    tokio::time::sleep(Duration::from_millis(20)).await;
    fixture.cycle().await;
    assert_eq!(fixture.reconciler.nodes().len(), 2);
    let logs = fixture.sink.contents();
    let warning = "level=WARN node refresh failed; will retry err=\"node list: list unavailable\" \
                   backoff=20ms";
    assert!(logs.contains(warning), "{logs}");
}

/// Go: `TestReconciler_PersistentListFailureIsRateLimited`: the backoff (20, 40, 80, 80, …
/// ms) fits about 7 attempts in the window; without it the loop would spin through thousands.
#[tokio::test(start_paused = true)]
async fn a_persistent_list_failure_is_rate_limited() {
    let docker = TestDocker::default();
    docker.set_list_errors(Some(unavailable()), None);
    let fixture = fixture(docker);
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = docker.service_list_count();
    running.stop().await;
    assert!(
        (2..=12).contains(&calls),
        "service list calls in 400ms = {calls}"
    );
}

/// Go: `TestReconciler_PeriodicResync`
#[tokio::test(start_paused = true)]
async fn a_periodic_resync_runs() {
    let fixture = fixture_with(
        TestDocker::default(),
        &ReconcilerPolicy {
            resync_interval: Duration::from_millis(20),
            ..test_policy()
        },
    );
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    eventually("a periodic resync after the first", WAIT, || {
        docker.service_list_count() >= 3
    })
    .await;
    running.stop().await;
}

// ---- Deadlines ----

/// Runs `step` and returns how long it took on the paused clock.
async fn timed(step: impl Future<Output = ()>) -> Duration {
    let started = Instant::now();
    step.await;
    started.elapsed()
}

/// Go: `TestDockerCalls_HaveDeadlines`, observed through the fake: every call is held forever in
/// turn, and each step it belongs to still ends at the request deadline.
#[tokio::test(start_paused = true)]
async fn docker_calls_have_deadlines() {
    let mut fixture = fixture(TestDocker::new(
        vec![
            global_service("glb1", "stack", "agent"),
            replicated_service("svc1", "stack", "web", 2),
        ],
        nodes(1),
    ));
    let cancel = CancellationToken::new();

    // The resync: the service list, then the node list.
    fixture.docker.set_blocked(Call::ServiceList, true);
    let elapsed = timed(fixture.reconciler.resync(&cancel)).await;
    assert_eq!(elapsed, DOCKER_REQUEST_TIMEOUT, "service list");
    fixture.docker.set_blocked(Call::ServiceList, false);
    fixture.docker.set_blocked(Call::NodeList, true);
    let elapsed = timed(fixture.reconciler.resync(&cancel)).await;
    assert_eq!(elapsed, DOCKER_REQUEST_TIMEOUT, "node list in the resync");
    assert_eq!(fixture.resyncs(), (Some(0.0), Some(2.0)));
    fixture.docker.set_blocked(Call::NodeList, false);
    fixture.complete_first_resync().await;

    // A service event, then a node event.
    fixture.docker.set_blocked(Call::ServiceInspect, true);
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    let elapsed = timed(fixture.cycle()).await;
    assert_eq!(elapsed, DOCKER_REQUEST_TIMEOUT, "service inspect");
    assert_eq!(
        fixture.pending_lengths(),
        (1, 1),
        "the failed inspect is retried"
    );
    fixture.docker.set_blocked(Call::ServiceInspect, false);
    fixture.docker.set_blocked(Call::NodeList, true);
    fixture.handle.enqueue_event(&node_event("n1"));
    fixture
        .reconciler
        .take_due_keys(Instant::now() + Duration::from_secs(1));
    let elapsed = timed(fixture.cycle()).await;
    assert_eq!(elapsed, DOCKER_REQUEST_TIMEOUT, "node list in the refresh");
    assert!(fixture.handle.shared.lock().nodes_dirty);

    // The poll makes one Docker call, its task list.
    fixture.docker.set_blocked(Call::TaskList, true);
    let snapshot = PollSnapshot {
        services: fixture
            .reconciler
            .metadata()
            .all()
            .into_iter()
            .map(|(id, metadata)| (id, PollService::new(0, metadata)))
            .collect(),
        ..PollSnapshot::default()
    };
    let started = Instant::now();
    let error = poll_replicas_state(&*fixture.docker, &snapshot)
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), DOCKER_REQUEST_TIMEOUT, "task list");
    assert_eq!(error.to_string(), "task list: context deadline exceeded");
    let logs = fixture.sink.contents();
    for line in [
        "level=WARN resync failed; will retry err=\"service list: context deadline exceeded\" \
         backoff=20ms",
        "level=WARN resync failed; will retry err=\"node list: context deadline exceeded\" \
         backoff=40ms",
        "level=WARN node refresh failed; will retry err=\"node list: context deadline exceeded\" \
         backoff=20ms",
    ] {
        assert!(logs.contains(line), "{line} in {logs}");
    }
}

/// Go: `TestReconciler_PanicInStepRequestsResync`, with the Q10 record.
#[tokio::test]
async fn a_panic_in_a_step_requests_a_resync() {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture.docker.lock().panic_on_inspect = true;
    let (requested_before, ..) = fixture.handle.resync_state();
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.cycle().await;
    assert_eq!(fixture.handle.resync_state().0, requested_before + 1);
    assert!(
        !fixture.reconciler.resync_due(Instant::now()),
        "the repair resync waits for the backoff"
    );
    let logs = fixture.sink.contents();
    assert!(
        logs.starts_with(
            "level=ERROR reconciler recovered from panic step=\"service svc1\" panic=\"unexpected \
             service data\" stack="
        ),
        "{logs}"
    );
    assert_eq!(
        fixture.resyncs(),
        (Some(1.0), Some(0.0)),
        "a service panic is no resync"
    );
}

/// Go: `TestReconciler_CancelDuringBlockedCall`: returning within a third of the request
/// deadline shows the blocked call followed the cancellation rather than its own deadline.
#[tokio::test]
async fn a_cancellation_ends_a_blocked_call() {
    let docker = TestDocker::default();
    docker.set_blocked(Call::ServiceList, true);
    let fixture = fixture(docker);
    let docker = Arc::clone(&fixture.docker);
    let metrics = Arc::clone(&fixture.collectors.metrics);
    let called = docker.service_list_called.notified();
    let running = Running::start(fixture.reconciler);
    tokio::time::timeout(WAIT, called)
        .await
        .unwrap_or_else(|_| panic!("the first resync never listed services"));
    assert!(docker.blocked(Call::ServiceList));
    tokio::time::timeout(DOCKER_REQUEST_TIMEOUT / 3, running.stop())
        .await
        .unwrap_or_else(|_| panic!("run did not return after cancellation"));
    let resyncs = metrics.resyncs();
    let counts = (
        resyncs.get(&[RESYNC_RESULT_SUCCESS]),
        resyncs.get(&[RESYNC_RESULT_FAILURE]),
    );
    assert_eq!(
        counts,
        (Some(0.0), Some(0.0)),
        "a cancelled resync is not counted"
    );
}

/// Go: `TestReconciler_PanickingResyncIsRateLimited`
#[tokio::test(start_paused = true)]
async fn a_panicking_resync_is_rate_limited() {
    let docker = TestDocker::default();
    docker.lock().panic_on_service_list = true;
    let fixture = fixture(docker);
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = docker.service_list_count();
    running.stop().await;
    assert!(
        (2..=12).contains(&calls),
        "service list calls in 400ms = {calls}"
    );
}

// ---- Resync outcome metrics ----

/// Go: `TestReconciler_ResyncOutcomeCounted`
#[tokio::test]
async fn resync_outcomes_are_counted() {
    let mut fixture = fixture(TestDocker::default());
    fixture.docker.set_list_errors(Some(unavailable()), None);
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert_eq!(
        fixture.resyncs(),
        (Some(0.0), Some(1.0)),
        "after a failed resync"
    );
    fixture.docker.set_list_errors(None, None);
    fixture.reconciler.resync(&CancellationToken::new()).await;
    assert_eq!(
        fixture.resyncs(),
        (Some(1.0), Some(1.0)),
        "after a successful resync"
    );
}

/// Go: `TestReconciler_CanceledResyncNotCounted`
#[tokio::test]
async fn a_cancelled_resync_is_not_counted() {
    let mut fixture = fixture(TestDocker::default());
    fixture.docker.set_list_errors(Some(unavailable()), None);
    let cancel = CancellationToken::new();
    cancel.cancel();
    fixture.reconciler.resync(&cancel).await;
    assert_eq!(fixture.resyncs(), (Some(0.0), Some(0.0)));
    assert_eq!(fixture.handle.resync_state().1, 0);
}

/// Go: `TestReconciler_PanickingResyncCountedAsFailure`
#[tokio::test]
async fn a_panicking_resync_counts_as_a_failure() {
    let mut fixture = fixture(TestDocker::default());
    fixture.docker.lock().panic_on_service_list = true;
    fixture.cycle().await;
    assert_eq!(
        fixture.docker.service_list_count(),
        1,
        "the one resync of the cycle"
    );
    assert_eq!(fixture.resyncs(), (Some(0.0), Some(1.0)));
    assert!(
        fixture
            .sink
            .contents()
            .starts_with("level=ERROR reconciler recovered from panic step=resync"),
        "{}",
        fixture.sink.contents()
    );
}

/// Go: `TestReconciler_LastResyncSuccessTimestamp`: the completion time, from the clock.
#[tokio::test]
async fn the_last_resync_success_is_the_completion_time() {
    let mut fixture = fixture(TestDocker::default());
    let timestamps = Arc::clone(fixture.collectors.metrics.timestamps());
    assert_eq!(timestamps.last_resync_success(), 0);
    let completed_at = Timestamp::new(1_800_000_000, 5).unwrap();
    fixture.clock.set(completed_at);
    fixture.complete_first_resync().await;
    assert_eq!(timestamps.last_resync_success(), 1_800_000_000_000_000_005);
}

// ---- Scheduling ----

/// The wake delay: due work is zero, nothing scheduled is none, else the earliest time.
#[tokio::test(start_paused = true)]
async fn next_wake_delay_follows_the_earliest_work() {
    let mut fixture = fixture(TestDocker::default());
    let now = Instant::now();
    assert_eq!(
        fixture.reconciler.next_wake_delay(now),
        Some(Duration::ZERO),
        "first resync"
    );
    fixture.complete_first_resync().await;
    assert_eq!(
        fixture.reconciler.next_wake_delay(Instant::now()),
        Some(PERIODIC_RESYNC_INTERVAL),
        "the periodic resync"
    );
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    assert_eq!(
        fixture.reconciler.next_wake_delay(Instant::now()),
        Some(Duration::ZERO)
    );
    let fixture = fixture_with(TestDocker::default(), &test_policy());
    fixture.handle.shared.lock().resync_completed = 1;
    assert_eq!(
        fixture.reconciler.next_wake_delay(Instant::now()),
        Some(Duration::ZERO),
        "a periodic resync never scheduled is due"
    );
}

/// The policy defaults are Go's constants.
#[test]
fn the_default_policy_is_gos() {
    let policy = ReconcilerPolicy::default();
    assert_eq!(policy.pending_cap, 4096);
    assert_eq!(SERVICE_KEYS_PER_CYCLE, 32);
    assert_eq!(policy.resync_interval, Duration::from_secs(300));
    assert_eq!(
        policy.retry_delays,
        [
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2)
        ]
    );
    assert_eq!(policy.backoff_initial, Duration::from_millis(500));
    assert_eq!(policy.backoff_max, Duration::from_secs(30));
}

/// The backoff doubles up to its cap.
#[tokio::test(start_paused = true)]
async fn the_resync_backoff_doubles_up_to_the_cap() {
    let mut fixture = fixture(TestDocker::default());
    fixture.docker.set_list_errors(Some(unavailable()), None);
    let mut delays = Vec::new();
    for _ in 0..4 {
        fixture.reconciler.resync(&CancellationToken::new()).await;
        delays.push(fixture.reconciler.next_wake_delay(Instant::now()));
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    let millis = |value: u64| Some(Duration::from_millis(value));
    assert_eq!(delays, [millis(20), millis(40), millis(80), millis(80)]);
}

/// A removed service is forgotten by the labels of the series it published.
#[tokio::test]
async fn a_removed_service_deletes_its_update_series() {
    let mut fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    fixture.complete_first_resync().await;
    let labels = labels_for_metadata(fixture.reconciler.metadata().get("svc1").unwrap());
    let service_update = Arc::clone(&fixture.collectors.service_update);
    let started = service_update.started().unwrap();
    assert_eq!(started.get_with(&labels), Some(0.0));
    fixture.docker.set_services(vec![]);
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "remove"));
    fixture.cycle().await;
    assert_eq!(started.get_with(&labels), None);
    assert!(fixture.handle.shared.lock().service_generation.is_empty());
}

/// The reconciler's own task is the one run waits on; a wake-up arrives as an event.
#[tokio::test(start_paused = true)]
async fn an_event_wakes_the_reconciler() {
    let fixture = fixture(TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
    ));
    let (handle, docker) = (fixture.handle.clone(), Arc::clone(&fixture.docker));
    let running = Running::start(fixture.reconciler);
    wait_ready(&handle, "the first resync").await;
    handle.enqueue_event(&service_event("svc1", "update"));
    eventually("the event's inspect", WAIT, || {
        docker.inspect_count("svc1") == 1
    })
    .await;
    running.stop().await;
}

/// A test docker holding no lock across its futures.
#[test]
fn the_test_docker_is_shareable() {
    fn shareable<T: Send + Sync>() {}
    shareable::<TestDocker>();
    shareable::<ReconcilerHandle>();
}
