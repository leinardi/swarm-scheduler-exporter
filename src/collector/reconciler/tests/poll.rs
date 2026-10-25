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

//! The 17 Go poll protocol tests, and the 3 health tests on the poll timestamp
//! (`TestPollAndPublishReplicasState_*`), which drive the protocol through a running reconciler.

use std::sync::mpsc as std_mpsc;

use crate::collector::replicas_state::TaskCounter;
use crate::docker::types::{GoTime, Placement};
use crate::metrics::Collector;
use crate::metrics::testing::{gather_series, series_id};
use crate::test_support::FakeDocker;

use super::*;

const RUNNING_REPLICAS: &str = "swarm_service_running_replicas";
const AT_DESIRED: &str = "swarm_service_at_desired";

// The counts are whole numbers, compared exactly as Go does; a bare float comparison would trip
// clippy's float_cmp, so they are compared wrapped in Some.

/// 2025-01-01T00:00:00Z plus `seconds`, the Go tests' task creation times.
fn created(seconds: i64) -> GoTime {
    GoTime::new(Timestamp::from_second(1_735_689_600 + seconds).unwrap())
}

/// Go's `makeJobTask`; an iteration of 0 is none.
fn job_task(
    service_id: &str,
    slot: i64,
    node_id: &str,
    iteration: u64,
    desired: &str,
    state: &str,
) -> Task {
    let mut task = Task {
        created_at: created(slot),
        service_id: service_id.to_owned(),
        slot,
        node_id: node_id.to_owned(),
        desired_state: desired.to_owned(),
        job_iteration: (iteration != 0).then_some(Version { index: iteration }),
        ..Task::default()
    };
    state.clone_into(&mut task.status.state);
    task
}

/// Go's `runningTask`.
fn running_task(service_id: &str, slot: i64, node_id: &str) -> Task {
    job_task(service_id, slot, node_id, 0, "running", "running")
}

/// Go's `newPollTestSetup`: a reconciler past its first resync on `services` and `nodes`, whose
/// Docker lists `tasks`.
async fn poll_fixture(services: Vec<Service>, nodes: Vec<Node>, tasks: Vec<Task>) -> Fixture {
    let docker = TestDocker::new(services, nodes);
    docker.set_tasks(tasks);
    let mut fixture = fixture(docker);
    fixture.complete_first_resync().await;
    fixture
}

impl Fixture {
    /// Counts the tasks of `snapshot`, as the poller does.
    async fn count(&self, snapshot: &PollSnapshot) -> ServiceCounter {
        poll_replicas_state(&*self.docker, snapshot)
            .await
            .unwrap_or_else(|error| panic!("poll_replicas_state: {error}"))
    }

    /// Go's `pollAgainst` with no change: the verdict on a fresh snapshot.
    async fn poll_once(&mut self) -> Result<(), PollError> {
        let snapshot = self.reconciler.build_poll_snapshot();
        let counters = self.count(&snapshot).await;
        self.reconciler.apply_poll_counts(&snapshot, &counters)
    }

    /// Go's `poll`: one poll with no change in between, which must be fully published.
    async fn poll(&mut self) {
        self.poll_once()
            .await
            .unwrap_or_else(|error| panic!("poll: {error}"));
    }

    /// Go's `update`: Docker returns `services`, and an update event of each is reconciled.
    async fn update(&mut self, services: Vec<Service>) {
        let ids: Vec<String> = services.iter().map(|service| service.id.clone()).collect();
        self.docker.set_services(services);
        for id in &ids {
            self.handle.enqueue_event(&service_event(id, "update"));
        }
        self.cycle().await;
    }

    fn replicas_state_series(&self) -> BTreeMap<String, f64> {
        gather_series(Arc::clone(self.collectors.replicas_state.collector()) as Arc<dyn Collector>)
    }

    /// The published value of `name` with `labels`, none when not published.
    fn value(&self, name: &str, labels: &BTreeMap<String, String>) -> Option<f64> {
        value_in(&self.replicas_state_series(), name, labels)
    }

