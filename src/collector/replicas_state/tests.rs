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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;

use super::*;
use crate::collector::metrics_ids::SERVICE_MODE_GLOBAL;
use crate::collector::nodes::{
    LABEL_NODE_AVAILABILITY, LABEL_NODE_ROLE, LABEL_NODE_STATUS, NODES_BY_STATE_NAME, NodesByState,
};
use crate::collector::types::display_name;
use crate::docker::Error;
use crate::docker::types::{GoTime, NodeSpec, NodeStatus, TaskStatus, Version};
use crate::logger::Level;
use crate::metrics::testing::{gather_series, series_id};
use crate::test_support::{FakeDocker, capture_logs, schedulable_node};

const REPLICAS_STATE_NAME: &str = "swarm_task_replicas_state";
const RUNNING_REPLICAS_NAME: &str = "swarm_service_running_replicas";
const AT_DESIRED_NAME: &str = "swarm_service_at_desired";

// The counts are whole numbers, compared exactly as Go does; a bare float comparison would trip
// clippy's float_cmp, so they are compared wrapped in Some.

/// 2025-01-01T00:00:00Z, the Go tests' base time.
const BASE_SECONDS: i64 = 1_735_689_600;

fn at(seconds: i64) -> GoTime {
    GoTime::new(Timestamp::from_second(BASE_SECONDS + seconds).unwrap())
}

/// Go's `makeTask`; `None` is Go's zero status time.
fn make_task(created_at: GoTime, status_timestamp: Option<GoTime>, version_index: u64) -> Task {
    Task {
        created_at,
        version: Version {
            index: version_index,
        },
        status: TaskStatus {
            timestamp: status_timestamp.unwrap_or_default(),
            ..TaskStatus::default()
        },
        ..Task::default()
    }
}

/// Go's `makeStateTask`.
fn make_state_task(created_at: GoTime, desired: &str, state: &str) -> Task {
    let mut task = make_task(created_at, None, 1);
    desired.clone_into(&mut task.desired_state);
    state.clone_into(&mut task.status.state);
    task
}

/// Go's `makeTestMetadata`.
fn test_metadata(stack: &str, service: &str, mode: &str) -> ServiceMetadata {
    ServiceMetadata {
        stack: stack.to_owned(),
        service: service.to_owned(),
        service_mode: mode.to_owned(),
        ..ServiceMetadata::default()
    }
}

/// Go's `serviceLabels`.
fn service_labels(stack: &str, service: &str, mode: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_STACK.to_owned(), stack.to_owned()),
        (LABEL_SERVICE.to_owned(), service.to_owned()),
        (LABEL_SERVICE_MODE.to_owned(), mode.to_owned()),
        (LABEL_DISPLAY_NAME.to_owned(), display_name(stack, service)),
    ])
}

/// Stands in for Go's `cacheSnapshot` until the reconciler builds snapshots (plan phase 14): a
/// polling snapshot of `services` and `nodes`, as the caches would hold them.
fn snapshot(services: &[(&str, ServiceMetadata)], nodes: Vec<Node>) -> PollSnapshot {
    PollSnapshot {
        services: services
            .iter()
            .map(|(id, metadata)| ((*id).to_owned(), PollService::new(0, metadata.clone())))
            .collect(),
        nodes,
        ..PollSnapshot::default()
    }
}

fn fake(tasks: Vec<Task>) -> FakeDocker {
    FakeDocker {
        tasks,
        ..FakeDocker::default()
    }
}

fn replicas_state() -> ReplicasState {
    ReplicasState::new(&CustomLabels::default(), capture_logs(Level::INFO).0)
}

fn gathered(state: &ReplicasState) -> BTreeMap<String, f64> {
    gather_series(Arc::clone(state.collector()) as Arc<dyn Collector>)
}

fn labelled(name: &str, labels: &BTreeMap<String, String>) -> String {
    let pairs: Vec<(&str, &str)> = labels
        .iter()
        .map(|(label, value)| (label.as_str(), value.as_str()))
        .collect();
    series_id(name, &pairs)
}

fn state_labels(labels: &BTreeMap<String, String>, state: &str) -> BTreeMap<String, String> {
    let mut labels = labels.clone();
    labels.insert(LABEL_STATE.to_owned(), state.to_owned());
    labels
}

/// Publishes `counters`, failing the test if the build fails.
fn publish(state: &ReplicasState, counters: &ServiceCounter) {
    state
        .update(counters)
        .unwrap_or_else(|error| panic!("update: {error}"));
}

/// Go: `TestPreferredTask`
#[test]
fn preferred_task_rules() {
    let (base, later) = (at(0), at(1));
    let mut late_shutdown_old = make_state_task(base, "shutdown", "shutdown");
    late_shutdown_old.status.timestamp = at(60);
    let mut late_shutdown_new = make_state_task(later, "running", "assigned");
    late_shutdown_new.status.timestamp = later;

    let cases = [
        (
            "issue 62: start-first update whose new task failed keeps the old one",
            make_state_task(base, "running", "running"),
            make_state_task(later, "shutdown", "failed"),
        ),
        (
            "node down: replacement beats the older task that did stop",
            make_state_task(later, "running", "pending"),
            make_state_task(base, "shutdown", "shutdown"),
        ),
        (
            "crash and restart: replacement beats failed task",
            make_state_task(later, "running", "running"),
            make_state_task(base, "shutdown", "failed"),
        ),
        (
            "start-first in progress: serving old task beats starting new one",
            make_state_task(base, "running", "running"),
            make_state_task(later, "running", "starting"),
        ),
        (
            "start-first with both running: newer wins",
            make_state_task(later, "running", "running"),
            make_state_task(base, "running", "running"),
        ),
        (
            "wanted task beats a newer retired one that stopped",
            make_state_task(base, "ready", "pending"),
            make_state_task(later, "shutdown", "shutdown"),
        ),
        (
            "late shutdown status: new task wins despite older task's newer status timestamp",
            late_shutdown_new,
            late_shutdown_old,
        ),
        (
            "both retired and not running: newer wins",
            make_state_task(later, "shutdown", "rejected"),
            make_state_task(base, "shutdown", "failed"),
        ),
        (
            "both wanted and not running: newer wins",
            make_state_task(later, "running", "preparing"),
            make_state_task(base, "running", "pending"),
        ),
        (
            "empty desired state counts as wanted",
            make_state_task(base, "", "running"),
            make_state_task(later, "shutdown", "failed"),
        ),
    ];
    for (name, winner, loser) in &cases {
        assert!(
            preferred_task(winner, loser),
            "{name}: winner must replace loser"
        );
        assert!(
            !preferred_task(loser, winner),
            "{name}: loser must not replace winner"
        );
    }
}

