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

//! `swarm_task_replicas_state`, `swarm_service_running_replicas` and `swarm_service_at_desired`:
//! the tasks of each service counted by state, from one task list per poll.
//!
//! Only the current task is counted: one per (service, slot) for replicated services and
//! replicated jobs, one per (service, node) for global services and global jobs, so the tasks of
//! earlier rollouts do not inflate the counts. Jobs count only the tasks of their current
//! iteration. The current task is the one Swarm still wants, preferring a running one, then the
//! newest ([`preferred_task`]). The three families share one snapshot, so a scrape never pairs
//! the running replicas of one poll with the at-desired value of another.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use crate::docker::types::{Node, Placement, Task};
use crate::docker::{DockerApi, with_docker_timeout};
use crate::labels::{sanitize_label_names, sanitize_metric_labels};
use crate::logger::{Attr, Logger};
use crate::metrics::{
    BuildError, Collector, Desc, Kind, RegisterError, Registry, SnapshotBuilder, SnapshotCollector,
    constant_desc,
};

use super::desired::{node_eligible, placement_from_metadata};
use super::error::CallError;
use super::metrics_ids::{
    LABEL_DISPLAY_NAME, LABEL_SERVICE, LABEL_SERVICE_MODE, LABEL_STACK, LABEL_STATE, NAMESPACE,
    SERVICE_MODE_GLOBAL_JOB, SERVICE_MODE_REPLICATED, SERVICE_MODE_REPLICATED_JOB,
    SERVICE_SUBSYSTEM, TASK_SUBSYSTEM,
};
use super::poll_snapshot::{PollService, PollSnapshot};
use super::types::{CustomLabels, ServiceMetadata, is_job_mode};

/// Every Swarm task state, emitted for each service with 0 where absent.
pub const KNOWN_TASK_STATES: [&str; 15] = [
    "new",
    "allocated",
    "pending",
    "assigned",
    "accepted",
    "preparing",
    "ready",
    "starting",
    "running",
    "complete",
    "shutdown",
    "failed",
    "rejected",
    "remove",
    "orphaned",
];

const TASK_STATE_RUNNING: &str = "running";
const TASK_STATE_COMPLETE: &str = "complete";

/// The desired states of a task Swarm no longer wants running, swarmkit's restart supervisor's
/// `DesiredState > Completed`: a task in one has been shut down or replaced, even if its node
/// still reports it running (a down node's tasks keep their last status for up to 24h).
const RETIRED_DESIRED_STATES: [&str; 5] = ["shutdown", "failed", "rejected", "remove", "orphaned"];

/// The reported states of a task that stopped, or never runs again. Any other state, an empty
/// or unknown one included, is not stopped.
const STOPPED_TASK_STATES: [&str; 6] = [
    "complete", "shutdown", "failed", "rejected", "remove", "orphaned",
];

/// The HELP text of `swarm_task_replicas_state`.
pub const REPLICAS_STATE_HELP: &str = "Number of tasks per Swarm service segmented by task state \
    (current task per slot for replicated services and replicated jobs or node for global \
    services and global jobs; jobs count only their current iteration).";
/// The HELP text of `swarm_service_running_replicas`.
pub const RUNNING_REPLICAS_HELP: &str = "Current number of running tasks per Swarm service \
    (current task per slot for replicated services and replicated jobs or node for global \
    services and global jobs; jobs count only their current iteration).";
/// The HELP text of `swarm_service_at_desired`.
pub const AT_DESIRED_HELP: &str = "Service is at desired replicas (1) or not (0). Jobs are at \
    desired once finished: every task of the current iteration completed.";