    fn rejections(&self) -> f64 {
        self.collectors.metrics.poll_rejections().get()
    }

    fn last_poll_success(&self) -> i64 {
        self.collectors.metrics.timestamps().last_poll_success()
    }

    fn now_nanos(&self) -> i64 {
        i64::try_from(self.clock.now().as_nanosecond()).unwrap()
    }
}

/// The value of `name` with `labels` in gathered `series`.
fn value_in(
    series: &BTreeMap<String, f64>,
    name: &str,
    labels: &BTreeMap<String, String>,
) -> Option<f64> {
    let pairs: Vec<(&str, &str)> = labels
        .iter()
        .map(|(label, value)| (label.as_str(), value.as_str()))
        .collect();
    series.get(&series_id(name, &pairs)).copied()
}

// ---- Staleness ----

/// Go: `TestPoll_SameIDJobRerunIsRejected`
#[tokio::test]
async fn a_same_id_job_rerun_is_rejected() {
    let finished = job_task("job1", 0, "n1", 1, "complete", "complete");
    let mut fixture = poll_fixture(
        vec![replicated_job_service("job1", "stack", "migrate", 1)],
        nodes(1),
        vec![finished.clone()],
    )
    .await;
    fixture.poll().await;
    let labels = service_labels("stack", "migrate", SERVICE_MODE_REPLICATED_JOB);
    assert_eq!(
        fixture.value(AT_DESIRED, &labels),
        Some(1.0),
        "after the first run"
    );

    // The job runs again under the same ID: its tasks of iteration 2 now run, and the snapshot
    // still holds iteration 1, whose finished task would count.
    let mut rerun = replicated_job_service("job1", "stack", "migrate", 1);
    rerun.job_status = Some(JobStatus {
        job_iteration: Version { index: 2 },
    });
    fixture.docker.set_tasks(vec![
        finished,
        job_task("job1", 0, "n1", 2, "complete", "running"),
    ]);
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.update(vec![rerun]).await;
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}, want the series carried over"));
    assert_eq!(Some(fixture.rejections()), Some(1.0));
    // Carried over, not the stale count: the next poll, against iteration 2, reports it running.
    assert_eq!(
        fixture.value(AT_DESIRED, &labels),
        Some(1.0),
        "carried over"
    );
    fixture.poll().await;
    assert_eq!(fixture.value(AT_DESIRED, &labels), Some(0.0), "the rerun");
}

/// Go: `TestPoll_ScalingIsRejected`
#[tokio::test]
async fn scaling_is_rejected() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 2)],
        nodes(1),
        vec![running_task("svc1", 1, "n1"), running_task("svc1", 2, "n1")],
    )
    .await;
    fixture.poll().await;
    let labels = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.value(AT_DESIRED, &labels), Some(1.0), "2 of 2");

    // Scaled to 5 while the tasks were listed: 2 running against the old desired of 2 would say
    // at desired.
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture
        .update(vec![replicated_service("svc1", "stack", "web", 5)])
        .await;
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}"));
    assert_eq!(Some(fixture.rejections()), Some(1.0));
    fixture.poll().await;
    assert_eq!(fixture.value(AT_DESIRED, &labels), Some(0.0), "2 of 5");
}

/// Go: `TestPoll_ModeChangeIsRejectedWithoutCarryOver`
#[tokio::test]
async fn a_mode_change_is_rejected_without_carry_over() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture
        .update(vec![global_service("svc1", "stack", "web")])
        .await;
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}"));
    // The label identity changed (service_mode), so the old series are not carried over.
    let replicated = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(
        fixture.value(RUNNING_REPLICAS, &replicated),
        None,
        "the replicated-mode series were carried over across a mode change"
    );
    let global = service_labels("stack", "web", SERVICE_MODE_GLOBAL);
    assert_eq!(
        fixture.value(RUNNING_REPLICAS, &global),
        None,
        "the global-mode series were published from a poll counted as replicated"
    );
}