/// Go: `TestPreferredTask_AllRulesTie`: identical rank, so the first task listed is kept. A
/// retired task that has not stopped never reaches `preferred_task`, so the retired pair
/// reports shutdown.
#[test]
fn tied_tasks_do_not_replace_each_other() {
    for (desired, state) in [("running", "running"), ("shutdown", "shutdown")] {
        let first = make_state_task(at(0), desired, state);
        let second = make_state_task(at(0), desired, state);
        assert!(!preferred_task(&first, &second), "desired={desired}");
        assert!(!preferred_task(&second, &first), "desired={desired}");
    }
}

/// Go: `TestRetiredTask`
#[test]
fn retired_task_by_desired_state() {
    for (desired, want) in [
        ("shutdown", true),
        ("failed", true),
        ("rejected", true),
        ("remove", true),
        ("orphaned", true),
        ("complete", false),
        ("running", false),
        ("ready", false),
        ("", false),
    ] {
        let task = Task {
            desired_state: desired.to_owned(),
            ..Task::default()
        };
        assert_eq!(retired_task(&task), want, "desired={desired:?}");
    }
}

/// Go: `TestRetiredNotStopped`
#[test]
fn retired_not_stopped_needs_retired_and_not_stopped() {
    let stopped = [
        "complete", "shutdown", "failed", "rejected", "remove", "orphaned",
    ];
    let not_stopped = [
        "new",
        "pending",
        "assigned",
        "preparing",
        "starting",
        "running",
        "",
    ];
    let check = |desired: &str, state: &str, want: bool| {
        let task = Task {
            desired_state: desired.to_owned(),
            status: TaskStatus {
                state: state.to_owned(),
                ..TaskStatus::default()
            },
            ..Task::default()
        };
        assert_eq!(
            retired_not_stopped(&task),
            want,
            "desired={desired:?} state={state:?}"
        );
    };
    // A task Swarm still wants is never skipped, whatever its state.
    for state in stopped.iter().chain(&not_stopped) {
        for desired in ["running", "ready", "complete", ""] {
            check(desired, state, false);
        }
    }
    for desired in ["shutdown", "remove"] {
        for state in stopped {
            check(desired, state, false);
        }
        for state in not_stopped {
            check(desired, state, true);
        }
    }
}

/// Go: `TestNewerThan_CreatedAt`
#[test]
fn newer_than_by_creation_time() {
    let newer = make_task(at(1), None, 1);
    let older = make_task(at(0), None, 2);
    assert!(newer_than(&newer, &older));
    assert!(!newer_than(&older, &newer));
}

/// Go: `TestNewerThan_CreatedAt_Equal_FallsBackToStatusTimestamp`
#[test]
fn newer_than_falls_back_to_the_status_time() {
    let later = make_task(at(0), Some(at(1)), 1);
    let earlier = make_task(at(0), Some(at(0)), 1);
    assert!(newer_than(&later, &earlier));
    assert!(!newer_than(&earlier, &later));
}

/// Go: `TestNewerThan_CreatedAt_Equal_OneZeroStatusTS_FallsBackToVersion`, with both status
/// times zero as the Go test builds them, plus the one-sided case its name describes.
#[test]
fn newer_than_falls_back_to_the_version() {
    let higher = make_task(at(0), None, 10);
    let lower = make_task(at(0), None, 5);
    assert!(newer_than(&higher, &lower));
    assert!(!newer_than(&lower, &higher));

    // One status time zero: the later status time does not decide, the version does.
    let stamped_lower = make_task(at(0), Some(at(5)), 5);
    assert!(newer_than(&higher, &stamped_lower));
    assert!(!newer_than(&stamped_lower, &higher));
}

/// Go: `TestNewerThan_AllEqual`: not strictly newer.
#[test]
fn newer_than_is_strict() {
    let first = make_task(at(0), Some(at(0)), 5);
    let second = make_task(at(0), Some(at(0)), 5);
    assert!(!newer_than(&first, &second));
}

/// Go: `TestTaskCounter_IncAndGet`
#[test]
fn task_counter_counts_per_state() {
    let mut counter = TaskCounter::new(BTreeMap::from([
        ("stack".to_owned(), "s".to_owned()),
        ("service".to_owned(), "sv".to_owned()),
    ]));
    counter.inc("running");
    counter.inc("running");
    counter.inc("failed");
    assert_eq!(Some(counter.state("running")), Some(2.0));
    assert_eq!(Some(counter.state("failed")), Some(1.0));
    assert_eq!(Some(counter.state("new")), Some(0.0));
    assert_eq!(counter.states.len(), 2);
}

/// A task of `service_id` in `slot` on `node`, created at `created_at`.
fn placed_task(
    service_id: &str,
    node: &str,
    slot: i64,
    created_at: GoTime,
    desired: &str,
    state: &str,
) -> Task {
    let mut task = make_state_task(created_at, desired, state);
    service_id.clone_into(&mut task.service_id);
    node.clone_into(&mut task.node_id);
    task.slot = slot;
    task
}

async fn poll(tasks: Vec<Task>, snapshot: &PollSnapshot) -> ServiceCounter {
    poll_replicas_state(&fake(tasks), snapshot)
        .await
        .unwrap_or_else(|error| panic!("poll: {error}"))
}

