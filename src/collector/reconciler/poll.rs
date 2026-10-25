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

//! The poll protocol between the poller and the reconciler. The poller asks for an immutable
//! polling snapshot, lists and counts tasks against it without touching the caches, and submits
//! the counts. The reconciler publishes a service's counts only if nothing about the service
//! changed in between, so a scrape never pairs task counts with metadata from another moment (a
//! job's new iteration, a new replica count, a new mode or placement).
//!
//! Go hands each request over an unbuffered channel, a rendezvous; here the request channels
//! hold one request. The difference cannot be observed: the poller awaits the reply either way,
//! and gives up on it when its own cancellation fires.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::docker::DockerApi;
use crate::logger::Attr;

use super::super::desired::labels_for_metadata;
use super::super::error::CallError;
use super::super::metrics_ids::{SERVICE_MODE_GLOBAL, SERVICE_MODE_GLOBAL_JOB};
use super::super::poll_snapshot::{PollService, PollSnapshot};
use super::super::replicas_state::{ServiceCounter, poll_replicas_state};
use super::{Reconciler, ReconcilerHandle, saturating_i64};

/// How many polls in a row a rejected service keeps its previously published series. A service
/// rejected more often than that has its series omitted and fails the poll, so health turns red
/// instead of the exporter publishing stale numbers indefinitely.
pub const MAX_CARRIED_POLLS: usize = 2;

const PARTIALLY_REJECTED: &str = "poll published without services rejected too many polls in a row";

/// Why a poll was not fully published.
#[derive(Debug, Clone)]
pub enum PollError {
    /// The poller's cancellation fired during the step named, Go's wrapped `context.Canceled`.
    Canceled(&'static str),
    /// The reconciler was shutting down when it took the request.
    Stopping,
    /// The task list failed.
    Poll(CallError),
    /// The replicas-state build failed; the previous set stays.
    Publish(String),
    /// Published without these services, rejected more than [`MAX_CARRIED_POLLS`] polls in a
    /// row, sorted.
    PartiallyRejected(Vec<String>),
}

impl PollError {
    /// Whether the poll ended on a cancellation, Go's `errors.Is(err, context.Canceled)`.
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Canceled(_) | Self::Stopping)
    }
}

impl fmt::Display for PollError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled(step) => write!(formatter, "{step}: context canceled"),
            Self::Stopping => formatter.write_str("reconciler stopping: context canceled"),
            Self::Poll(error) => fmt::Display::fmt(error, formatter),
            Self::Publish(message) => formatter.write_str(message),
            // Go's %v of a []string.
            Self::PartiallyRejected(omitted) => {
                write!(formatter, "{PARTIALLY_REJECTED}: [{}]", omitted.join(" "))
            }
        }
    }
}

impl std::error::Error for PollError {}

/// Asks the reconciler for a polling snapshot.
pub(super) struct SnapshotRequest {
    reply: oneshot::Sender<Result<Arc<PollSnapshot>, PollError>>,
}

/// Submits the counts of a poll made against `snapshot`.
pub(super) struct ApplyRequest {
    snapshot: Arc<PollSnapshot>,
    counters: ServiceCounter,
    reply: oneshot::Sender<Result<(), PollError>>,
}

/// The poller's ends of the request channels.
pub(super) struct PollSenders {
    snapshots: mpsc::Sender<SnapshotRequest>,
    applies: mpsc::Sender<ApplyRequest>,
}

/// The reconciler's ends of the request channels.
pub(super) struct PollReceivers {
    snapshots: mpsc::Receiver<SnapshotRequest>,
    applies: mpsc::Receiver<ApplyRequest>,
}

/// The request channels, each holding one request.
pub(super) fn poll_channels() -> (PollSenders, PollReceivers) {
    let (snapshot_sender, snapshot_receiver) = mpsc::channel(1);
    let (apply_sender, apply_receiver) = mpsc::channel(1);
    (
        PollSenders {
            snapshots: snapshot_sender,
            applies: apply_sender,
        },
        PollReceivers {
            snapshots: snapshot_receiver,
            applies: apply_receiver,
        },
    )
}