/// Go: `TestPoll_PlacementChangeIsRejected`
#[tokio::test]
async fn a_placement_change_is_rejected() {
    let mut fixture = poll_fixture(
        vec![global_service("glb1", "stack", "agent")],
        nodes(2),
        vec![running_task("glb1", 0, "n1"), running_task("glb1", 0, "n2")],
    )
    .await;
    fixture.poll().await;
    let mut pinned = global_service("glb1", "stack", "agent");
    pinned.spec.task_template.placement = Some(Placement {
        constraints: vec!["node.hostname == h1".to_owned()],
        ..Placement::default()
    });
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.update(vec![pinned]).await;
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}"));
    assert_eq!(Some(fixture.rejections()), Some(1.0));
}

/// Go: `TestPoll_NodeChangeDuringTaskListRejectsNodeDependentOnly`
#[tokio::test]
async fn a_node_change_during_the_task_list_rejects_node_dependent_services_only() {
    let mut fixture = poll_fixture(
        vec![
            global_service("glb1", "infra", "agent"),
            replicated_service("svc1", "stack", "web", 1),
        ],
        nodes(1),
        vec![running_task("glb1", 0, "n1"), running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    fixture
        .docker
        .set_tasks(vec![running_task("glb1", 0, "n1")]);
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.docker.set_nodes(nodes(2));
    fixture.handle.enqueue_event(&node_event("n2"));
    fixture.cycle().await;
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("apply: {error}"));
    assert_eq!(
        Some(fixture.rejections()),
        Some(1.0),
        "the global service only"
    );
    // The replicated service is not node-dependent: its new count (no task) was published.
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &web), Some(0.0));
    // The global one kept its previous series.
    let agent = service_labels("infra", "agent", SERVICE_MODE_GLOBAL);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &agent), Some(1.0));
}

/// Go: `TestPoll_UnappliedNodeChangeRejectsNodeDependentOnly`
#[tokio::test]
async fn an_unapplied_node_change_rejects_node_dependent_services_only() {
    let mut fixture = poll_fixture(
        vec![
            global_service("glb1", "stack", "agent"),
            replicated_service("svc1", "stack", "web", 1),
        ],
        nodes(1),
        vec![running_task("glb1", 0, "n1"), running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;

    // A node joined, but the node list fails: the refresh sits in backoff with the node event
    // queued and unapplied, before and after the snapshot is taken.
    fixture.docker.set_nodes(nodes(2));
    fixture.docker.set_list_errors(None, Some(unavailable()));
    fixture.handle.enqueue_event(&node_event("n2"));
    fixture.cycle().await;
    fixture.docker.set_tasks(vec![
        running_task("glb1", 0, "n1"),
        running_task("glb1", 0, "n2"),
        running_task("svc1", 1, "n1"),
    ]);
    fixture
        .poll_once()
        .await
        .unwrap_or_else(|error| panic!("apply: {error}"));
    assert_eq!(
        Some(fixture.rejections()),
        Some(1.0),
        "the global service only"
    );
    // Counted against the one-node list, the agent would read 2 running of 1 desired.
    let agent = service_labels("stack", "agent", SERVICE_MODE_GLOBAL);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &agent), Some(1.0));
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &web), Some(1.0));
}

/// Go: `TestPoll_QueuedUnappliedEventIsRejected`
#[tokio::test]
async fn a_queued_unapplied_event_is_rejected() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    // Queued while the tasks were listed, not applied yet.
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}"));
    assert_eq!(Some(fixture.rejections()), Some(1.0));
}

/// Go: `TestPoll_ResyncRequestedDuringTaskListIsRejected`
#[tokio::test]
async fn a_resync_requested_during_the_task_list_is_rejected() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    // A resync (a dropped event, an overflow) means the caches may be behind on anything: the
    // epoch moves and no count from before it is published.
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.handle.request_resync("test");
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("first rejection: {error}"));
    assert_eq!(Some(fixture.rejections()), Some(1.0));
}

