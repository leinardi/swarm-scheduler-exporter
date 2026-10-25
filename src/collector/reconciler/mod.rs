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

//! The reconciler owns the exporter's view of Swarm. Its task makes every write to the service
//! metadata cache, the node cache, the desired / schedulable / update-state families and the
//! nodes-by-state family; owning the caches outright, it needs no lock for them. The event side
//! only marks keys dirty through a [`ReconcilerHandle`], under one short lock and without
//! blocking, so the Docker event stream is always drained; the reconciler then inspects what
//! changed. A full resync (service list, then node list) seeds the caches at startup, backs the
//! event stream up periodically, and recovers whatever the dirty set had to drop.
//!
//! Go guards the dirty set, the generations and the resync counters with `Reconciler.mu`, taken
//! before `metadataMu` when an overflow prunes the generations of uncached services. Here the
//! cache's key set rides in the generation entries ([`Generation::cached`]), so the two locks
//! become the one [`Shared`] lock, never held across an await.

mod recover;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::docker::types::{Event, Node, Service};
use crate::docker::{DockerApi, with_docker_timeout};
use crate::logger::{Attr, Logger};

use super::desired::{ReplicaGauges, labels_for_metadata, replica_counts};
use super::error::CallError;
use super::exporter_metrics::{ExporterMetrics, ResyncResult};
use super::must;
use super::nodes::NodesByState;
use super::replicas_state::ReplicasState;
use super::service_update::ServiceUpdateMetrics;
use super::types::{CustomLabels, MetadataCache, NodeCache, ServiceMetadata, build_metadata};

use self::recover::{PanicReport, recover};

/// The dirty service keys kept at most. Past it the set is dropped and a resync requested
/// instead: a resync costs one service list, whatever the number of changes.
pub const PENDING_KEY_CAP: usize = 4096;

/// The service inspects of one cycle at most, so due node refreshes and resyncs are not starved
/// by a burst of service events.
pub const SERVICE_KEYS_PER_CYCLE: usize = 32;

/// How often a full resync runs even when no event asked for one. It covers what the event
/// stream cannot: a silent stream, clock skew between the exporter and the daemon, and events
/// the daemon no longer has when the stream reconnects.
pub const PERIODIC_RESYNC_INTERVAL: Duration = Duration::from_mins(5);

/// The waits before retrying a service whose inspect failed with an error other than not-found.
/// A failure with no delay left drops the key and requests a resync, which picks the service up
/// with everything else.
pub const SERVICE_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// The first retry delay of a failing resync or node refresh.
pub const BACKOFF_INITIAL_DELAY: Duration = Duration::from_millis(500);
/// The cap of the exponential backoff.
pub const BACKOFF_MAX_DELAY: Duration = Duration::from_secs(30);
/// The backoff's growth per failure.
const BACKOFF_MULTIPLIER: u32 = 2;

/// The step name of the resync, which counts a panic in it as a failed resync.
const RESYNC_STEP: &str = "resync";

const EVENT_TYPE_NODE: &str = "node";
const EVENT_TYPE_SERVICE: &str = "service";

/// The reconciler's timing policy, Go's fields that tests shorten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilerPolicy {
    /// The dirty-set cap.
    pub pending_cap: usize,
    /// The service retry delays.
    pub retry_delays: Vec<Duration>,
    /// The first backoff of a failing resync or node refresh.
    pub backoff_initial: Duration,
    /// The backoff cap.
    pub backoff_max: Duration,
    /// The periodic resync interval.
    pub resync_interval: Duration,
}

impl Default for ReconcilerPolicy {
    fn default() -> Self {
        Self {
            pending_cap: PENDING_KEY_CAP,
            retry_delays: SERVICE_RETRY_DELAYS.to_vec(),
            backoff_initial: BACKOFF_INITIAL_DELAY,
            backoff_max: BACKOFF_MAX_DELAY,
            resync_interval: PERIODIC_RESYNC_INTERVAL,
        }
    }
}