/// The task counts of one service.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskCounter {
    /// The count per task state; a state never seen is absent.
    pub states: HashMap<String, f64>,
    /// The service's label identity.
    pub labels: BTreeMap<String, String>,
    /// The counted tasks Swarm still wants (not [`retired_task`]).
    pub wanted: f64,
    /// Those of them that are complete. A job is finished when the two are equal.
    pub wanted_complete: f64,
    /// Global jobs only: every node eligible now has a wanted, completed task of the current
    /// iteration.
    pub eligible_nodes_covered: bool,
    /// The service's desired replicas in the snapshot the tasks were counted against.
    pub desired: f64,
    /// False when the counter was built without a desired value.
    pub desired_known: bool,
}

impl TaskCounter {
    /// An empty counter with `labels`; only the states seen are recorded, the zeros are added at
    /// publish time.
    #[must_use]
    pub fn new(labels: BTreeMap<String, String>) -> Self {
        Self {
            labels,
            ..Self::default()
        }
    }

    /// An empty counter with the label identity and desired replicas of `service`, as the
    /// snapshot recorded them.
    #[must_use]
    pub fn for_service(service: &PollService) -> Self {
        Self {
            desired: service.metadata.desired_replicas,
            desired_known: true,
            ..Self::new(service.labels.clone())
        }
    }

    /// Counts one task in `state`.
    pub fn inc(&mut self, state: &str) {
        *self.states.entry(state.to_owned()).or_insert(0.0) += 1.0;
    }

    /// The count of `state`, 0 when never seen.
    #[must_use]
    pub fn state(&self, state: &str) -> f64 {
        self.states.get(state).copied().unwrap_or(0.0)
    }

    fn mode(&self) -> &str {
        self.labels
            .get(LABEL_SERVICE_MODE)
            .map_or("", String::as_str)
    }
}

/// The counters of a poll, by service ID (unique, unlike the visible names).
pub type ServiceCounter = HashMap<String, TaskCounter>;

/// The counter of `service_id`, created from `service` when missing.
pub fn counter_for<'counters>(
    counters: &'counters mut ServiceCounter,
    service_id: &str,
    service: &PollService,
) -> &'counters mut TaskCounter {
    counters
        .entry(service_id.to_owned())
        .or_insert_with(|| TaskCounter::for_service(service))
}

/// A deduplication group: (service, slot) for replicated modes, (service, node) for global ones.
/// Swarm gives each task of a replicated job its own slot and each task of a global job its own
/// node, so the same keys hold for jobs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LatestKey<'task> {
    service_id: &'task str,
    slot: i64,
    node_id: &'task str,
}

/// Lists the cluster's tasks and counts those of the snapshot's services by state: the current
/// task per dedupe key ([`preferred_task`]), skipping tasks of an earlier job iteration and
/// tasks Swarm retired that have not reported stopping ([`retired_not_stopped`]). Everything but
/// the task list comes from `snapshot`, so the counts and the metadata they are judged against
/// describe the same moment; a task of a service not in the snapshot is skipped. Every service
/// of the snapshot gets a counter.
///
/// # Errors
///
/// `task list: …` when the task list fails.
pub async fn poll_replicas_state(
    docker: &impl DockerApi,
    snapshot: &PollSnapshot,
) -> Result<ServiceCounter, CallError> {
    let mut counters = ServiceCounter::new();
    // An empty snapshot has nothing to count: no task list at all.
    let service_ids = snapshot.service_ids();
    if service_ids.is_empty() {
        return Ok(counters);
    }
    let tasks = list_all_tasks(docker).await?;

    // Step 1: the current task per dedupe key.
    let mut latest: HashMap<LatestKey<'_>, &Task> = HashMap::new();
    for task in &tasks {
        let Some(service) = snapshot.services.get(&task.service_id) else {
            continue;
        };
        if retired_not_stopped(task) {
            continue;
        }
        let Some(key) = dedupe_key_for_task(task, &service.metadata) else {
            continue;
        };
        let replace = latest
            .get(&key)
            .is_none_or(|previous| preferred_task(task, previous));
        if replace {
            latest.insert(key, task);
        }
    }

    // Step 2: the chosen tasks per service, by state. The nodes with a wanted completed task of
    // each global job live for this poll only.
    let mut completed_nodes: HashMap<String, HashSet<String>> = HashMap::new();
    for (key, task) in &latest {
        let Some(service) = snapshot.services.get(key.service_id) else {
            continue;
        };
        let counter = counter_for(&mut counters, key.service_id, service);
        counter.inc(&task.status.state);
        count_wanted(counter, task, &mut completed_nodes);
    }

    mark_eligible_nodes_covered(&mut counters, &completed_nodes, snapshot);
    add_services_without_tasks(&mut counters, &service_ids, snapshot);
    Ok(counters)
}