/// A request the reconciler took.
pub(super) enum PollRequest {
    Snapshot(SnapshotRequest),
    Apply(ApplyRequest),
}

impl PollReceivers {
    /// The next request, waiting for one.
    pub(super) async fn recv(&mut self) -> Option<PollRequest> {
        tokio::select! {
            Some(request) = self.snapshots.recv() => Some(PollRequest::Snapshot(request)),
            Some(request) = self.applies.recv() => Some(PollRequest::Apply(request)),
            else => None,
        }
    }

    /// A request already waiting, without waiting.
    fn try_recv(&mut self) -> Option<PollRequest> {
        if let Ok(request) = self.snapshots.try_recv() {
            return Some(PollRequest::Snapshot(request));
        }
        self.applies.try_recv().ok().map(PollRequest::Apply)
    }
}

/// Sends `request` unless `cancel` fires first. A reconciler that is gone never takes it, so
/// the poller waits for its own cancellation, as Go's send on a channel nobody reads does.
async fn send<T>(
    sender: &mpsc::Sender<T>,
    request: T,
    cancel: &CancellationToken,
    step: &'static str,
) -> Result<(), PollError> {
    let sent = cancel.run_until_cancelled(sender.send(request)).await;
    match sent {
        Some(Ok(())) => Ok(()),
        Some(Err(_)) => {
            cancel.cancelled().await;
            Err(PollError::Canceled(step))
        }
        None => Err(PollError::Canceled(step)),
    }
}

/// Waits for the reply unless `cancel` fires first; a request dropped unanswered waits for the
/// cancellation too.
async fn reply<T>(
    receiver: oneshot::Receiver<Result<T, PollError>>,
    cancel: &CancellationToken,
    step: &'static str,
) -> Result<T, PollError> {
    match cancel.run_until_cancelled(receiver).await {
        Some(Ok(answer)) => answer,
        Some(Err(_)) => {
            cancel.cancelled().await;
            Err(PollError::Canceled(step))
        }
        None => Err(PollError::Canceled(step)),
    }
}