/// Go: `TestPoll_RemovedServiceIsDroppedAndMarkedDirty`
#[tokio::test]
async fn a_removed_service_is_dropped_and_marked_dirty() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        vec![],
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.reconciler.remove_service("svc1");
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("apply: {error}"));
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(
        fixture.value(RUNNING_REPLICAS, &web),
        None,
        "a service removed during the poll was published"
    );
    assert_eq!(
        fixture.pending_lengths().0,
        1,
        "the dropped service is marked dirty"
    );
}

/// A running reconciler whose retries and periodic resync are an hour away, so a key that fails
/// stays dirty for the whole test.
fn with_an_hour_until_retries(fixture: &mut Fixture) {
    fixture.reconciler.retry_delays = vec![Duration::from_secs(3600)];
    fixture.reconciler.next_periodic_resync = Some(Instant::now() + Duration::from_secs(3600));
}

/// Fails `service_id`'s inspect, queues an update of it, and waits for the inspect to fail.
async fn fail_inspect(fixture: &Fixture, service_id: &str) {
    fixture
        .docker
        .set_inspect_err(service_id, Some(unavailable()));
    fixture
        .handle
        .enqueue_event(&service_event(service_id, "update"));
    let docker = Arc::clone(&fixture.docker);
    eventually("the inspect to fail", WAIT, || {
        docker.inspect_count(service_id) > 0
    })
    .await;
}

/// Go: `TestPoll_ThirdConsecutiveRejectionOmitsAndFails`
#[tokio::test]
async fn the_third_rejection_in_a_row_omits_and_fails() {
    let mut fixture = poll_fixture(
        vec![
            replicated_service("svc1", "stack", "web", 1),
            replicated_service("svc2", "stack", "db", 1),
        ],
        nodes(1),
        vec![running_task("svc1", 1, "n1"), running_task("svc2", 1, "n1")],
    )
    .await;
    with_an_hour_until_retries(&mut fixture);
    let reconciler = std::mem::replace(
        &mut fixture.reconciler,
        fixture_with(TestDocker::default(), &test_policy()).reconciler,
    );
    let running = Running::start(reconciler);
    let cancel = CancellationToken::new();
    let docker = Arc::clone(&fixture.docker);
    poll_and_publish_replicas_state(&*docker, &fixture.handle, &cancel)
        .await
        .unwrap_or_else(|error| panic!("first poll: {error}"));

    // svc1's inspect now fails with an hour until its retry: its key stays dirty, so every poll
    // is rejected for it.
    fail_inspect(&fixture, "svc1").await;
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    for rejection in 1..=MAX_CARRIED_POLLS {
        poll_and_publish_replicas_state(&*docker, &fixture.handle, &cancel)
            .await
            .unwrap_or_else(|error| {
                panic!("rejection {rejection}: {error}, want the series carried over")
            });
        assert_eq!(
            fixture.value(RUNNING_REPLICAS, &web),
            Some(1.0),
            "carried over"
        );
    }
    let before = fixture.last_poll_success();
    fixture.clock.advance(jiff::SignedDuration::from_secs(1));
    let error = poll_and_publish_replicas_state(&*docker, &fixture.handle, &cancel)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, PollError::PartiallyRejected(omitted) if omitted == &["svc1"]),
        "third rejection: {error}"
    );
    assert_eq!(
        error.to_string(),
        "poll published without services rejected too many polls in a row: [svc1]"
    );
    assert_eq!(
        fixture.value(RUNNING_REPLICAS, &web),
        None,
        "svc1's series still published after its third rejection in a row"
    );
    let db = service_labels("stack", "db", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &db), Some(1.0));
    assert_eq!(
        fixture.last_poll_success(),
        before,
        "the poll timestamp moved on a partial failure"
    );
    assert!(fixture.sink.contents().contains(
        "level=WARN poll published without services rejected too many polls in a row \
         service_ids=[svc1] limit=2"
    ));
    running.stop().await;
}

