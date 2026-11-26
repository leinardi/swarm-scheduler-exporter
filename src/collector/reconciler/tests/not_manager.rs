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

//! The reconciler on a daemon that is not a Swarm manager (#140): a worker, or Swarm inactive,
//! answers every manager-only call with a 503 "This node is not a swarm manager". That is a
//! state, not a failing resync: no Swarm series, no manager-only call but the backed-off resync
//! probe, and the caches rebuilt once the probe succeeds.

use std::collections::BTreeSet;

use crate::collector::health::Health;
use crate::test_support::gather_series;

use super::*;

const NOT_MANAGER_WARNING: &str =
    "this node is not a swarm manager; swarm metrics are not exported until it is one";
const MANAGER_AGAIN: &str = "this node is a swarm manager; swarm metrics resumed";

fn not_manager() -> Error {
    Error::not_manager(
        "This node is not a swarm manager. Worker nodes can't be used to view or modify cluster \
         state.",
    )
}

fn running_task(service_id: &str, node_id: &str) -> Task {
    let mut task = Task {
        service_id: service_id.to_owned(),
        slot: 1,
        node_id: node_id.to_owned(),
        desired_state: "running".to_owned(),
        ..Task::default()
    };
    "running".clone_into(&mut task.status.state);
    task
}

impl Fixture {
    /// Runs the reconciler's cycles for `span` of paused time, advancing the clock to each wake.
    async fn run_for(&mut self, span: Duration) {
        let end = Instant::now() + span;
        for _ in 0..10_000 {
            if Instant::now() >= end {
                return;
            }
            self.cycle().await;
            match self.reconciler.next_wake_delay(Instant::now()) {
                Some(delay) if delay.is_zero() => {}
                Some(delay) => {
                    let left = end.saturating_duration_since(Instant::now());
                    tokio::time::advance(delay.min(left).max(Duration::from_millis(1))).await;
                }
                None => return,
            }
        }
        panic!("the reconciler never went idle");
    }

    /// Every series of every Swarm family the reconciler writes or publishes.
    fn swarm_series(&self) -> BTreeMap<String, f64> {
        let collectors = &self.collectors;
        crate::test_support::gather_registered(|registry| {
            collectors.replica_gauges.register(registry);
            collectors.service_update.register(registry);
            collectors.nodes_by_state.register(registry);
            collectors.node_metrics.register(registry);
            collectors.restart_policy.register(registry);
            collectors.replicas_state.register(registry);
        })
    }

    fn families(&self) -> BTreeSet<String> {
        self.swarm_series()
            .keys()
            .map(|series| series.split('{').next().unwrap_or_default().to_owned())
            .collect()
    }

    fn warnings(&self) -> usize {
        self.logs.with_message(NOT_MANAGER_WARNING).len()
    }

    fn node_list_count(&self) -> usize {
        self.docker.node_list_calls.load(Ordering::SeqCst)
    }

    /// A manager with one replicated service on one node, past its first resync, one poll
    /// published and the restart policy with it.
    async fn published_manager() -> Self {
        let docker = TestDocker::new(
            vec![replicated_service("svc1", "stack", "web", 1)],
            nodes(1),
        );
        let node_id = docker.lock().nodes[0].id.clone();
        docker.set_tasks(vec![running_task("svc1", &node_id)]);
        let mut fixture = fixture(docker);
        fixture.complete_first_resync().await;
        fixture.cycle().await;
        let snapshot = fixture.reconciler.build_poll_snapshot();
        let counters = poll_replicas_state(&*fixture.docker, &snapshot)
            .await
            .unwrap_or_else(|error| panic!("poll: {error}"));
        fixture
            .reconciler
            .apply_poll_counts(&snapshot, &counters)
            .unwrap_or_else(|error| panic!("publish: {error}"));
        let families = fixture.families();
        for family in [
            "swarm_service_desired_replicas",
            "swarm_service_schedulable_replicas",
            "swarm_service_restart_policy",
            "swarm_cluster_nodes_by_state",
            "swarm_task_replicas_state",
            "swarm_service_running_replicas",
            "swarm_service_at_desired",
        ] {
            assert!(
                families.contains(family),
                "{family} not published: {families:?}"
            );
        }
        assert_eq!(fixture.handle.role(), Role::Manager);
        fixture
    }

    /// The end state of a demotion: the role, no Swarm series, no queued work and no carry-over.
    fn assert_demoted(&self) {
        assert_eq!(self.handle.role(), Role::NotManager);
        assert_eq!(self.swarm_series(), BTreeMap::new());
        assert_eq!(self.pending_lengths(), (0, 0));
        assert!(!self.handle.shared.lock().nodes_dirty);
        assert!(self.reconciler.metadata().ids().is_empty());
        assert!(self.reconciler.nodes().is_empty());
        assert!(self.reconciler.last_published.is_empty());
        assert!(self.reconciler.poll_rejections.is_empty());
        assert_eq!(self.warnings(), 1, "{}", self.logs.dump());
    }
}