/// Go: `TestPollReplicasState_DedupeBySlot_NewerWins`: same desired state and neither running,
/// so only the creation time tells them apart.
#[tokio::test]
async fn dedupe_by_slot_keeps_the_newer_task() {
    let snapshot = snapshot(
        &[(
            "svc1",
            test_metadata("stack", "svc", SERVICE_MODE_REPLICATED),
        )],
        vec![],
    );
    let older = placed_task("svc1", "", 1, at(0), "shutdown", "failed");
    let mut newer = placed_task("svc1", "", 1, at(1), "shutdown", "rejected");
    newer.version.index = 2;
    let counters = poll(vec![older, newer], &snapshot).await;
    let counter = &counters["svc1"];
    assert_eq!(Some(counter.state("rejected")), Some(1.0));
    assert_eq!(
        Some(counter.state("failed")),
        Some(0.0),
        "the older task is deduped"
    );
}

/// Go: `TestPollReplicasState_NewerRetiredTask_DoesNotHideRunningTask`: issue 62, a start-first
/// update whose new task failed leaves a newer retired task next to the serving one, in the same
/// slot (replicated) or on the same node (global).
#[tokio::test]
async fn a_newer_retired_task_does_not_hide_the_running_one() {
    for (mode, slot, node) in [
        (SERVICE_MODE_REPLICATED, 1, ""),
        (SERVICE_MODE_GLOBAL, 0, "node-a"),
    ] {
        let snapshot = snapshot(&[("svc1", test_metadata("stack", "svc", mode))], vec![]);
        let serving = placed_task("svc1", node, slot, at(0), "running", "running");
        let mut failed_update = placed_task("svc1", node, slot, at(3600), "shutdown", "failed");
        failed_update.version.index = 2;
        // Both list orders: Docker does not guarantee one.
        for tasks in [
            vec![serving.clone(), failed_update.clone()],
            vec![failed_update.clone(), serving.clone()],
        ] {
            let counters = poll(tasks, &snapshot).await;
            assert_eq!(Some(counters["svc1"].state("running")), Some(1.0), "{mode}");
            assert_eq!(
                Some(counters["svc1"].state("failed")),
                Some(0.0),
                "{mode}: retired task counted"
            );
        }
    }
}

/// Go: `TestPollReplicasState_GlobalService_DedupeByNodeID`: two nodes, two dedupe keys.
#[tokio::test]
async fn global_service_dedupes_by_node() {
    let snapshot = snapshot(
        &[("glb1", test_metadata("", "glbsvc", SERVICE_MODE_GLOBAL))],
        vec![],
    );
    let tasks = vec![
        placed_task("glb1", "node-a", 0, at(0), "", "running"),
        placed_task("glb1", "node-b", 0, at(0), "", "running"),
    ];
    let counters = poll(tasks, &snapshot).await;
    assert_eq!(
        Some(counters["glb1"].state("running")),
        Some(2.0),
        "one per node"
    );
}

/// Go's `castorTasks`: on a two-node cluster with one node asleep, n1 runs the global service's
/// task, and the down node n2 still reports its retired task running, next to an older retired
/// task that did stop.
fn castor_tasks() -> Vec<Task> {
    vec![
        placed_task("svc1", "n1", 0, at(0), "running", "running"),
        placed_task("svc1", "n2", 0, at(3600), "shutdown", "running"),
        placed_task("svc1", "n2", 0, at(0), "shutdown", "shutdown"),
    ]
}

type RetiredCase = (
    &'static str,
    &'static str,
    Vec<Task>,
    &'static [(&'static str, f64)],
);

/// Go: `TestPollReplicasState_RetiredNotStoppedSkipped`: a retired task that has not reported
/// stopping is counted under no state, and cannot stand for its slot or node.
#[tokio::test]
async fn retired_tasks_that_did_not_stop_are_skipped() {
    // Name, mode, tasks, and the states counted; every other state must be 0.
    let cases: [RetiredCase; 4] = [
        (
            "down node: the older task that stopped stands for it",
            SERVICE_MODE_GLOBAL,
            castor_tasks(),
            &[("running", 1.0), ("shutdown", 1.0)],
        ),
        (
            "down node with only a retired running task: node counted nowhere",
            SERVICE_MODE_GLOBAL,
            vec![
                placed_task("svc1", "n1", 0, at(0), "running", "running"),
                placed_task("svc1", "n2", 0, at(0), "shutdown", "running"),
            ],
            &[("running", 1.0)],
        ),
        (
            "slot whose only task is retired while starting",
            SERVICE_MODE_REPLICATED,
            vec![placed_task("svc1", "n1", 1, at(0), "shutdown", "starting")],
            &[],
        ),
        (
            "stop-first update in progress",
            SERVICE_MODE_REPLICATED,
            vec![
                placed_task("svc1", "n1", 1, at(0), "shutdown", "running"),
                placed_task("svc1", "n1", 1, at(1), "ready", "pending"),
            ],
            &[("pending", 1.0)],
        ),
    ];
    for (name, mode, tasks, want) in cases {
        let snapshot = snapshot(&[("svc1", test_metadata("stack", "svc", mode))], vec![]);
        let mut reversed = tasks.clone();
        reversed.reverse();
        // Both list orders: Docker does not guarantee one.
        for tasks in [tasks, reversed] {
            let counters = poll(tasks, &snapshot).await;
            let counter = &counters["svc1"];
            for state in KNOWN_TASK_STATES {
                let expected = want
                    .iter()
                    .find(|(wanted, _)| *wanted == state)
                    .map_or(0.0, |(_, count)| *count);
                assert_eq!(
                    Some(counter.state(state)),
                    Some(expected),
                    "{name}: {state}"
                );
            }
        }
    }
}

/// Go: `TestUpdateReplicasStateGauge_RetiredNotStoppedOnDownNode`: the down node's stale task
/// neither makes the global service look over-provisioned nor stands in for a missing replica.
#[tokio::test]
async fn a_down_nodes_stale_task_is_published_nowhere() {
    let state = replicas_state();
    let mut metadata = test_metadata("s", "sv", SERVICE_MODE_GLOBAL);
    metadata.desired_replicas = 1.0;
    let counters = poll(castor_tasks(), &snapshot(&[("svc1", metadata)], vec![])).await;
    publish(&state, &counters);
    let series = gathered(&state);
    let labels = service_labels("s", "sv", SERVICE_MODE_GLOBAL);
    assert_eq!(
        series.get(&labelled(RUNNING_REPLICAS_NAME, &labels)),
        Some(&1.0)
    );
    assert_eq!(series.get(&labelled(AT_DESIRED_NAME, &labels)), Some(&1.0));
}