// ---- Completeness ----

/// Go: `TestPoll_ManyServicesOneTaskList`: a whole poll over more services than a proxy's
/// buffer could carry as a task filter. One task list must cover them all (the task list here
/// takes no filter at all), every service must be counted and published, and the poll must
/// count as a success only then.
#[tokio::test]
async fn many_services_take_one_task_list() {
    const SERVICE_COUNT: usize = 10_001;
    let ids: Vec<String> = (0..SERVICE_COUNT)
        .map(|index| format!("svc-{index:05}"))
        .collect();
    let mut fixture = poll_fixture(
        ids.iter()
            .map(|id| replicated_service(id, "stack", id, 1))
            .collect(),
        nodes(1),
        ids.iter().map(|id| running_task(id, 1, "n1")).collect(),
    )
    .await;
    fixture.reconciler.next_periodic_resync = Some(Instant::now() + Duration::from_secs(3600));
    let reconciler = std::mem::replace(
        &mut fixture.reconciler,
        fixture_with(TestDocker::default(), &test_policy()).reconciler,
    );
    let running = Running::start(reconciler);
    fixture.clock.advance(jiff::SignedDuration::from_secs(1));
    poll_and_publish_replicas_state(&*fixture.docker, &fixture.handle, &CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("poll: {error}"));
    assert_eq!(fixture.docker.task_list_count(), 1);
    // Gathered once: the families hold 17 series per service.
    let series = fixture.replicas_state_series();
    let prefix = format!("{RUNNING_REPLICAS}{{");
    let running_series = series.keys().filter(|id| id.starts_with(&prefix)).count();
    assert_eq!(running_series, SERVICE_COUNT);
    let last = &ids[SERVICE_COUNT - 1];
    let labels = service_labels("stack", last, SERVICE_MODE_REPLICATED);
    assert_eq!(value_in(&series, RUNNING_REPLICAS, &labels), Some(1.0));
    assert_eq!(fixture.last_poll_success(), fixture.now_nanos());
    assert_eq!(Some(fixture.rejections()), Some(0.0));
    running.stop().await;
}