/// The families the reconciler writes, shared with the registry and the poller.
#[derive(Debug, Clone)]
pub struct Collectors {
    /// The custom labels every per-service family carries.
    pub custom_labels: CustomLabels,
    /// `swarm_service_desired_replicas` and `swarm_service_schedulable_replicas`.
    pub replica_gauges: Arc<ReplicaGauges>,
    /// The service update families.
    pub service_update: Arc<ServiceUpdateMetrics>,
    /// `swarm_cluster_nodes_by_state`.
    pub nodes_by_state: Arc<NodesByState>,
    /// The replicas-state families, whose mismatch log forgets a removed service.
    pub replicas_state: Arc<ReplicasState>,
    /// The self-observability families.
    pub metrics: Arc<ExporterMetrics>,
}

/// The generation of one service: a fresh value on every change, so a poll computed against an
/// older one is not published. `cached` mirrors whether the metadata cache holds the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Generation {
    value: u64,
    cached: bool,
}

/// The retry state of one dirty service key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingKey {
    attempts: usize,
    not_before: Instant,
}

/// A dirty service key taken for reconciling, with the failures it already had.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DueKey {
    service_id: String,
    attempts: usize,
}

/// The state the event side shares with the reconciler, under [`Shared::state`].
#[derive(Debug)]
struct State {
    pending_cap: usize,
    // Hands out generation values, so a service removed and seen again never reuses one.
    generation_counter: u64,
    service_generation: HashMap<String, Generation>,
    node_generation: u64,
    epoch: u64,
    resync_requested: u64,
    resync_completed: u64,
    // None is Go's zero time: no resync outstanding.
    resync_outstanding_since: Option<Timestamp>,
    // The same keys: a key is appended to pending_order only when newly inserted into
    // pending_set, and removed from both when taken.
    pending_set: HashMap<String, PendingKey>,
    pending_order: Vec<String>,
    nodes_dirty: bool,
}

impl State {
    fn bump_service_generation(&mut self, service_id: &str, cached: Option<bool>) {
        self.generation_counter += 1;
        let value = self.generation_counter;
        let entry = self
            .service_generation
            .entry(service_id.to_owned())
            .or_insert(Generation {
                value,
                cached: false,
            });
        entry.value = value;
        if let Some(cached) = cached {
            entry.cached = cached;
        }
    }

    /// Drops the generation of every service that is neither cached nor queued, so the map does
    /// not keep the IDs of services the dirty set dropped on overflow.
    fn prune_service_generations(&mut self) {
        let pending = &self.pending_set;
        self.service_generation
            .retain(|service_id, generation| generation.cached || pending.contains_key(service_id));
    }

    /// Bumps the epoch, so a poll computed before the request is not published, and records
    /// when the resync started being outstanding.
    fn request_resync(&mut self, now: Timestamp) {
        if self.resync_requested == self.resync_completed {
            self.resync_outstanding_since = Some(now);
        }
        self.resync_requested += 1;
        self.epoch += 1;
    }
}

/// What the reconciler and its handles share.
struct Shared {
    state: Mutex<State>,
    // Room for one wake-up: an event that arrives while one is pending needs no other (Go's
    // channel of capacity 1).
    wake: Notify,
    // True once the first resync completed.
    ready: watch::Sender<bool>,
    clock: Arc<dyn Clock>,
    logger: Logger,
    metrics: Arc<ExporterMetrics>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes the reconciler without blocking.
    fn signal(&self) {
        self.wake.notify_one();
    }