/// Go: `TestPollReplicasState_ServiceNotInSnapshot_SkippedWithoutInspect`: a service the
/// reconciler has not applied is skipped, and the poller inspects nothing.
#[tokio::test]
async fn a_service_not_in_the_snapshot_is_skipped_without_an_inspect() {
    let snapshot = snapshot(
        &[(
            "svc_known",
            test_metadata("s", "web", SERVICE_MODE_REPLICATED),
        )],
        vec![],
    );
    let docker = fake(vec![placed_task("svc_other", "", 1, at(0), "", "running")]);
    let counters = poll_replicas_state(&docker, &snapshot).await.unwrap();
    assert!(!counters.contains_key("svc_other"));
    assert!(counters.contains_key("svc_known"));
    assert_eq!(
        docker.service_inspect_calls(),
        0,
        "the poller reads the snapshot only"
    );
}

/// Go: `TestPollReplicasState_TaskListError`, with the wrapped wording.
#[tokio::test]
async fn a_task_list_error_fails_the_poll() {
    let snapshot = snapshot(
        &[("svc", test_metadata("s", "web", SERVICE_MODE_REPLICATED))],
        vec![],
    );
    let docker = FakeDocker {
        task_list_err: Some(Error::deadline_exceeded()),
        ..FakeDocker::default()
    };
    let error = poll_replicas_state(&docker, &snapshot).await.unwrap_err();
    assert_eq!(error.to_string(), "task list: context deadline exceeded");
}

/// An empty snapshot lists no tasks: a task list failure cannot even happen.
#[tokio::test]
async fn an_empty_snapshot_lists_no_tasks() {
    let docker = FakeDocker {
        task_list_err: Some(Error::deadline_exceeded()),
        ..FakeDocker::default()
    };
    let counters = poll_replicas_state(&docker, &PollSnapshot::default())
        .await
        .unwrap();
    assert!(counters.is_empty());
}

/// Go: `TestPollReplicasState_ServicesWithoutTasks_EmitZeroSeries`
#[tokio::test]
async fn services_without_tasks_emit_zero_series() {
    let state = replicas_state();
    let with_desired = |service: &str, mode: &str, desired: f64| {
        let mut metadata = test_metadata("s", service, mode);
        metadata.desired_replicas = desired;
        metadata
    };
    let snapshot = snapshot(
        &[
            // A global service no node is eligible for: no tasks, desired 0.
            ("glb_empty", with_desired("glb", SERVICE_MODE_GLOBAL, 0.0)),
            // A replicated service whose tasks were never created: no tasks, desired 2.
            (
                "rep_pending",
                with_desired("pending", SERVICE_MODE_REPLICATED, 2.0),
            ),
            // A service with a running task, to check it is left alone.
            ("rep_ok", with_desired("ok", SERVICE_MODE_REPLICATED, 1.0)),
        ],
        vec![],
    );
    let counters = poll(
        vec![placed_task("rep_ok", "", 1, at(0), "", "running")],
        &snapshot,
    )
    .await;
    assert_eq!(counters.len(), 3, "services without tasks included");
    publish(&state, &counters);
    let series = gathered(&state);
    let count = |name: &str| {
        series
            .keys()
            .filter(|id| id.starts_with(&format!("{name}{{")))
            .count()
    };
    assert_eq!(count(RUNNING_REPLICAS_NAME), 3);
    assert_eq!(count(AT_DESIRED_NAME), 3);
    assert_eq!(count(REPLICAS_STATE_NAME), 3 * KNOWN_TASK_STATES.len());
    for (service, mode, running, at_desired) in [
        ("glb", SERVICE_MODE_GLOBAL, 0.0, 1.0),
        ("pending", SERVICE_MODE_REPLICATED, 0.0, 0.0),
        ("ok", SERVICE_MODE_REPLICATED, 1.0, 1.0),
    ] {
        let labels = service_labels("s", service, mode);
        assert_eq!(
            series.get(&labelled(RUNNING_REPLICAS_NAME, &labels)),
            Some(&running),
            "{service}"
        );
        assert_eq!(
            series.get(&labelled(AT_DESIRED_NAME, &labels)),
            Some(&at_desired),
            "{service}"
        );
    }
}

/// Go: `TestAddServicesWithoutTasks`
#[test]
fn add_services_without_tasks_fills_only_missing_snapshot_services() {
    let snapshot = snapshot(
        &[(
            "svc_empty",
            test_metadata("s", "empty", SERVICE_MODE_REPLICATED),
        )],
        vec![],
    );
    let mut existing = TaskCounter::new(service_labels("s", "busy", SERVICE_MODE_REPLICATED));
    existing.inc("running");
    let mut counters = ServiceCounter::from([("svc_busy".to_owned(), existing)]);
    // svc_removed is not in the snapshot.
    let queried = ["svc_busy", "svc_empty", "svc_removed"].map(str::to_owned);
    add_services_without_tasks(&mut counters, &queried, &snapshot);
    assert_eq!(
        Some(counters["svc_busy"].state("running")),
        Some(1.0),
        "existing counter kept"
    );
    let empty = &counters["svc_empty"];
    assert!(empty.states.is_empty());
    assert_eq!(
        empty.labels.get(LABEL_SERVICE).map(String::as_str),
        Some("empty")
    );
    assert!(empty.desired_known);
    assert!(!counters.contains_key("svc_removed"));
}

/// One counter of service `name` in stack `s` with `running` running tasks and `desired`.
fn running_counter(name: &str, running: u32, desired: f64) -> TaskCounter {
    let mut counter = TaskCounter::new(service_labels("s", name, SERVICE_MODE_REPLICATED));
    counter.desired = desired;
    counter.desired_known = true;
    for _ in 0..running {
        counter.inc("running");
    }
    counter
}