/// Go: `TestPoll_UncountedServiceIsNeverSilentlyDropped`: counts that miss a snapshot service
/// carry it over like a rejected one, and omit it with a failed poll after that, never drop it
/// while the poll succeeds.
#[tokio::test]
async fn an_uncounted_service_is_never_silently_dropped() {
    let mut fixture = poll_fixture(
        vec![
            replicated_service("svc1", "stack", "web", 1),
            replicated_service("svc2", "stack", "db", 1),
        ],
        nodes(1),
        vec![running_task("svc1", 1, "n1"), running_task("svc2", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    for poll in 1..=MAX_CARRIED_POLLS + 1 {
        let snapshot = fixture.reconciler.build_poll_snapshot();
        let mut counters = fixture.count(&snapshot).await;
        counters.remove("svc1");
        let verdict = fixture.reconciler.apply_poll_counts(&snapshot, &counters);
        if poll <= MAX_CARRIED_POLLS {
            verdict.unwrap_or_else(|error| panic!("poll {poll}: {error}, want svc1 carried over"));
            assert_eq!(
                fixture.value(RUNNING_REPLICAS, &web),
                Some(1.0),
                "carried over"
            );
            continue;
        }
        assert!(
            matches!(verdict, Err(PollError::PartiallyRejected(_))),
            "poll {poll}: {verdict:?}"
        );
        assert_eq!(
            fixture.value(RUNNING_REPLICAS, &web),
            None,
            "svc1 still published after its counts were missing too many polls in a row"
        );
    }
    assert_eq!(Some(fixture.rejections()), Some(3.0));
}

// ---- Liveness ----

/// Go: `TestPoll_PendingRetryDoesNotDelayPublication`
#[tokio::test]
async fn a_pending_retry_does_not_delay_publication() {
    let mut fixture = poll_fixture(
        vec![
            replicated_service("svc1", "stack", "web", 1),
            replicated_service("svc2", "stack", "db", 1),
        ],
        nodes(1),
        vec![running_task("svc2", 1, "n1")],
    )
    .await;
    with_an_hour_until_retries(&mut fixture);
    let reconciler = std::mem::replace(
        &mut fixture.reconciler,
        fixture_with(TestDocker::default(), &test_policy()).reconciler,
    );
    let running = Running::start(reconciler);
    // svc2's inspect fails with an hour until its retry: the poll must not wait for it.
    fail_inspect(&fixture, "svc2").await;
    fixture.docker.set_tasks(vec![
        running_task("svc1", 1, "n1"),
        running_task("svc2", 1, "n1"),
    ]);
    tokio::time::timeout(
        WAIT,
        poll_and_publish_replicas_state(
            &*fixture.docker,
            &fixture.handle,
            &CancellationToken::new(),
        ),
    )
    .await
    .expect("the poll waited for the retry")
    .unwrap_or_else(|error| panic!("poll with a retry pending: {error}"));
    let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
    assert_eq!(fixture.value(RUNNING_REPLICAS, &web), Some(1.0));
    running.stop().await;
}

/// Go: `TestPoll_PersistentListFailureDoesNotDelayPolls`
#[tokio::test]
async fn a_persistent_list_failure_does_not_delay_polls() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.reconciler.backoff_max = Duration::from_secs(3600);
    fixture.reconciler.resync_backoff = Duration::from_secs(3600);
    fixture.docker.set_list_errors(Some(unavailable()), None);
    fixture.handle.request_resync("test");
    let reconciler = std::mem::replace(
        &mut fixture.reconciler,
        fixture_with(TestDocker::default(), &test_policy()).reconciler,
    );
    let running = Running::start(reconciler);
    let docker = Arc::clone(&fixture.docker);
    eventually("the resync to fail", WAIT, || {
        docker.service_list_count() > 1
    })
    .await;
    let cancel = CancellationToken::new();
    for poll in 0..3 {
        tokio::time::timeout(
            WAIT,
            poll_and_publish_replicas_state(&*docker, &fixture.handle, &cancel),
        )
        .await
        .expect("the poll waited for the resync")
        .unwrap_or_else(|error| panic!("poll {poll} with a failing resync outstanding: {error}"));
    }
    running.stop().await;
}

// ---- Cancellation races ----

/// Go's `pollExchanges`: the snapshot request, or the apply request of a snapshot taken first.
#[derive(Debug, Clone, Copy)]
enum Exchange {
    Snapshot,
    Apply,
}

const EXCHANGES: [Exchange; 2] = [Exchange::Snapshot, Exchange::Apply];

async fn exchange(
    kind: Exchange,
    handle: &ReconcilerHandle,
    cancel: &CancellationToken,
    snapshot: Arc<PollSnapshot>,
) -> Result<(), PollError> {
    match kind {
        Exchange::Snapshot => handle.request_poll_snapshot(cancel).await.map(drop),
        Exchange::Apply => {
            handle
                .submit_poll_counts(cancel, snapshot, ServiceCounter::new())
                .await
        }
    }
}

/// Go's `newRaceReconciler`, with the snapshot an apply exchange submits, taken before the
/// reconciler runs since it then owns the caches.
async fn race_fixture() -> (Fixture, Arc<PollSnapshot>) {
    let mut fixture = fixture(TestDocker::new(vec![], vec![]));
    fixture.complete_first_resync().await;
    let snapshot = Arc::new(fixture.reconciler.build_poll_snapshot());
    (fixture, snapshot)
}

/// Go: `TestPoll_ReconcilerExitsBeforeSubmission`
#[tokio::test]
async fn the_reconciler_exits_before_the_submission() {
    for kind in EXCHANGES {
        let (fixture, snapshot) = race_fixture().await;
        let running = Running::start(fixture.reconciler);
        let cancel = running.cancel.clone();
        // Kept alive: nobody receives any more, and the poller, on the same root token, gives
        // up.
        let _stopped = tokio::time::timeout(WAIT, running.stop()).await.unwrap();
        let outcome =
            tokio::time::timeout(WAIT, exchange(kind, &fixture.handle, &cancel, snapshot))
                .await
                .unwrap_or_else(|_| panic!("{kind:?}: the exchange did not return"));
        let error = outcome.unwrap_err();
        assert!(error.is_canceled(), "{kind:?}: {error}");
    }
}

/// Go: `TestPoll_ReconcilerExitsBeforeReply`
#[tokio::test]
async fn the_reconciler_exits_before_the_reply() {
    for kind in EXCHANGES {
        let (fixture, snapshot) = race_fixture().await;
        let running = Running::start(fixture.reconciler);
        // Shutdown lands after the request was accepted, before it is answered.
        let reconciler_cancel = running.cancel.clone();
        fixture
            .handle
            .set_poll_request_hook(Some(Arc::new(move || reconciler_cancel.cancel())));
        // The poller's own token is never cancelled, so only the reconciler's answer can release
        // it.
        let outcome = tokio::time::timeout(
            WAIT,
            exchange(kind, &fixture.handle, &CancellationToken::new(), snapshot),
        )
        .await
        .unwrap_or_else(|_| panic!("{kind:?}: the exchange did not return"));
        let error = outcome.unwrap_err();
        assert!(error.is_canceled(), "{kind:?}: {error}");
        assert_eq!(
            error.to_string(),
            "reconciler stopping: context canceled",
            "{kind:?}"
        );
        tokio::time::timeout(WAIT, running.task)
            .await
            .unwrap()
            .unwrap();
    }
}

/// Go: `TestPoll_PollerExitsBeforeReply`. The poller runs on a thread of its own, so the hook
/// can hold the reconciler until the poller gave up without holding the poller too.
#[tokio::test]
async fn the_poller_exits_before_the_reply() {
    for kind in EXCHANGES {
        let (fixture, snapshot) = race_fixture().await;
        let running = Running::start(fixture.reconciler);
        let poller_cancel = CancellationToken::new();
        let (done_sender, done_receiver) = std_mpsc::channel::<Result<(), PollError>>();
        let done_receiver = Mutex::new(done_receiver);
        let (outcome_sender, outcome_receiver) = std_mpsc::channel();

        // The poller gives up after the request was accepted; the reconciler answers only once
        // it has, into the reply nobody reads.
        let hook_cancel = poller_cancel.clone();
        fixture.handle.set_poll_request_hook(Some(Arc::new(move || {
            hook_cancel.cancel();
            let outcome = done_receiver
                .lock()
                .unwrap()
                .recv_timeout(WAIT)
                .expect("the poller did not give up");
            outcome_sender.send(outcome).unwrap();
        })));
        let handle = fixture.handle.clone();
        let poller = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = runtime.block_on(exchange(kind, &handle, &poller_cancel, snapshot));
            done_sender.send(outcome).unwrap();
        });

        // The reconciler must still be serving: a poller that stopped waiting cannot block it.
        let outcome = tokio::task::spawn_blocking(move || outcome_receiver.recv_timeout(WAIT))
            .await
            .unwrap()
            .unwrap_or_else(|_| panic!("{kind:?}: the poller did not give up"));
        poller.join().unwrap();
        fixture.handle.set_poll_request_hook(None);
        tokio::time::timeout(
            WAIT,
            fixture
                .handle
                .request_poll_snapshot(&CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|_| panic!("{kind:?}: no snapshot after an abandoned request"))
        .unwrap_or_else(|error| panic!("{kind:?}: snapshot after an abandoned request: {error}"));
        tokio::time::timeout(WAIT, running.stop()).await.unwrap();
        let error = outcome.unwrap_err();
        assert!(error.is_canceled(), "{kind:?}: {error}");
    }
}

// ---- The poll timestamp ----

/// The fixture's reconciler running, with the poll timestamp a minute in the past.
fn run_with_an_old_poll(fixture: &mut Fixture) -> (Running, i64) {
    let previous = fixture.now_nanos();
    fixture
        .collectors
        .metrics
        .timestamps()
        .mark_poll_ok(fixture.clock.now());
    fixture.clock.advance(jiff::SignedDuration::from_mins(1));
    let reconciler = std::mem::replace(
        &mut fixture.reconciler,
        fixture_with(TestDocker::default(), &test_policy()).reconciler,
    );
    (Running::start(reconciler), previous)
}

/// Go: `TestPollAndPublishReplicasState_SuccessMovesTimestamp`
#[tokio::test]
async fn a_full_publish_moves_the_poll_timestamp() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc", "s", "web", 1)],
        vec![],
        vec![],
    )
    .await;
    let (running, previous) = run_with_an_old_poll(&mut fixture);
    poll_and_publish_replicas_state(
        &FakeDocker::default(),
        &fixture.handle,
        &CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|error| panic!("poll_and_publish_replicas_state: {error}"));
    assert_eq!(
        fixture.last_poll_success(),
        fixture.now_nanos(),
        "the publish time"
    );
    assert!(fixture.last_poll_success() > previous);
    running.stop().await;
}