/// Every task of the cluster in one call, under one request deadline. No service filter on
/// purpose: the IDs would travel in the query, about 41 bytes each, and a socket proxy rejects
/// the URL long before the cluster is large (`HAProxy`'s 16 KB buffer at about 400 services); a
/// filter would not save work either, since a poll queries every service of its snapshot.
async fn list_all_tasks(docker: &impl DockerApi) -> Result<Vec<Task>, CallError> {
    with_docker_timeout(docker.task_list())
        .await
        .map_err(|error| CallError::new("task list", error))
}

/// The key `task` is counted under, or none when it belongs to an earlier run of a job.
fn dedupe_key_for_task<'task>(
    task: &'task Task,
    metadata: &ServiceMetadata,
) -> Option<LatestKey<'task>> {
    // A job's earlier runs leave their tasks behind; only the current iteration describes it.
    if is_job_mode(&metadata.service_mode)
        && task
            .job_iteration
            .as_ref()
            .is_none_or(|iteration| iteration.index != metadata.job_iteration)
    {
        return None;
    }
    let mode = metadata.service_mode.as_str();
    if mode == SERVICE_MODE_REPLICATED || mode == SERVICE_MODE_REPLICATED_JOB {
        return Some(LatestKey {
            service_id: &task.service_id,
            slot: task.slot,
            node_id: "",
        });
    }
    // Global services and global jobs have no slots.
    Some(LatestKey {
        service_id: &task.service_id,
        slot: 0,
        node_id: &task.node_id,
    })
}

/// Adds `task` to the wanted counts when Swarm still wants it, and records the node of a wanted
/// completed global-job task.
fn count_wanted(
    counter: &mut TaskCounter,
    task: &Task,
    completed_nodes: &mut HashMap<String, HashSet<String>>,
) {
    if retired_task(task) {
        return;
    }
    counter.wanted += 1.0;
    if task.status.state != TASK_STATE_COMPLETE {
        return;
    }
    counter.wanted_complete += 1.0;
    if counter.mode() == SERVICE_MODE_GLOBAL_JOB {
        completed_nodes
            .entry(task.service_id.clone())
            .or_default()
            .insert(task.node_id.clone());
    }
}

/// Sets `eligible_nodes_covered` on every global-job counter, comparing the snapshot's eligible
/// nodes with the nodes the job completed on, by identity: when node A completed and became
/// ineligible while node B became eligible, A's completion must not stand in for B.
fn mark_eligible_nodes_covered(
    counters: &mut ServiceCounter,
    completed_nodes: &HashMap<String, HashSet<String>>,
    snapshot: &PollSnapshot,
) {
    for (service_id, counter) in counters.iter_mut() {
        if counter.mode() != SERVICE_MODE_GLOBAL_JOB {
            continue;
        }
        let placement = snapshot
            .services
            .get(service_id)
            .and_then(|service| placement_from_metadata(&service.metadata));
        let empty = HashSet::new();
        counter.eligible_nodes_covered = eligible_nodes_covered(
            &snapshot.nodes,
            placement.as_ref(),
            completed_nodes.get(service_id).unwrap_or(&empty),
        );
    }
}