/// Go's `replicasStateCounters`.
fn running_counters(running: u32, desired: f64, names: &[&str]) -> ServiceCounter {
    names
        .iter()
        .map(|name| {
            (
                format!("id_{name}"),
                running_counter(name, running, desired),
            )
        })
        .collect()
}

/// Go's `expectedReplicasStateSeries`: what `counters` publish, from a separate collector.
fn expected_series(counters: &ServiceCounter) -> BTreeMap<String, f64> {
    let reference = replicas_state();
    publish(&reference, counters);
    gathered(&reference)
}

/// Go: `TestUpdateReplicasStateGauge_AtDesired`
#[test]
fn running_equal_to_desired_is_at_desired() {
    let state = replicas_state();
    publish(
        &state,
        &ServiceCounter::from([("svc1".to_owned(), running_counter("sv", 3, 3.0))]),
    );
    let series = gathered(&state);
    let labels = service_labels("s", "sv", SERVICE_MODE_REPLICATED);
    assert_eq!(
        series.get(&labelled(RUNNING_REPLICAS_NAME, &labels)),
        Some(&3.0)
    );
    assert_eq!(series.get(&labelled(AT_DESIRED_NAME, &labels)), Some(&1.0));
}

/// Go: `TestUpdateReplicasStateGauge_NotAtDesired`
#[test]
fn running_below_desired_is_not_at_desired() {
    let state = replicas_state();
    publish(
        &state,
        &ServiceCounter::from([("svc1".to_owned(), running_counter("sv", 1, 3.0))]),
    );
    let labels = service_labels("s", "sv", SERVICE_MODE_REPLICATED);
    assert_eq!(
        gathered(&state).get(&labelled(AT_DESIRED_NAME, &labels)),
        Some(&0.0)
    );
}

/// Go: `TestUpdateReplicasStateGauge_MissingDesiredCache_NoPanic`: a counter not built from a
/// snapshot carries no desired replicas, publishes 0 and warns.
#[test]
fn missing_desired_replicas_publish_zero_and_warn() {
    let (logger, sink) = capture_logs(Level::INFO);
    let state = ReplicasState::new(&CustomLabels::default(), logger);
    let mut counter = TaskCounter::new(service_labels("s", "sv", SERVICE_MODE_REPLICATED));
    counter.inc("running");
    publish(
        &state,
        &ServiceCounter::from([("svc_nodesired".to_owned(), counter)]),
    );
    let labels = service_labels("s", "sv", SERVICE_MODE_REPLICATED);
    assert_eq!(
        gathered(&state).get(&labelled(AT_DESIRED_NAME, &labels)),
        Some(&0.0)
    );
    assert_eq!(
        sink.contents(),
        "level=WARN desired replicas missing from the polling snapshot service_id=svc_nodesired \
         stack=s service=sv mode=replicated running=1\n"
    );
}

/// Go: `TestServiceCounter_GetCreatesLazily`
#[test]
fn counter_for_creates_lazily() {
    let mut counters = ServiceCounter::new();
    let service = PollService::new(0, test_metadata("s", "web", SERVICE_MODE_REPLICATED));
    counter_for(&mut counters, "svc1", &service).inc("running");
    assert_eq!(
        Some(counter_for(&mut counters, "svc1", &service).state("running")),
        Some(1.0)
    );
    let _ = counter_for(&mut counters, "svc2", &service);
    assert_eq!(counters.len(), 2);
}

/// Go: `TestUpdateReplicasStateGauge_BuildDoesNotPublish` (#72): while the next set is built, a
/// scrape still sees the whole previous set.
#[test]
fn a_build_publishes_nothing_until_published() {
    let state = replicas_state();
    let counters_a = running_counters(1, 1.0, &["a1", "a2"]);
    let counters_b = running_counters(2, 3.0, &["b1"]);
    let (expected_a, expected_b) = (expected_series(&counters_a), expected_series(&counters_b));
    publish(&state, &counters_a);
    assert_eq!(gathered(&state), expected_a);
    let builder = state.build(&counters_b);
    assert_eq!(gathered(&state), expected_a, "after building B");
    let families = builder.build().unwrap();
    assert_eq!(gathered(&state), expected_a, "after build() of B");
    state.collector().publish(families);
    assert_eq!(gathered(&state), expected_b);
}

/// Go: `TestUpdateReplicasStateGauge_OnePublishUpdatesAllFamilies`
#[test]
fn one_publish_updates_all_families() {
    let state = replicas_state();
    publish(&state, &running_counters(1, 2.0, &["svc"]));
    publish(&state, &running_counters(2, 2.0, &["svc"]));
    let series = gathered(&state);
    let labels = service_labels("s", "svc", SERVICE_MODE_REPLICATED);
    for (id, want) in [
        (
            labelled(REPLICAS_STATE_NAME, &state_labels(&labels, "running")),
            2.0,
        ),
        (labelled(RUNNING_REPLICAS_NAME, &labels), 2.0),
        (labelled(AT_DESIRED_NAME, &labels), 1.0),
    ] {
        assert_eq!(series.get(&id), Some(&want), "{id}");
    }
}

/// A failed build publishes nothing and keeps the previous set.
#[test]
fn a_failed_build_keeps_the_previous_set() {
    let state = replicas_state();
    let counters = running_counters(1, 1.0, &["a"]);
    publish(&state, &counters);
    let mut broken = TaskCounter::new(BTreeMap::from([(LABEL_STACK.to_owned(), "s".to_owned())]));
    broken.desired_known = true;
    let error = state
        .update(&ServiceCounter::from([("broken".to_owned(), broken)]))
        .unwrap_err();
    assert!(
        error.starts_with(
            "build replicas state snapshot; keeping previous: label set does not match the \
             family's label names: "
        ),
        "{error}"
    );
    assert_eq!(gathered(&state), expected_series(&counters));
}