/// Never a manager: one warning however many probes, no failed resync counted, no call but the
/// service list, retried on the resync backoff (20, 40, 80, 80, … ms).
#[tokio::test(start_paused = true)]
async fn never_a_manager_probes_on_the_backoff_only() {
    let docker = TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
    );
    docker.set_list_errors(Some(not_manager()), Some(not_manager()));
    let mut fixture = fixture(docker);
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.handle.enqueue_event(&node_event("n1"));
    fixture.run_for(Duration::from_millis(400)).await;
    assert_eq!(fixture.handle.role(), Role::NotManager);
    assert!(!fixture.handle.is_ready());
    assert_eq!(fixture.warnings(), 1, "{}", fixture.logs.dump());
    assert_eq!(fixture.resyncs(), (Some(0.0), Some(0.0)));
    let probes = fixture.docker.service_list_count();
    assert!(
        (3..=12).contains(&probes),
        "service list calls in 400ms = {probes}"
    );
    assert_eq!(fixture.node_list_count(), 0);
    assert_eq!(fixture.docker.inspect_count("svc1"), 0);
    assert_eq!(fixture.swarm_series(), BTreeMap::new());
    // The one wake is the probe, after the capped backoff at most.
    let delay = fixture.reconciler.next_wake_delay(Instant::now());
    assert!(
        delay.is_some_and(|delay| delay <= Duration::from_millis(80)),
        "{delay:?}"
    );
}

/// Once the daemon is a manager, the next probe completes the resync: the role, readiness and
/// one info line.
#[tokio::test(start_paused = true)]
async fn a_node_that_becomes_a_manager_resumes() {
    let docker = TestDocker::new(
        vec![replicated_service("svc1", "stack", "web", 1)],
        nodes(1),
    );
    docker.set_list_errors(Some(not_manager()), None);
    let mut fixture = fixture(docker);
    fixture.run_for(Duration::from_millis(100)).await;
    assert_eq!(fixture.handle.role(), Role::NotManager);
    fixture.docker.set_list_errors(None, None);
    fixture.run_for(Duration::from_millis(100)).await;
    assert_eq!(fixture.handle.role(), Role::Manager);
    assert!(fixture.handle.is_ready());
    assert!(fixture.reconciler.metadata().get("svc1").is_some());
    assert_eq!(fixture.resyncs(), (Some(1.0), Some(0.0)));
    assert_eq!(
        fixture.logs.with_message(MANAGER_AGAIN).len(),
        1,
        "{}",
        fixture.logs.dump()
    );
    let (requested, completed, outstanding) = fixture.handle.resync_state();
    assert_eq!(
        (requested, outstanding),
        (completed, None),
        "no resync left behind"
    );
}

/// A demotion found by a resync removes every Swarm series, the poller's included, and the poll
/// carry-over; a poll made before it is not published.
#[tokio::test(start_paused = true)]
async fn a_demotion_found_by_a_resync_removes_every_swarm_series() {
    let mut fixture = Fixture::published_manager().await;
    let stale = fixture.reconciler.build_poll_snapshot();
    let counters = poll_replicas_state(&*fixture.docker, &stale).await.unwrap();
    fixture
        .docker
        .set_list_errors(Some(not_manager()), Some(not_manager()));
    fixture.handle.request_resync("test");
    fixture.cycle().await;
    fixture.assert_demoted();
    assert!(matches!(
        fixture.reconciler.apply_poll_counts(&stale, &counters),
        Err(PollError::NotManager)
    ));
    assert_eq!(fixture.swarm_series(), BTreeMap::new());
    assert_eq!(fixture.resyncs(), (Some(1.0), Some(0.0)));
}

/// The poll protocol answers a poll made across a demotion with `NotManager`, and publishes
/// nothing.
#[tokio::test(start_paused = true)]
async fn a_poll_across_a_demotion_is_not_published() {
    let fixture = Fixture::published_manager().await;
    let handle = fixture.handle.clone();
    let docker = Arc::clone(&fixture.docker);
    let collectors = fixture.collectors.clone();
    let running = Running::start(fixture.reconciler);
    let cancel = CancellationToken::new();
    let snapshot = handle.request_poll_snapshot(&cancel).await.unwrap();
    let counters = poll_replicas_state(&*docker, &snapshot).await.unwrap();
    docker.set_list_errors(Some(not_manager()), Some(not_manager()));
    handle.request_resync("test");
    eventually("the demotion", WAIT, || handle.role() == Role::NotManager).await;
    let verdict = handle.submit_poll_counts(&cancel, snapshot, counters).await;
    assert!(matches!(verdict, Err(PollError::NotManager)), "{verdict:?}");
    let snapshot = handle.request_poll_snapshot(&cancel).await;
    assert!(
        matches!(snapshot, Err(PollError::NotManager)),
        "{snapshot:?}"
    );
    running.stop().await;
    assert_eq!(
        gather_series(collectors.replicas_state.collector()),
        BTreeMap::new()
    );
}