/// Go: `TestPollAndPublishReplicasState_FailedBuildLeavesTimestamp`: the families carry no
/// custom label, so a service with one fails the build.
#[tokio::test]
async fn a_failed_build_leaves_the_poll_timestamp() {
    let mut fixture = poll_fixture(vec![], vec![], vec![]).await;
    let mut metadata = build_metadata(
        &replicated_service("svc", "s", "web", 1),
        &CustomLabels::default(),
    );
    metadata.custom_labels = BTreeMap::from([("team".to_owned(), "a".to_owned())]);
    fixture.reconciler.metadata.set("svc", metadata);
    let (running, previous) = run_with_an_old_poll(&mut fixture);
    let error = poll_and_publish_replicas_state(
        &FakeDocker::default(),
        &fixture.handle,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, PollError::Publish(message)
            if message.starts_with("build replicas state snapshot; keeping previous")),
        "{error}"
    );
    assert_eq!(fixture.last_poll_success(), previous);
    running.stop().await;
}

/// Go: `TestPollAndPublishReplicasState_FailedPollLeavesTimestamp`
#[tokio::test]
async fn a_failed_poll_leaves_the_poll_timestamp() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc", "s", "web", 1)],
        vec![],
        vec![],
    )
    .await;
    let (running, previous) = run_with_an_old_poll(&mut fixture);
    let docker = FakeDocker {
        task_list_err: Some(unavailable()),
        ..FakeDocker::default()
    };
    let error =
        poll_and_publish_replicas_state(&docker, &fixture.handle, &CancellationToken::new())
            .await
            .unwrap_err();
    assert_eq!(error.to_string(), "task list: list unavailable");
    assert_eq!(fixture.last_poll_success(), previous);
    running.stop().await;
}

/// A rejected service carries over exactly what the last poll published, not the new counts,
/// and the rejection count is replaced along with the published set.
#[tokio::test]
async fn a_carried_over_counter_is_the_last_published_one() {
    let mut fixture = poll_fixture(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
        vec![running_task("svc1", 1, "n1")],
    )
    .await;
    fixture.poll().await;
    let published: TaskCounter = fixture.reconciler.last_published["svc1"].clone();
    fixture.docker.set_tasks(vec![]);
    let snapshot = fixture.reconciler.build_poll_snapshot();
    let counters = fixture.count(&snapshot).await;
    fixture.handle.request_resync("test");
    fixture
        .reconciler
        .apply_poll_counts(&snapshot, &counters)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(fixture.reconciler.last_published["svc1"], published);
    assert_eq!(fixture.reconciler.poll_rejections.get("svc1"), Some(&1));
}