/// Whether every node of `nodes` eligible for `placement` is in `completed_nodes`. No nodes at
/// all (not seeded yet) is false, so a global job is not taken for finished before the nodes are
/// known.
fn eligible_nodes_covered(
    nodes: &[Node],
    placement: Option<&Placement>,
    completed_nodes: &HashSet<String>,
) -> bool {
    !nodes.is_empty()
        && nodes
            .iter()
            .filter(|node| node_eligible(Some(node), placement))
            .all(|node| completed_nodes.contains(&node.id))
}

/// Gives every queried service without tasks an empty counter, so it still emits
/// `running_replicas` 0, zeroed task states and an at-desired series; without it a service never
/// scheduled would have no series, and an `at_desired == 0` alert would never fire for it.
pub fn add_services_without_tasks(
    counters: &mut ServiceCounter,
    queried_service_ids: &[String],
    snapshot: &PollSnapshot,
) {
    for service_id in queried_service_ids {
        if counters.contains_key(service_id) {
            continue;
        }
        if let Some(service) = snapshot.services.get(service_id) {
            counters.insert(service_id.clone(), TaskCounter::for_service(service));
        }
    }
}

/// Whether `candidate` replaces `current` as the counted task of a key; the first rule that
/// tells them apart decides, and each is symmetric, so Docker's list order does not matter:
/// 1. a task Swarm still wants beats a retired one (after a failed start-first update the newer,
///    failed task is retired while the older one serves);
/// 2. a running task beats one that is not (during a start-first update the old task serves
///    until its replacement runs);
/// 3. the newer task wins ([`newer_than`]).
#[must_use]
pub fn preferred_task(candidate: &Task, current: &Task) -> bool {
    let candidate_retired = retired_task(candidate);
    if candidate_retired != retired_task(current) {
        return !candidate_retired;
    }
    let candidate_running = candidate.status.state == TASK_STATE_RUNNING;
    if candidate_running != (current.status.state == TASK_STATE_RUNNING) {
        return candidate_running;
    }
    newer_than(candidate, current)
}

/// Whether Swarm no longer wants `task` running; an empty desired state is wanted.
#[must_use]
pub fn retired_task(task: &Task) -> bool {
    RETIRED_DESIRED_STATES.contains(&task.desired_state.as_str())
}

/// Whether Swarm retired `task` but it has not reported stopping: its node is down and still
/// shows its last state, or a live node is stopping it. Neither is a replica Swarm runs, so the
/// task is not counted at all.
#[must_use]
pub fn retired_not_stopped(task: &Task) -> bool {
    retired_task(task) && !STOPPED_TASK_STATES.contains(&task.status.state.as_str())
}

/// Whether `candidate` is strictly newer than `current`: by creation time (stable across status
/// updates, so a late shutdown status does not win), then by status timestamp when both have
/// one, then by version index.
#[must_use]
pub fn newer_than(candidate: &Task, current: &Task) -> bool {
    if candidate.created_at != current.created_at {
        return candidate.created_at > current.created_at;
    }
    let (candidate_status, current_status) = (candidate.status.timestamp, current.status.timestamp);
    if !candidate_status.is_zero() && !current_status.is_zero() {
        return candidate_status > current_status;
    }
    candidate.version.index > current.version.index
}

/// Whether a job finished, from the tasks of its current iteration. Only tasks Swarm still wants
/// count: a failed task is shut down before its replacement, and a global job's task when its
/// node goes down or is drained, so a retired task in the total would hold a finished job at 0.
/// A slot whose restarts are exhausted keeps a retired failed task and no replacement, so a
/// replicated job stays below its total completions.
fn job_at_desired(mode: &str, counter: &TaskCounter, desired: f64) -> bool {
    // Every wanted task is complete: none pending, running, or failed awaiting a restart.
    let all_wanted_complete = counter.wanted_complete.eq(&counter.wanted);
    if mode == SERVICE_MODE_REPLICATED_JOB {
        // desired (total completions) is at least 1, so this also requires a task.
        return all_wanted_complete && counter.wanted_complete >= desired;
    }
    // Global job: a job with no eligible node never runs and stays at 0, while a finished one
    // whose last eligible node is drained keeps its completed tasks and stays at 1 with desired
    // at 0. Covering every eligible node implies wanted_complete >= desired.
    counter.wanted > 0.0 && all_wanted_complete && counter.eligible_nodes_covered
}