    /// Drops the dirty set and requests a resync, which will see every change the set held.
    /// Fresh allocations release the old storage, and the generations of services neither
    /// cached nor queued go too (the epoch bump already invalidates every poll), so memory stays
    /// bounded by the cap plus the cache across any number of overflows, even while resyncs
    /// fail.
    fn overflow(&self, state: &mut State, now: Timestamp) {
        self.logger.warn(
            "dirty service keys over the cap; dropping them for a resync",
            &[Attr::int("cap", saturating_i64(state.pending_cap))],
        );
        state.pending_set = HashMap::new();
        state.pending_order = Vec::new();
        state.prune_service_generations();
        state.request_resync(now);
    }
}

fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn duration_nanos(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

/// The event side's view of the reconciler: it marks what changed dirty and asks for resyncs.
#[derive(Clone)]
pub struct ReconcilerHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ReconcilerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReconcilerHandle")
            .finish_non_exhaustive()
    }
}

impl ReconcilerHandle {
    /// Marks what `event` changed dirty. It never blocks and never calls Docker, so the
    /// dispatcher keeps draining the stream whatever the reconciler is doing. An event without
    /// an actor ID is dropped and counted; only node and service events are acted on.
    pub fn enqueue_event(&self, event: &Event) {
        if event.actor.id.is_empty() {
            self.shared.metrics.inc_events_dropped();
            self.shared.logger.debug(
                "event without actor ID dropped",
                &[
                    Attr::string("type", event.kind.as_str()),
                    Attr::string("action", event.action.as_str()),
                ],
            );
            return;
        }
        match event.kind.as_str() {
            EVENT_TYPE_NODE => self.mark_nodes_dirty(),
            EVENT_TYPE_SERVICE => self.mark_service_dirty(&event.actor.id, Instant::now()),
            _ => return,
        }
        self.shared.signal();
    }

    /// Flags the nodes for a refresh and invalidates the node generation. A pending backoff is
    /// kept: new events must not turn a failing node list into a tight loop.
    fn mark_nodes_dirty(&self) {
        let mut state = self.shared.lock();
        state.node_generation += 1;
        state.nodes_dirty = true;
    }

    /// Invalidates `service_id`'s generation and queues it, due at `now`. A queued key keeps its
    /// place in the FIFO and has its retry state reset: a fresh event supersedes a retry.
    fn mark_service_dirty(&self, service_id: &str, now: Instant) {
        let mut state = self.shared.lock();
        state.bump_service_generation(service_id, None);
        if let Some(pending) = state.pending_set.get_mut(service_id) {
            pending.attempts = 0;
            pending.not_before = now;
            return;
        }
        if state.pending_set.len() >= state.pending_cap {
            self.shared.overflow(&mut state, self.shared.clock.now());
            return;
        }
        state.pending_set.insert(
            service_id.to_owned(),
            PendingKey {
                attempts: 0,
                not_before: now,
            },
        );
        state.pending_order.push(service_id.to_owned());
    }

    /// Asks for a full resync and wakes the reconciler, which may have nothing scheduled;
    /// `reason` is logged.
    pub fn request_resync(&self, reason: &str) {
        {
            let mut state = self.shared.lock();
            self.shared
                .logger
                .debug("resync requested", &[Attr::string("reason", reason)]);
            state.request_resync(self.shared.clock.now());
        }
        self.shared.signal();
    }

    /// The resync counters, and since when a resync has been outstanding (none when none is).
    #[must_use]
    pub fn resync_state(&self) -> (u64, u64, Option<Timestamp>) {
        let state = self.shared.lock();
        (
            state.resync_requested,
            state.resync_completed,
            state.resync_outstanding_since,
        )
    }

    /// Sets the resync counters, as the Go tests set the reconciler's fields.
    #[cfg(test)]
    pub(crate) fn set_resync_state(
        &self,
        requested: u64,
        completed: u64,
        outstanding_since: Option<Timestamp>,
    ) {
        let mut state = self.shared.lock();
        state.resync_requested = requested;
        state.resync_completed = completed;
        state.resync_outstanding_since = outstanding_since;
    }