/// A demotion first seen by the poller, as a task list refused as not a manager, asks for the
/// resync that finds it. Real time: no periodic resync or node refresh comes due meanwhile.
#[tokio::test]
async fn a_task_list_refused_as_not_a_manager_requests_the_resync() {
    let fixture = Fixture::published_manager().await;
    let handle = fixture.handle.clone();
    let docker = Arc::clone(&fixture.docker);
    let running = Running::start(fixture.reconciler);
    docker.set_task_list_err(Some(not_manager()));
    docker.set_list_errors(Some(not_manager()), Some(not_manager()));
    let cancel = CancellationToken::new();
    let polled = poll_and_publish_replicas_state(&*docker, &handle, &cancel).await;
    assert!(
        matches!(&polled, Err(PollError::Poll(error)) if error.docker_error().is_not_manager()),
        "{polled:?}"
    );
    eventually("the demotion", WAIT, || handle.role() == Role::NotManager).await;
    let (requested, completed, _) = handle.resync_state();
    assert!(requested > completed, "the probe stays requested");
    running.stop().await;
}

/// Work queued when the demotion is found (service keys, a due node refresh) is dropped: no
/// inspect and no node list after it, no further warning, and only the probe wakes the
/// reconciler.
#[tokio::test(start_paused = true)]
async fn a_demotion_drops_the_queued_work() {
    let mut fixture = Fixture::published_manager().await;
    fixture.handle.request_resync("test");
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture
        .handle
        .enqueue_event(&service_event("svc2", "create"));
    // A node refresh due now, which the cycle runs before the resync: found through it.
    fixture
        .docker
        .set_list_errors(Some(not_manager()), Some(not_manager()));
    fixture.handle.enqueue_event(&node_event("n1"));
    let node_lists = fixture.node_list_count();
    fixture.cycle().await;
    fixture.assert_demoted();
    assert_eq!(
        fixture.node_list_count(),
        node_lists + 1,
        "the refresh that found it"
    );
    let delay = fixture.reconciler.next_wake_delay(Instant::now());
    assert_eq!(
        delay,
        Some(Duration::from_millis(20)),
        "the probe's backoff alone"
    );
    fixture
        .handle
        .enqueue_event(&service_event("svc1", "update"));
    fixture.handle.enqueue_event(&node_event("n1"));
    fixture.run_for(Duration::from_millis(300)).await;
    assert_eq!(fixture.node_list_count(), node_lists + 1);
    assert_eq!(fixture.docker.inspect_count("svc1"), 0);
    assert_eq!(fixture.docker.inspect_count("svc2"), 0);
    assert_eq!(fixture.warnings(), 1, "{}", fixture.logs.dump());
    assert!(
        fixture.docker.service_list_count() > 2,
        "the probe still runs"
    );
    assert_eq!(fixture.swarm_series(), BTreeMap::new());
}

/// A demotion found by a service inspect stops the batch it is in: no inspect runs after it.
#[tokio::test(start_paused = true)]
async fn a_demotion_found_by_an_inspect_stops_the_batch() {
    let mut fixture = Fixture::published_manager().await;
    fixture.docker.set_services(vec![
        replicated_service("svc1", "stack", "web", 1),
        replicated_service("svc2", "stack", "api", 1),
        replicated_service("svc3", "stack", "db", 1),
    ]);
    fixture.docker.set_inspect_err("svc1", Some(not_manager()));
    fixture
        .docker
        .set_list_errors(Some(not_manager()), Some(not_manager()));
    for id in ["svc1", "svc2", "svc3"] {
        fixture.handle.enqueue_event(&service_event(id, "update"));
    }
    fixture.cycle().await;
    assert_eq!(fixture.docker.inspect_count("svc1"), 1);
    assert_eq!(fixture.docker.inspect_count("svc2"), 0);
    assert_eq!(fixture.docker.inspect_count("svc3"), 0);
    fixture.assert_demoted();
    fixture.run_for(Duration::from_millis(300)).await;
    assert_eq!(fixture.docker.inspect_count("svc2"), 0);
    assert_eq!(fixture.resyncs(), (Some(1.0), Some(0.0)));
}

/// Any other failure is not `NotManager`: a first resync failing with a 500 keeps the role
/// unknown, counts a failed resync and reads as the initial resync for health.
#[tokio::test(start_paused = true)]
async fn a_generic_failure_is_not_a_non_manager() {
    let docker = TestDocker::default();
    docker.set_list_errors(Some(Error::new(ErrorKind::Other, "boom")), None);
    let mut fixture = fixture(docker);
    fixture.run_for(Duration::from_millis(100)).await;
    assert_eq!(fixture.handle.role(), Role::Unknown);
    let (_, failures) = fixture.resyncs();
    assert!(
        failures.is_some_and(|failures| failures >= 1.0),
        "{failures:?}"
    );
    assert_eq!(fixture.warnings(), 0);
    let health = Health::new(
        fixture.handle.clone(),
        Arc::new(Timestamps::default()),
        Duration::from_secs(10),
        true,
        Arc::clone(&fixture.clock) as Arc<dyn Clock>,
    );
    assert_eq!(
        health.snapshot_now(),
        (false, "initial resync not completed")
    );
}