impl ReconcilerHandle {
    /// Asks the reconciler for a polling snapshot, giving up when `cancel` fires.
    ///
    /// # Errors
    ///
    /// A [`PollError::Canceled`] or [`PollError::Stopping`].
    pub async fn request_poll_snapshot(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Arc<PollSnapshot>, PollError> {
        let (reply_sender, reply_receiver) = oneshot::channel();
        let request = SnapshotRequest {
            reply: reply_sender,
        };
        send(
            &self.shared.polls.snapshots,
            request,
            cancel,
            "request polling snapshot",
        )
        .await?;
        reply(reply_receiver, cancel, "wait for polling snapshot").await
    }

    /// Hands the counts of a poll to the reconciler and waits for the verdict: success when
    /// every service of `snapshot` was published or carried over.
    ///
    /// # Errors
    ///
    /// A cancellation, the reconciler stopping, a failed build or a partial rejection.
    pub async fn submit_poll_counts(
        &self,
        cancel: &CancellationToken,
        snapshot: Arc<PollSnapshot>,
        counters: ServiceCounter,
    ) -> Result<(), PollError> {
        let (reply_sender, reply_receiver) = oneshot::channel();
        let request = ApplyRequest {
            snapshot,
            counters,
            reply: reply_sender,
        };
        send(
            &self.shared.polls.applies,
            request,
            cancel,
            "submit poll counts",
        )
        .await?;
        reply(reply_receiver, cancel, "wait for poll verdict").await
    }
}

/// Runs one poll against the reconciler of `handle`: asks for a polling snapshot, lists and
/// counts the tasks of its services, and hands the counts back for the reconciler to judge and
/// publish. Only when the reconciler replies that everything was published is the poll recorded
/// as successful for health, at the time of that reply. A failed poll, a failed build or a
/// partial failure leaves the health timestamp where it was, so health turns red once polls keep
/// failing instead of reporting the last attempt.
///
/// # Errors
///
/// The first failure of the exchange, the task list or the verdict.
pub async fn poll_and_publish_replicas_state(
    docker: &impl DockerApi,
    handle: &ReconcilerHandle,
    cancel: &CancellationToken,
) -> Result<(), PollError> {
    let snapshot = handle.request_poll_snapshot(cancel).await?;
    let counters = cancel
        .run_until_cancelled(poll_replicas_state(docker, &snapshot))
        .await
        .ok_or(PollError::Canceled("task list"))?
        .map_err(PollError::Poll)?;
    handle
        .submit_poll_counts(cancel, snapshot, counters)
        .await?;
    handle
        .shared
        .metrics
        .timestamps()
        .mark_poll_ok(handle.shared.clock.now());
    Ok(())
}

/// The state a poll is judged against, read under the lock in one go.
struct PollVerdict {
    generations: HashMap<String, u64>,
    dirty: HashMap<String, bool>,
    node_generation: u64,
    nodes_dirty: bool,
    epoch: u64,
}

impl PollVerdict {
    /// Whether the counts of `service` can be published: nothing about it changed since the
    /// snapshot, and no change is waiting to be applied.
    fn accepts(&self, snapshot: &PollSnapshot, service_id: &str, service: &PollService) -> bool {
        let generation = self.generations.get(service_id).copied().unwrap_or(0);
        if self.epoch != snapshot.epoch
            || generation != service.generation
            || self.dirty.get(service_id).copied().unwrap_or(false)
        {
            return false;
        }
        let mode = service.metadata.service_mode.as_str();
        let node_dependent = mode == SERVICE_MODE_GLOBAL || mode == SERVICE_MODE_GLOBAL_JOB;
        // A node change waiting to be applied (a node list in backoff) leaves the node
        // generation where the snapshot saw it, but the snapshot's nodes are already known to be
        // out of date.
        !node_dependent || (self.node_generation == snapshot.node_generation && !self.nodes_dirty)
    }
}

impl<D: DockerApi> Reconciler<D> {
    /// Answers every poll request already waiting, without blocking. A cycle starts with it, so
    /// a poll never waits behind a burst of service keys.
    pub(super) fn serve_pending_poll_requests(&mut self, cancel: &CancellationToken) {
        while let Some(request) = self.poll_receivers.try_recv() {
            self.serve_poll_request(request, cancel);
        }
    }

    /// Answers one accepted request: with a snapshot or a verdict, or with the stopping error
    /// once the reconciler is shutting down. A poller that stopped waiting is not waited for.
    pub(super) fn serve_poll_request(&mut self, request: PollRequest, cancel: &CancellationToken) {
        #[cfg(test)]
        self.run_poll_request_hook();
        match request {
            PollRequest::Snapshot(request) => {
                let answer = if cancel.is_cancelled() {
                    Err(PollError::Stopping)
                } else {
                    Ok(Arc::new(self.build_poll_snapshot()))
                };
                let _ = request.reply.send(answer);
            }
            PollRequest::Apply(request) => {
                let answer = if cancel.is_cancelled() {
                    Err(PollError::Stopping)
                } else {
                    self.apply_poll_counts(&request.snapshot, &request.counters)
                };
                let _ = request.reply.send(answer);
            }
        }
    }

    /// Copies what a poll needs from the caches, which only the reconciler writes, with the
    /// generations and the epoch they correspond to.
    pub(super) fn build_poll_snapshot(&self) -> PollSnapshot {
        let state = self.shared.lock();
        let services = self
            .metadata
            .all()
            .into_iter()
            .map(|(service_id, metadata)| {
                let generation = state
                    .service_generation
                    .get(&service_id)
                    .map_or(0, |generation| generation.value);
                (service_id, PollService::new(generation, metadata))
            })
            .collect();
        PollSnapshot {
            services,
            nodes: self.nodes.get().to_vec(),
            node_generation: state.node_generation,
            epoch: state.epoch,
        }
    }