fn worker_and_manager_nodes(managers: usize, workers: usize) -> Vec<Node> {
    let node = |role: &str| Node {
        spec: NodeSpec {
            role: role.to_owned(),
            availability: "active".to_owned(),
            ..NodeSpec::default()
        },
        status: NodeStatus {
            state: "ready".to_owned(),
            ..NodeStatus::default()
        },
        ..Node::default()
    };
    let mut nodes: Vec<Node> = (0..managers).map(|_| node("manager")).collect();
    nodes.extend((0..workers).map(|_| node("worker")));
    nodes
}

fn node_series_id(role: &str) -> String {
    series_id(
        NODES_BY_STATE_NAME,
        &[
            (LABEL_NODE_ROLE, role),
            (LABEL_NODE_AVAILABILITY, "active"),
            (LABEL_NODE_STATUS, "ready"),
        ],
    )
}

/// Go: `TestSnapshotFamilies_ConcurrentScrapes_SeeWholeSnapshots`: updates and scrapes run
/// together on a multi-thread runtime, the replicas state from one updater as the poller
/// publishes it and the nodes from two as concurrent event workers do; every scrape sees one
/// whole snapshot. A partial publish is pinned by the build-does-not-publish tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_scrapes_see_whole_snapshots() {
    let state = Arc::new(replicas_state());
    let nodes = Arc::new(NodesByState::new(capture_logs(Level::INFO).0));
    let counters_a = Arc::new(running_counters(1, 1.0, &["a1", "a2", "a3"]));
    let counters_b = Arc::new(running_counters(2, 3.0, &["b1", "b2"]));
    let (expected_a, expected_b) = (expected_series(&counters_a), expected_series(&counters_b));
    let nodes_a = Arc::new(worker_and_manager_nodes(0, 3));
    let nodes_b = Arc::new(worker_and_manager_nodes(2, 1));
    let expected_nodes_a = BTreeMap::from([(node_series_id("worker"), 3.0)]);
    let expected_nodes_b = BTreeMap::from([
        (node_series_id("manager"), 2.0),
        (node_series_id("worker"), 1.0),
    ]);

    publish(&state, &counters_a);
    nodes.update_from_slice(&nodes_a);

    let stop = Arc::new(AtomicBool::new(false));
    let mut updaters = Vec::new();
    {
        let (state, stop) = (Arc::clone(&state), Arc::clone(&stop));
        let (counters_a, counters_b) = (Arc::clone(&counters_a), Arc::clone(&counters_b));
        updaters.push(tokio::spawn(async move {
            for index in 0_u64.. {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                publish(
                    &state,
                    if index % 2 == 0 {
                        &counters_b
                    } else {
                        &counters_a
                    },
                );
                tokio::task::yield_now().await;
            }
        }));
    }
    for _ in 0..2 {
        let (nodes, stop) = (Arc::clone(&nodes), Arc::clone(&stop));
        let (nodes_a, nodes_b) = (Arc::clone(&nodes_a), Arc::clone(&nodes_b));
        updaters.push(tokio::spawn(async move {
            for index in 0_u64.. {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                nodes.update_from_slice(if index % 2 == 0 { &nodes_b } else { &nodes_a });
                tokio::task::yield_now().await;
            }
        }));
    }

    for _ in 0..200 {
        let series = gathered(&state);
        assert!(
            series == expected_a || series == expected_b,
            "replicas scrape saw {series:?}, want exactly snapshot A or B"
        );
        let node_series = gather_series(Arc::clone(nodes.family()) as Arc<dyn Collector>);
        assert!(
            node_series == expected_nodes_a || node_series == expected_nodes_b,
            "nodes scrape saw {node_series:?}"
        );
        tokio::task::yield_now().await;
    }
    stop.store(true, Ordering::Relaxed);
    for updater in updaters {
        updater.await.unwrap();
    }
}

/// Go's `makeJobTask`: a task of job `service_id`, spawned by `iteration` (0 is no
/// `JobIteration`), created `slot` seconds after the base time.
fn make_job_task(
    service_id: &str,
    slot: i64,
    node_id: &str,
    iteration: u64,
    desired: &str,
    state: &str,
) -> Task {
    let mut task = placed_task(service_id, node_id, slot, at(slot), desired, state);
    task.version = Version::default();
    task.job_iteration = (iteration != 0).then_some(Version { index: iteration });
    task
}

/// Go's `seedJobMetadata`.
fn job_metadata(mode: &str, iteration: u64, desired: f64) -> ServiceMetadata {
    let mut metadata = test_metadata("s", "job", mode);
    metadata.job_iteration = iteration;
    metadata.desired_replicas = desired;
    metadata
}

/// Go's `drainedNode`.
fn drained_node(id: &str) -> Node {
    let mut node = schedulable_node(id, id);
    "drain".clone_into(&mut node.spec.availability);
    node
}

/// Go: `TestPollReplicasState_ReplicatedJob_DedupeBySlot`: 5 completions over 2 nodes, which
/// keyed by node would collapse into 2.
#[tokio::test]
async fn replicated_job_dedupes_by_slot() {
    let snapshot = snapshot(
        &[("job1", job_metadata(SERVICE_MODE_REPLICATED_JOB, 1, 0.0))],
        vec![],
    );
    let tasks = (0..5)
        .map(|slot| {
            let node = if slot % 2 == 0 { "n1" } else { "n2" };
            make_job_task("job1", slot, node, 1, "complete", "complete")
        })
        .collect();
    assert_eq!(
        Some(poll(tasks, &snapshot).await["job1"].state("complete")),
        Some(5.0)
    );
}

/// Go: `TestPollReplicasState_Job_OldIterationIgnored`
#[tokio::test]
async fn jobs_count_only_their_current_iteration() {
    let snapshot = snapshot(
        &[("job1", job_metadata(SERVICE_MODE_REPLICATED_JOB, 2, 0.0))],
        vec![],
    );
    let tasks = vec![
        // Previous run, marked for removal by Swarm.
        make_job_task("job1", 0, "n1", 1, "remove", "complete"),
        make_job_task("job1", 1, "n1", 1, "remove", "complete"),
        // A task without a JobIteration cannot be placed in any run.
        make_job_task("job1", 2, "n1", 0, "complete", "complete"),
        // Current run.
        make_job_task("job1", 0, "n2", 2, "complete", "running"),
    ];
    let counters = poll(tasks, &snapshot).await;
    assert_eq!(Some(counters["job1"].state("running")), Some(1.0));
    assert_eq!(Some(counters["job1"].state("complete")), Some(0.0));
}