/// The three families, published as one snapshot.
#[derive(Debug)]
pub struct ReplicasState {
    collector: Arc<SnapshotCollector>,
    state_desc: Arc<Desc>,
    running_desc: Arc<Desc>,
    at_desired_desc: Arc<Desc>,
    logger: Logger,
    // The first at-desired mismatch logged per service; the reconciler forgets a removed
    // service.
    mismatch_logged: Mutex<HashMap<String, String>>,
}

impl ReplicasState {
    /// The families, with the custom labels after the base labels (after `state` on the state
    /// family), logging to `logger`, with nothing published.
    #[must_use]
    pub fn new(custom_labels: &CustomLabels, logger: Logger) -> Self {
        let base = [
            LABEL_STACK,
            LABEL_SERVICE,
            LABEL_SERVICE_MODE,
            LABEL_DISPLAY_NAME,
        ];
        let custom = custom_labels.sanitized_names();
        let names = |with_state: bool| {
            let mut names: Vec<String> = base.iter().map(|&name| name.to_owned()).collect();
            if with_state {
                names.push(LABEL_STATE.to_owned());
            }
            names.extend(custom.iter().cloned());
            sanitize_label_names(&names)
        };
        let desc = |subsystem: &str, name: &str, help: &str, labels: &[String]| {
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            constant_desc(
                &format!("{NAMESPACE}_{subsystem}_{name}"),
                help,
                Kind::Gauge,
                &labels,
            )
        };
        let state_desc = desc(
            TASK_SUBSYSTEM,
            "replicas_state",
            REPLICAS_STATE_HELP,
            &names(true),
        );
        let running_desc = desc(
            SERVICE_SUBSYSTEM,
            "running_replicas",
            RUNNING_REPLICAS_HELP,
            &names(false),
        );
        let at_desired_desc = desc(
            SERVICE_SUBSYSTEM,
            "at_desired",
            AT_DESIRED_HELP,
            &names(false),
        );
        Self {
            collector: Arc::new(SnapshotCollector::new(vec![
                Arc::clone(&state_desc),
                Arc::clone(&running_desc),
                Arc::clone(&at_desired_desc),
            ])),
            state_desc,
            running_desc,
            at_desired_desc,
            logger,
            mismatch_logged: Mutex::new(HashMap::new()),
        }
    }