    /// Whether the first resync completed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        *self.shared.ready.borrow()
    }

    /// Completes once the first resync completed: from then on the caches hold every service
    /// and node.
    pub async fn ready(&self) {
        let mut receiver = self.shared.ready.subscribe();
        // The sender lives in self, so the wait ends only once ready.
        let _ = receiver.wait_for(|ready| *ready).await;
    }
}

/// The reconciler. Create it with [`Reconciler::new`], feed it events through its
/// [`ReconcilerHandle`], and run it with [`Reconciler::run`].
pub struct Reconciler<D> {
    docker: Arc<D>,
    shared: Arc<Shared>,
    collectors: Collectors,
    retry_delays: Vec<Duration>,
    backoff_initial: Duration,
    backoff_max: Duration,
    resync_interval: Duration,

    // Owned by the reconciler alone: no lock.
    metadata: MetadataCache,
    nodes: NodeCache,

    // Scheduling, read and written by the reconciler alone. None is Go's zero time: due at once.
    resync_not_before: Option<Instant>,
    resync_backoff: Duration,
    next_periodic_resync: Option<Instant>,
    nodes_not_before: Option<Instant>,
    nodes_backoff: Duration,
}

impl<D> std::fmt::Debug for Reconciler<D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reconciler")
            .field("metadata", &self.metadata)
            .field("nodes", &self.nodes)
            .finish_non_exhaustive()
    }
}

impl<D: DockerApi> Reconciler<D> {
    /// A reconciler on `docker` writing `collectors`, with the default policy and its first
    /// resync already requested.
    #[must_use]
    pub fn new(
        docker: Arc<D>,
        collectors: Collectors,
        clock: Arc<dyn Clock>,
        logger: Logger,
    ) -> Self {
        Self::with_policy(
            docker,
            collectors,
            clock,
            logger,
            &ReconcilerPolicy::default(),
        )
    }