/// Go: `TestPollReplicasState_GlobalJob_DedupeByNode`
#[tokio::test]
async fn global_job_dedupes_by_node() {
    let snapshot = snapshot(
        &[("gjob1", job_metadata(SERVICE_MODE_GLOBAL_JOB, 1, 0.0))],
        vec![],
    );
    let failed = make_job_task("gjob1", 0, "n1", 1, "shutdown", "failed");
    let mut restarted = make_job_task("gjob1", 0, "n1", 1, "complete", "complete");
    restarted.created_at = at(1);
    let other = make_job_task("gjob1", 0, "n2", 1, "complete", "complete");
    let counters = poll(vec![failed, restarted, other], &snapshot).await;
    assert_eq!(
        Some(counters["gjob1"].state("complete")),
        Some(2.0),
        "one per node"
    );
    assert_eq!(
        Some(counters["gjob1"].state("failed")),
        Some(0.0),
        "replaced on the same node"
    );
}

/// Go's `pollJobAtDesired`: polls `tasks` for job `job1`, publishes, and returns its at-desired
/// value and its count of `count_state`.
async fn poll_job_at_desired(
    state: &ReplicasState,
    snapshot: &PollSnapshot,
    mode: &str,
    tasks: Vec<Task>,
    count_state: &str,
) -> (f64, f64) {
    publish(state, &poll(tasks, snapshot).await);
    let series = gathered(state);
    let labels = service_labels("s", "job", mode);
    let at_desired = *series
        .get(&labelled(AT_DESIRED_NAME, &labels))
        .unwrap_or_else(|| panic!("at_desired series missing"));
    let count = *series
        .get(&labelled(
            REPLICAS_STATE_NAME,
            &state_labels(&labels, count_state),
        ))
        .unwrap_or_else(|| panic!("replicas_state{{state={count_state:?}}} series missing"));
    (at_desired, count)
}

/// One case of [`jobs_are_at_desired_once_finished`].
struct JobCase {
    name: &'static str,
    mode: &'static str,
    desired: f64,
    nodes: Vec<Node>,
    tasks: Vec<Task>,
    want: f64,
    count_state: &'static str,
    want_count: f64,
}

fn job_task(slot: i64, node: &str, desired: &str, state: &str) -> Task {
    make_job_task("job1", slot, node, 1, desired, state)
}

fn replicated_job_cases() -> Vec<JobCase> {
    let task = job_task;
    let replicated = SERVICE_MODE_REPLICATED_JOB;
    vec![
        JobCase {
            name: "replicated-job finished",
            mode: replicated,
            desired: 3.0,
            nodes: vec![],
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(1, "n2", "complete", "complete"),
                task(2, "n1", "complete", "complete"),
            ],
            want: 1.0,
            count_state: "complete",
            want_count: 3.0,
        },
        JobCase {
            name: "replicated-job in progress",
            mode: replicated,
            desired: 3.0,
            nodes: vec![],
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(1, "n2", "complete", "complete"),
                task(2, "n1", "complete", "running"),
            ],
            want: 0.0,
            count_state: "running",
            want_count: 1.0,
        },
        JobCase {
            name: "replicated-job with a failed task awaiting restart",
            mode: replicated,
            desired: 2.0,
            nodes: vec![],
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(1, "n2", "complete", "failed"),
            ],
            want: 0.0,
            count_state: "failed",
            want_count: 1.0,
        },
        JobCase {
            name: "replicated-job slot with exhausted restarts",
            mode: replicated,
            desired: 2.0,
            nodes: vec![],
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(1, "n2", "shutdown", "failed"),
            ],
            want: 0.0,
            count_state: "failed",
            want_count: 1.0,
        },
    ]
}

fn global_job_cases() -> Vec<JobCase> {
    let ready = || vec![schedulable_node("n1", "n1"), schedulable_node("n2", "n2")];
    let task = job_task;
    let global = SERVICE_MODE_GLOBAL_JOB;
    let mut with_drained = ready();
    with_drained.push(drained_node("n3"));
    vec![
        JobCase {
            name: "global-job finished",
            mode: global,
            desired: 2.0,
            nodes: ready(),
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(0, "n2", "complete", "complete"),
            ],
            want: 1.0,
            count_state: "complete",
            want_count: 2.0,
        },
        JobCase {
            name: "global-job without tasks and no eligible node",
            mode: global,
            desired: 0.0,
            nodes: vec![drained_node("n1")],
            tasks: vec![],
            want: 0.0,
            count_state: "complete",
            want_count: 0.0,
        },
        JobCase {
            name: "global-job finished with a retired task on a drained node",
            mode: global,
            desired: 2.0,
            nodes: with_drained,
            tasks: vec![
                task(0, "n1", "complete", "complete"),
                task(0, "n2", "complete", "complete"),
                // Swarm shut the task down when n3 was drained; its node still reports it
                // running, so it is not counted at all.
                task(0, "n3", "shutdown", "running"),
            ],
            want: 1.0,
            count_state: "running",
            want_count: 0.0,
        },
        JobCase {
            name: "global-job finished whose last eligible node was drained",
            mode: global,
            desired: 0.0,
            nodes: vec![drained_node("n1")],
            tasks: vec![task(0, "n1", "complete", "complete")],
            want: 1.0,
            count_state: "complete",
            want_count: 1.0,
        },
        JobCase {
            name: "global-job with an empty node cache",
            mode: global,
            desired: 1.0,
            nodes: vec![],
            tasks: vec![task(0, "n1", "complete", "complete")],
            want: 0.0,
            count_state: "complete",
            want_count: 1.0,
        },
    ]
}