    /// Registers the three families.
    ///
    /// # Errors
    ///
    /// The registry's refusal.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        registry.register(Arc::clone(&self.collector) as Arc<dyn Collector>)
    }

    /// The snapshot collector.
    #[must_use]
    pub fn collector(&self) -> &Arc<SnapshotCollector> {
        &self.collector
    }

    /// Publishes `counters` as the three families, built whole first and swapped in at once, so
    /// services that disappeared are dropped without a scrape seeing a half-built set.
    ///
    /// # Errors
    ///
    /// `build replicas state snapshot; keeping previous: …` when the build fails, in which case
    /// nothing was published and the poll must not count as a success.
    pub fn update(&self, counters: &ServiceCounter) -> Result<(), String> {
        let families = self.build(counters).build().map_err(|error: BuildError| {
            format!("build replicas state snapshot; keeping previous: {error}")
        })?;
        self.collector.publish(families);
        Ok(())
    }

    /// Forgets the last mismatch logged for a removed service.
    pub fn forget_service(&self, service_id: &str) {
        self.mismatch_logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(service_id);
    }

    /// The series of every service of `counters`, for all three families, without publishing
    /// them: every known state, 0 where absent.
    #[must_use]
    pub fn build(&self, counters: &ServiceCounter) -> SnapshotBuilder {
        let mut builder = SnapshotBuilder::new();
        for (service_id, counter) in counters {
            let base = sanitize_metric_labels(counter.labels.clone());
            for state in KNOWN_TASK_STATES {
                let mut labels = base.clone();
                labels.insert(LABEL_STATE.to_owned(), state.to_owned());
                let labels: Vec<(&String, &String)> = labels.iter().collect();
                builder.set(&self.state_desc, &labels, counter.state(state));
            }
            let pairs: Vec<(&String, &String)> = base.iter().collect();
            builder.set(
                &self.running_desc,
                &pairs,
                counter.state(TASK_STATE_RUNNING),
            );
            let at_desired = self.at_desired(service_id, &base, counter);
            builder.set(&self.at_desired_desc, &pairs, at_desired);
        }
        builder
    }

    /// The at-desired value of one service, logging why when it cannot be or is not reached.
    fn at_desired(
        &self,
        service_id: &str,
        base: &BTreeMap<String, String>,
        counter: &TaskCounter,
    ) -> f64 {
        let label = |name: &str| base.get(name).map_or("", String::as_str);
        let running = counter.state(TASK_STATE_RUNNING);
        if !counter.desired_known {
            self.logger.warn(
                "desired replicas missing from the polling snapshot",
                &[
                    Attr::string("service_id", service_id),
                    Attr::string(LABEL_STACK, label(LABEL_STACK)),
                    Attr::string(LABEL_SERVICE, label(LABEL_SERVICE)),
                    Attr::string("mode", label(LABEL_SERVICE_MODE)),
                    Attr::float("running", running),
                ],
            );
            return 0.0;
        }
        let mode = label(LABEL_SERVICE_MODE);
        let desired = counter.desired;
        let reached = if is_job_mode(mode) {
            job_at_desired(mode, counter, desired)
        } else {
            running.eq(&desired)
        };
        if reached {
            return 1.0;
        }
        self.log_mismatch_once_per_change(service_id, base, running, desired, counter);
        0.0
    }

    /// Logs an at-desired mismatch at debug unless it repeats the service's stored one, keyed as
    /// Go's `%.0f|%.0f` (plus `|%.0f|%.0f|%t` for jobs). As Go's `sync.Map.LoadOrStore`, only
    /// the first key is stored: a later, different mismatch is logged every time it recurs.
    fn log_mismatch_once_per_change(
        &self,
        service_id: &str,
        base: &BTreeMap<String, String>,
        running: f64,
        desired: f64,
        counter: &TaskCounter,
    ) {
        let label = |name: &str| base.get(name).map_or("", String::as_str);
        let mode = label(LABEL_SERVICE_MODE);
        let job = is_job_mode(mode);
        let key = if job {
            format!(
                "{running:.0}|{desired:.0}|{:.0}|{:.0}|{}",
                counter.wanted, counter.wanted_complete, counter.eligible_nodes_covered
            )
        } else {
            format!("{running:.0}|{desired:.0}")
        };
        let mut attrs = vec![
            Attr::string("service_id", service_id),
            Attr::string(LABEL_STACK, label(LABEL_STACK)),
            Attr::string(LABEL_SERVICE, label(LABEL_SERVICE)),
            Attr::string("mode", mode),
            Attr::float("running", running),
            Attr::float("desired", desired),
        ];
        if job {
            attrs.extend([
                Attr::float("wanted", counter.wanted),
                Attr::float("wanted_complete", counter.wanted_complete),
                Attr::bool("eligible_nodes_covered", counter.eligible_nodes_covered),
            ]);
        }
        let mut logged = self
            .mismatch_logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if logged.get(service_id) == Some(&key) {
            return;
        }
        logged.entry(service_id.to_owned()).or_insert(key);
        drop(logged);
        self.logger.debug("at_desired mismatch", &attrs);
    }
}

#[cfg(test)]
mod tests;