    /// [`Reconciler::new`] with `policy`.
    #[must_use]
    pub fn with_policy(
        docker: Arc<D>,
        collectors: Collectors,
        clock: Arc<dyn Clock>,
        logger: Logger,
        policy: &ReconcilerPolicy,
    ) -> Self {
        let now = clock.now();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending_cap: policy.pending_cap,
                generation_counter: 0,
                service_generation: HashMap::new(),
                node_generation: 0,
                epoch: 0,
                resync_requested: 1,
                resync_completed: 0,
                resync_outstanding_since: Some(now),
                pending_set: HashMap::new(),
                pending_order: Vec::new(),
                nodes_dirty: false,
            }),
            wake: Notify::new(),
            ready: watch::Sender::new(false),
            clock,
            logger,
            metrics: Arc::clone(&collectors.metrics),
        });
        Self {
            docker,
            shared,
            collectors,
            retry_delays: policy.retry_delays.clone(),
            backoff_initial: policy.backoff_initial,
            backoff_max: policy.backoff_max,
            resync_interval: policy.resync_interval,
            metadata: MetadataCache::new(),
            nodes: NodeCache::new(),
            resync_not_before: None,
            resync_backoff: policy.backoff_initial,
            next_periodic_resync: None,
            nodes_not_before: None,
            nodes_backoff: policy.backoff_initial,
        }
    }

    /// The handle the event side and the health check use.
    #[must_use]
    pub fn handle(&self) -> ReconcilerHandle {
        ReconcilerHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The metadata cache.
    #[must_use]
    pub fn metadata(&self) -> &MetadataCache {
        &self.metadata
    }

    /// The node snapshot.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        self.nodes.get()
    }

    fn logger(&self) -> &Logger {
        &self.shared.logger
    }

    /// Reconciles until `cancel` is cancelled, then returns itself. It never sleeps: it waits on
    /// one timer armed for the earliest scheduled work, on new events, and on the cancellation.
    pub async fn run(mut self, cancel: CancellationToken) -> Self {
        while !cancel.is_cancelled() {
            self.cycle(&cancel).await;
            let delay = self.next_wake_delay(Instant::now());
            if delay == Some(Duration::ZERO) {
                continue;
            }
            let timer = async {
                match delay {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = cancel.cancelled() => break,
                () = self.shared.wake.notified() => {}
                () = timer => {}
            }
        }
        self
    }

    /// One round of due work, in fairness order: a periodic resync if one is due, the nodes, a
    /// resync, then at most [`SERVICE_KEYS_PER_CYCLE`] service keys. (Serving the polls waiting
    /// for a snapshot comes first; it arrives with the poll protocol, in plan phase 14.)
    async fn cycle(&mut self, cancel: &CancellationToken) {
        let now = Instant::now();
        if self.periodic_resync_due(now) {
            self.handle().request_resync("periodic");
        }
        if self.nodes_due(now) {
            let outcome = recover(self.reconcile_nodes(cancel)).await;
            self.after_step("nodes", outcome);
        }
        if self.resync_due(now) {
            let outcome = recover(self.resync(cancel)).await;
            self.after_step(RESYNC_STEP, outcome);
        }
        for key in self.take_due_keys(now) {
            if cancel.is_cancelled() {
                return;
            }
            let step = format!("service {}", key.service_id);
            let outcome = recover(self.reconcile_service(cancel, key)).await;
            self.after_step(&step, outcome);
        }
    }

    /// Logs a step's panic and repairs after it: the step may have stopped halfway, so a resync
    /// is requested, after the resync backoff.
    fn after_step(&mut self, step: &str, outcome: Result<(), PanicReport>) {
        let Err(report) = outcome else {
            return;
        };
        self.logger().error(
            "reconciler recovered from panic",
            &[
                Attr::string("step", step),
                Attr::string("panic", report.payload),
                Attr::string("stack", report.stack),
            ],
        );
        // A resync cut short by a panic neither completed nor went through its failure path.
        if step == RESYNC_STEP {
            self.collectors.metrics.inc_resync(ResyncResult::Failure);
        }
        self.request_resync_after_panic();
    }

    /// Requests a resync and schedules it after the resync backoff, like a failed one. Bad
    /// Docker data that made a step panic usually makes the resync panic too, and without the
    /// backoff the reconciler would retry it in a tight loop.
    fn request_resync_after_panic(&mut self) {
        self.shared.lock().request_resync(self.shared.clock.now());
        self.schedule_resync_retry(Instant::now());
    }

    /// Delays the next resync by the current backoff and grows it.
    fn schedule_resync_retry(&mut self, now: Instant) {
        self.resync_not_before = Some(now + self.resync_backoff);
        self.resync_backoff = self
            .resync_backoff
            .saturating_mul(BACKOFF_MULTIPLIER)
            .min(self.backoff_max);
    }

    /// Removes and returns, in FIFO order, up to [`SERVICE_KEYS_PER_CYCLE`] keys whose time has
    /// come. Keys wait until the first resync completed: before it there is no node snapshot to
    /// compute their counts from.
    fn take_due_keys(&self, now: Instant) -> Vec<DueKey> {
        let mut state = self.shared.lock();
        if state.resync_completed == 0 || state.pending_order.is_empty() {
            return Vec::new();
        }
        let mut taken = Vec::new();
        let order = std::mem::take(&mut state.pending_order);
        let mut remaining = Vec::with_capacity(order.len());
        for service_id in order {
            let due = state
                .pending_set
                .get(&service_id)
                .is_some_and(|pending| pending.not_before <= now);
            if taken.len() < SERVICE_KEYS_PER_CYCLE && due {
                if let Some(pending) = state.pending_set.remove(&service_id) {
                    taken.push(DueKey {
                        service_id,
                        attempts: pending.attempts,
                    });
                }
                continue;
            }
            remaining.push(service_id);
        }
        state.pending_order = remaining;
        taken
    }

    fn nodes_due(&self, now: Instant) -> bool {
        let state = self.shared.lock();
        state.resync_completed > 0
            && state.nodes_dirty
            && self.nodes_not_before.is_none_or(|at| at <= now)
    }

    fn resync_due(&self, now: Instant) -> bool {
        let state = self.shared.lock();
        state.resync_requested > state.resync_completed
            && self.resync_not_before.is_none_or(|at| at <= now)
    }

    fn periodic_resync_due(&self, now: Instant) -> bool {
        let state = self.shared.lock();
        state.resync_completed > 0
            && state.resync_requested == state.resync_completed
            && self.next_periodic_resync.is_none_or(|at| at <= now)
    }

    /// How long the reconciler may wait before the earliest scheduled work, zero when some is
    /// due, and none when nothing is scheduled (only an event can create work then).
    fn next_wake_delay(&self, now: Instant) -> Option<Duration> {
        let state = self.shared.lock();
        // None inside is Go's zero time: earlier than any instant.
        let mut earliest: Option<Option<Instant>> = None;
        let mut consider = |at: Option<Instant>| {
            let earlier = match (earliest, at) {
                (None, _) | (Some(Some(_)), None) => true,
                (Some(Some(current)), Some(at)) => at < current,
                (Some(None), _) => false,
            };
            if earlier {
                earliest = Some(at);
            }
        };
        if state.resync_requested > state.resync_completed {
            consider(self.resync_not_before);
        }
        if state.resync_completed > 0 {
            if state.resync_requested == state.resync_completed {
                consider(self.next_periodic_resync);
            }
            if state.nodes_dirty {
                consider(self.nodes_not_before);
            }
            for pending in state.pending_set.values() {
                consider(Some(pending.not_before));
            }
        }
        earliest.map(|at| at.map_or(Duration::ZERO, |at| at.saturating_duration_since(now)))
    }

    /// Inspects one dirty service and applies the result: the new spec, its removal when
    /// Docker no longer has it, or a delayed retry on any other failure.
    async fn reconcile_service(&mut self, cancel: &CancellationToken, key: DueKey) {
        let Some(inspected) = cancel
            .run_until_cancelled(inspect_service(&*self.docker, &key.service_id))
            .await
        else {
            // Shutting down: the key is not worth keeping.
            return;
        };
        match inspected {
            Ok(service) => {
                let nodes = self.nodes.shared();
                self.apply_service(&service, &nodes);
            }
            Err(error) if error.docker_error().is_not_found() => {
                self.remove_service(&key.service_id);
            }
            Err(error) => self.retry_service(&key, &error),
        }
    }

    /// Requeues a key whose inspect failed, with the next retry delay, or drops it and requests
    /// a resync once the delays are used up. A fresh event queued meanwhile wins.
    fn retry_service(&self, key: &DueKey, error: &CallError) {
        let now = Instant::now();
        let mut state = self.shared.lock();
        if state.pending_set.contains_key(&key.service_id) {
            return;
        }
        let Some(&delay) = self.retry_delays.get(key.attempts) else {
            self.logger().warn(
                "service inspect keeps failing; leaving it to a resync",
                &[
                    Attr::string("service_id", key.service_id.as_str()),
                    Attr::int("attempts", saturating_i64(key.attempts).saturating_add(1)),
                    Attr::error("err", error),
                ],
            );
            state.request_resync(self.shared.clock.now());
            return;
        };
        self.logger().debug(
            "service inspect failed; will retry",
            &[
                Attr::string("service_id", key.service_id.as_str()),
                Attr::duration("backoff", duration_nanos(delay)),
                Attr::error("err", error),
            ],
        );
        if state.pending_set.len() >= state.pending_cap {
            self.shared.overflow(&mut state, self.shared.clock.now());
            return;
        }
        state.pending_set.insert(
            key.service_id.clone(),
            PendingKey {
                attempts: key.attempts + 1,
                not_before: now + delay,
            },
        );
        state.pending_order.push(key.service_id.clone());
    }

    /// Stores the metadata of `service` and publishes its per-service families, with counts
    /// computed from `nodes`. When the label identity changed (a renamed service, a moved
    /// stack, a changed custom label value), the series under the old identity are deleted
    /// first: they would otherwise stay forever.
    fn apply_service(&mut self, service: &Service, nodes: &[Node]) {
        let mut metadata = build_metadata(service, &self.collectors.custom_labels);
        if let Some(previous) = self.metadata.get(&service.id)
            && labels_for_metadata(previous) != labels_for_metadata(&metadata)
        {
            let previous = previous.clone();
            self.delete_service_series(&previous);
        }
        let (desired, schedulable) = replica_counts(&metadata, nodes);
        metadata.desired_replicas = desired;
        self.metadata.replace(&service.id, metadata.clone());
        let gauges = &self.collectors.replica_gauges;
        must(gauges.set_desired(&metadata, desired));
        must(gauges.set_schedulable(&metadata, schedulable));
        must(
            self.collectors
                .service_update
                .update_for_service(service, &metadata),
        );
        self.shared
            .lock()
            .bump_service_generation(&service.id, Some(true));
    }

    /// Deletes the series and the cache entry of a service Docker no longer has.
    fn remove_service(&mut self, service_id: &str) {
        if let Some(metadata) = self.metadata.get(service_id).cloned() {
            self.delete_service_series(&metadata);
            self.metadata.delete(service_id);
        }
        self.collectors.replicas_state.forget_service(service_id);
        let mut state = self.shared.lock();
        if state.pending_set.contains_key(service_id) {
            // An event arrived meanwhile; its key still needs the generation it bumped.
            if let Some(generation) = state.service_generation.get_mut(service_id) {
                generation.cached = false;
            }
            return;
        }
        state.service_generation.remove(service_id);
    }

    /// Deletes every per-service series written under `metadata`'s label identity.
    fn delete_service_series(&self, metadata: &ServiceMetadata) {
        let labels = labels_for_metadata(metadata);
        let gauges = &self.collectors.replica_gauges;
        gauges.desired().delete_with(&labels);
        gauges.schedulable().delete_with(&labels);
        self.collectors.service_update.clear(metadata);
    }

    /// Refreshes the node snapshot and recomputes every service's counts from it and the
    /// cached placement. On failure nothing changes, the flag stays set, and the next attempt
    /// waits for a growing backoff.
    async fn reconcile_nodes(&mut self, cancel: &CancellationToken) {
        // Cleared before the call: a node event that arrives during it sets the flag again.
        self.shared.lock().nodes_dirty = false;
        let Some(listed) = cancel.run_until_cancelled(list_nodes(&*self.docker)).await else {
            return;
        };
        match listed {
            Ok(nodes) => {
                self.apply_nodes(&nodes);
                self.nodes_backoff = self.backoff_initial;
            }
            Err(error) => {
                self.shared.lock().nodes_dirty = true;
                self.nodes_not_before = Some(Instant::now() + self.nodes_backoff);
                self.logger().warn(
                    "node refresh failed; will retry",
                    &[
                        Attr::error("err", &error),
                        Attr::duration("backoff", duration_nanos(self.nodes_backoff)),
                    ],
                );
                self.nodes_backoff = self
                    .nodes_backoff
                    .saturating_mul(BACKOFF_MULTIPLIER)
                    .min(self.backoff_max);
            }
        }
    }

    /// Replaces the node snapshot, recomputes every cached service's counts from it and bumps
    /// the node generation.
    fn apply_nodes(&mut self, nodes: &[Node]) {
        self.nodes.set(nodes);
        self.collectors.nodes_by_state.update_from_slice(nodes);
        for (service_id, metadata) in self.metadata.all() {
            let (desired, schedulable) = replica_counts(&metadata, nodes);
            self.metadata.set_desired_replicas(&service_id, desired);
            let gauges = &self.collectors.replica_gauges;
            must(gauges.set_desired(&metadata, desired));
            must(gauges.set_schedulable(&metadata, schedulable));
        }
        self.shared.lock().node_generation += 1;
    }

    /// Lists every service and node and applies both. Nothing is written unless both lists
    /// succeeded; a failure only schedules the next attempt, after a growing backoff, so a
    /// daemon that keeps failing is not hammered. Requests raised while the pass runs (an
    /// overflow, a retry running out) are left outstanding for another pass, and so are the keys
    /// marked dirty meanwhile.
    async fn resync(&mut self, cancel: &CancellationToken) {
        let pass_start = self.shared.clock.now();
        let target = self.shared.lock().resync_requested;
        let Some(listed) = cancel
            .run_until_cancelled(list_services_and_nodes(&*self.docker))
            .await
        else {
            // A cancelled resync is a shutdown, not a failure.
            return;
        };
        let (services, nodes) = match listed {
            Ok(lists) => lists,
            Err(error) => {
                self.collectors.metrics.inc_resync(ResyncResult::Failure);
                self.logger().warn(
                    "resync failed; will retry",
                    &[
                        Attr::error("err", &error),
                        Attr::duration("backoff", duration_nanos(self.resync_backoff)),
                    ],
                );
                self.schedule_resync_retry(Instant::now());
                return;
            }
        };
        self.apply_nodes(&nodes);
        let mut listed_ids = HashSet::with_capacity(services.len());
        for service in &services {
            listed_ids.insert(service.id.as_str());
            self.apply_service(service, &nodes);
        }
        for service_id in self.metadata.ids() {
            if !listed_ids.contains(service_id.as_str()) {
                self.remove_service(&service_id);
            }
        }
        self.shared.lock().prune_service_generations();
        self.complete_resync(target, pass_start);
    }

    /// Records that every request up to `target` is served, and counts the resync as a success
    /// at the time it completed.
    fn complete_resync(&mut self, target: u64, pass_start: Timestamp) {
        self.collectors.metrics.inc_resync(ResyncResult::Success);
        self.collectors
            .metrics
            .timestamps()
            .mark_resync_ok(self.shared.clock.now());
        {
            let mut state = self.shared.lock();
            state.resync_completed = state.resync_completed.max(target);
            state.resync_outstanding_since = if state.resync_requested == state.resync_completed {
                None
            } else {
                // Raised during this pass, so no earlier than its start.
                Some(pass_start)
            };
        }
        self.resync_backoff = self.backoff_initial;
        self.resync_not_before = None;
        self.next_periodic_resync = Some(Instant::now() + self.resync_interval);
        self.shared
            .ready
            .send_if_modified(|ready| !std::mem::replace(ready, true));
    }
}

/// Every service, under one request deadline.
async fn list_services(docker: &impl DockerApi) -> Result<Vec<Service>, CallError> {
    with_docker_timeout(docker.service_list())
        .await
        .map_err(|error| CallError::new("service list", error))
}

/// Every node, under one request deadline.
async fn list_nodes(docker: &impl DockerApi) -> Result<Vec<Node>, CallError> {
    with_docker_timeout(docker.node_list())
        .await
        .map_err(|error| CallError::new("node list", error))
}

/// One service, under one request deadline.
async fn inspect_service(docker: &impl DockerApi, service_id: &str) -> Result<Service, CallError> {
    with_docker_timeout(docker.service_inspect(service_id))
        .await
        .map_err(|error| CallError::new(format!("service inspect {service_id}"), error))
}

/// Both lists, services first; fails if either fails.
async fn list_services_and_nodes(
    docker: &impl DockerApi,
) -> Result<(Vec<Service>, Vec<Node>), CallError> {
    let services = list_services(docker).await?;
    let nodes = list_nodes(docker).await?;
    Ok((services, nodes))
}

#[cfg(test)]
mod tests;