/// Go: `TestUpdateReplicasStateGauge_JobAtDesired`
#[tokio::test]
async fn jobs_are_at_desired_once_finished() {
    for case in replicated_job_cases().into_iter().chain(global_job_cases()) {
        let state = replicas_state();
        let snapshot = snapshot(
            &[("job1", job_metadata(case.mode, 1, case.desired))],
            case.nodes,
        );
        let (at_desired, count) =
            poll_job_at_desired(&state, &snapshot, case.mode, case.tasks, case.count_state).await;
        assert_eq!(
            Some(at_desired),
            Some(case.want),
            "{}: at_desired",
            case.name
        );
        assert_eq!(
            Some(count),
            Some(case.want_count),
            "{}: {}",
            case.name,
            case.count_state
        );
    }
}

/// Go: `TestUpdateReplicasStateGauge_GlobalJob_NodeReplacement`: completion goes by node
/// identity, so drained node A's completed task does not stand in for node B, eligible since
/// and without a task, even though the counts match.
#[tokio::test]
async fn a_global_jobs_completion_goes_by_node_identity() {
    let state = replicas_state();
    let snapshot = snapshot(
        &[("job1", job_metadata(SERVICE_MODE_GLOBAL_JOB, 1, 1.0))],
        vec![drained_node("nA"), schedulable_node("nB", "nB")],
    );
    let mut tasks = vec![make_job_task("job1", 0, "nA", 1, "complete", "complete")];
    let (at_desired, _) = poll_job_at_desired(
        &state,
        &snapshot,
        SERVICE_MODE_GLOBAL_JOB,
        tasks.clone(),
        "complete",
    )
    .await;
    assert_eq!(Some(at_desired), Some(0.0), "node B has no completed task");
    tasks.push(make_job_task("job1", 0, "nB", 1, "complete", "complete"));
    let (at_desired, _) = poll_job_at_desired(
        &state,
        &snapshot,
        SERVICE_MODE_GLOBAL_JOB,
        tasks,
        "complete",
    )
    .await;
    assert_eq!(Some(at_desired), Some(1.0), "node B completed");
}

fn mismatch_lines(contents: &str) -> Vec<&str> {
    contents
        .lines()
        .filter(|line| line.contains("at_desired mismatch"))
        .collect()
}

/// The mismatch log is keyed as Go's: a repeat of the stored key is not logged, and since Go
/// stores with `LoadOrStore`, only the first key is kept, so a different mismatch logs each
/// time; forgetting the service starts over.
#[test]
fn mismatch_logs_follow_gos_first_stored_key() {
    let (logger, sink) = capture_logs(Level::DEBUG);
    let state = ReplicasState::new(&CustomLabels::default(), logger);
    let publish_running = |running| {
        publish(
            &state,
            &ServiceCounter::from([("svc1".to_owned(), running_counter("sv", running, 3.0))]),
        );
    };
    publish_running(1);
    publish_running(1);
    assert_eq!(
        mismatch_lines(&sink.contents()).len(),
        1,
        "a repeat is not logged"
    );
    publish_running(2);
    publish_running(2);
    assert_eq!(
        mismatch_lines(&sink.contents()).len(),
        3,
        "only the first key is stored"
    );
    publish_running(3);
    publish_running(1);
    assert_eq!(
        mismatch_lines(&sink.contents()).len(),
        3,
        "at desired, then the stored key"
    );
    state.forget_service("svc1");
    publish_running(1);
    assert_eq!(
        mismatch_lines(&sink.contents()).len(),
        4,
        "a forgotten service logs again"
    );
    assert_eq!(
        mismatch_lines(&sink.contents())[0],
        "level=DEBUG at_desired mismatch service_id=svc1 stack=s service=sv mode=replicated \
         running=1 desired=3"
    );
}

/// A job's mismatch key and log carry the wanted counts and the coverage too.
#[test]
fn job_mismatch_logs_carry_the_job_counts() {
    let (logger, sink) = capture_logs(Level::DEBUG);
    let state = ReplicasState::new(&CustomLabels::default(), logger);
    let mut counter = TaskCounter::new(service_labels("s", "job", SERVICE_MODE_GLOBAL_JOB));
    counter.desired = 2.0;
    counter.desired_known = true;
    counter.wanted = 2.0;
    counter.wanted_complete = 1.0;
    publish(
        &state,
        &ServiceCounter::from([("job1".to_owned(), counter.clone())]),
    );
    publish(
        &state,
        &ServiceCounter::from([("job1".to_owned(), counter.clone())]),
    );
    counter.wanted_complete = 2.0;
    publish(
        &state,
        &ServiceCounter::from([("job1".to_owned(), counter)]),
    );
    let contents = sink.contents();
    let lines = mismatch_lines(&contents);
    assert_eq!(lines.len(), 2, "the job counts are part of the key");
    assert_eq!(
        lines[0],
        "level=DEBUG at_desired mismatch service_id=job1 stack=s service=job mode=global-job \
         running=0 desired=2 wanted=2 wanted_complete=1 eligible_nodes_covered=false"
    );
}

/// The families keep Go's names, HELP texts and label order, the custom labels after the base
/// labels and after `state`.
#[test]
fn families_carry_go_names_help_and_label_order() {
    let custom = CustomLabels::new(&["team".to_owned()], &["team".to_owned()]);
    let state = ReplicasState::new(&custom, capture_logs(Level::INFO).0);
    let descs = state.collector().descs();
    let summary: Vec<(&str, &str, Vec<&str>)> = descs
        .iter()
        .map(|desc| {
            (
                desc.name(),
                desc.help(),
                desc.label_names().iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let base = ["stack", "service", "service_mode", "display_name"];
    assert_eq!(
        summary,
        vec![
            (
                REPLICAS_STATE_NAME,
                REPLICAS_STATE_HELP,
                [&base[..], &["state", "team"]].concat()
            ),
            (
                RUNNING_REPLICAS_NAME,
                RUNNING_REPLICAS_HELP,
                [&base[..], &["team"]].concat()
            ),
            (
                AT_DESIRED_NAME,
                AT_DESIRED_HELP,
                [&base[..], &["team"]].concat()
            ),
        ]
    );
    let registry = Registry::new();
    state.register(&registry).unwrap();
    assert!(state.register(&registry).is_err());
}