    fn current_poll_verdict(&self, snapshot: &PollSnapshot) -> PollVerdict {
        let state = self.shared.lock();
        let mut verdict = PollVerdict {
            generations: HashMap::with_capacity(snapshot.services.len()),
            dirty: HashMap::new(),
            node_generation: state.node_generation,
            nodes_dirty: state.nodes_dirty,
            epoch: state.epoch,
        };
        for service_id in snapshot.services.keys() {
            let generation = state
                .service_generation
                .get(service_id)
                .map_or(0, |generation| generation.value);
            verdict.generations.insert(service_id.clone(), generation);
            if state.pending_set.contains_key(service_id) {
                verdict.dirty.insert(service_id.clone(), true);
            }
        }
        verdict
    }

    /// Judges each service of a poll and publishes the result. A service no longer cached is
    /// dropped and marked dirty. An accepted service is published with the snapshot's labels and
    /// desired count, which are current since its generation did not move. A rejected one, or
    /// one the poll has no counts for, keeps its previously published series for up to
    /// [`MAX_CARRIED_POLLS`] polls in a row if its label identity did not change; past that it
    /// is omitted and the poll fails as a partial failure.
    pub(super) fn apply_poll_counts(
        &mut self,
        snapshot: &PollSnapshot,
        counters: &ServiceCounter,
    ) -> Result<(), PollError> {
        let verdict = self.current_poll_verdict(snapshot);
        let mut published = ServiceCounter::with_capacity(snapshot.services.len());
        let mut rejections = HashMap::with_capacity(self.poll_rejections.len());
        let mut omitted = Vec::new();
        for service_id in snapshot.service_ids() {
            let Some(service) = snapshot.services.get(&service_id) else {
                continue;
            };
            let Some(metadata) = self.metadata.get(&service_id) else {
                self.handle()
                    .mark_service_dirty(&service_id, Instant::now());
                continue;
            };
            // A service the poll has no counts for is handled like a rejected one, never
            // skipped: skipping it would drop its series while the poll still counted as a
            // success.
            if let Some(counter) = counters.get(&service_id)
                && verdict.accepts(snapshot, &service_id, service)
            {
                published.insert(service_id, counter.clone());
                continue;
            }
            self.collectors.metrics.inc_poll_rejections();
            let rejected = self.poll_rejections.get(&service_id).copied().unwrap_or(0) + 1;
            rejections.insert(service_id.clone(), rejected);
            let previous = self
                .last_published
                .get(&service_id)
                .filter(|previous| previous.labels == labels_for_metadata(metadata));
            if rejected > MAX_CARRIED_POLLS {
                omitted.push(service_id);
            } else if let Some(previous) = previous {
                published.insert(service_id, previous.clone());
            }
            // Else nothing current to show yet (a new service, a new identity): omitted for now.
        }
        self.collectors
            .replicas_state
            .update(&published)
            .map_err(PollError::Publish)?;
        self.last_published = published;
        self.poll_rejections = rejections;
        if omitted.is_empty() {
            return Ok(());
        }
        omitted.sort_unstable();
        self.logger().warn(
            PARTIALLY_REJECTED,
            &[
                Attr::strings("service_ids", omitted.iter().cloned()),
                Attr::int("limit", saturating_i64(MAX_CARRIED_POLLS)),
            ],
        );
        Err(PollError::PartiallyRejected(omitted))
    }
}

/// Runs in a test when the reconciler accepted a poll request, before it answers it.
#[cfg(test)]
pub(super) type PollRequestHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
impl<D> Reconciler<D> {
    fn run_poll_request_hook(&self) {
        // Cloned out first: the hook may block, and a test swaps it while the reconciler runs.
        let hook = self
            .shared
            .poll_request_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
impl ReconcilerHandle {
    /// Sets the hook run when a poll request is accepted, before it is answered; none clears it.
    pub(super) fn set_poll_request_hook(&self, hook: Option<PollRequestHook>) {
        *self
            .shared
            .poll_request_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hook;
    }
}
