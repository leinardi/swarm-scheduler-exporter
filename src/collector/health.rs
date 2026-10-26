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

//! Exporter health: the check `/healthz` and `swarm_exporter_health` share, evaluated when asked
//! rather than marked by the poller, so it turns unhealthy when polls stop, even if the poller
//! is stuck; the success timestamps it reads; and `swarm_exporter_build_info`.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::Timestamp;

use crate::clock::Clock;
use crate::metrics::{Collector, GaugeFunc, GaugeVec, Kind};
use crate::metrics::{RegisterError, Registry, constant_desc};

use super::metrics_ids::{EXPORTER_SUBSYSTEM, NAMESPACE};
use super::must;
use super::reconciler::ReconcilerHandle;

/// The reason before the first resync completed.
pub const REASON_INITIAL_RESYNC: &str = "initial resync not completed";
/// The reason before the first successful poll.
pub const REASON_NO_POLL: &str = "no successful poll yet";
/// The reason when the last successful poll is older than the window.
pub const REASON_STALE_POLL: &str = "last poll too old";
/// The reason when a requested resync has been outstanding for longer than the window.
pub const REASON_RESYNC_OUTSTANDING: &str = "resync outstanding";

/// The floor of the staleness window, whatever the poll delay.
const MIN_WINDOW: Duration = Duration::from_secs(30);

/// Unix nanoseconds of `timestamp`, Go's `Time.UnixNano`, saturating outside its range.
fn unix_nanos(timestamp: Timestamp) -> i64 {
    let nanos = timestamp.as_nanosecond();
    i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX })
}

/// The success times, in unix nanoseconds as Go stores them; 0 means never.
#[derive(Debug, Default)]
pub struct Timestamps {
    poll_success: AtomicI64,
    resync_success: AtomicI64,
    events_connect: AtomicI64,
}

impl Timestamps {
    /// Records the latest successful replicas-state publish. Callers pass the time the publish
    /// completed, not the time the poll started, so a slow poll does not look fresher than it
    /// is.
    pub fn mark_poll_ok(&self, now: Timestamp) {
        self.poll_success.store(unix_nanos(now), Ordering::SeqCst);
    }

    /// Records the latest completed resync.
    pub fn mark_resync_ok(&self, now: Timestamp) {
        self.resync_success.store(unix_nanos(now), Ordering::SeqCst);
    }

    /// Records when the event stream connected (or reconnected).
    pub fn mark_events_connected(&self, now: Timestamp) {
        self.events_connect.store(unix_nanos(now), Ordering::SeqCst);
    }

    /// The latest successful poll, 0 if none.
    #[must_use]
    pub fn last_poll_success(&self) -> i64 {
        self.poll_success.load(Ordering::SeqCst)
    }

    /// The latest completed resync, 0 if none.
    #[must_use]
    pub fn last_resync_success(&self) -> i64 {
        self.resync_success.load(Ordering::SeqCst)
    }

    /// The latest event stream connect, 0 if none.
    #[must_use]
    pub fn last_events_connect(&self) -> i64 {
        self.events_connect.load(Ordering::SeqCst)
    }
}

/// The health check.
#[derive(Clone)]
pub struct Health {
    // Go's activeReconciler: unset until the reconciler exists, which reads as "initial resync
    // not completed".
    reconciler: Arc<OnceLock<ReconcilerHandle>>,
    timestamps: Arc<Timestamps>,
    poll_delay: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Health {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Health")
            .field("reconciler", &self.reconciler)
            .field("timestamps", &self.timestamps)
            .field("poll_delay", &self.poll_delay)
            .finish_non_exhaustive()
    }
}

impl Health {
    /// The check of `reconciler`'s resyncs and the poll times in `timestamps`, for a poller
    /// running every `poll_delay`, reading the time from `clock`.
    #[must_use]
    pub fn new(
        reconciler: ReconcilerHandle,
        timestamps: Arc<Timestamps>,
        poll_delay: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let health = Self::unbound(timestamps, poll_delay, clock);
        health.bind(reconciler);
        health
    }

    /// The check before the reconciler exists: unhealthy, for the initial resync, until
    /// [`Health::bind`] names the reconciler. The metrics are registered before the Docker client
    /// and the reconciler are created, as in Go.
    #[must_use]
    pub fn unbound(
        timestamps: Arc<Timestamps>,
        poll_delay: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            reconciler: Arc::new(OnceLock::new()),
            timestamps,
            poll_delay,
            clock,
        }
    }

    /// Names the reconciler whose resyncs the check reads, for this check and every clone of it;
    /// a second call is ignored, as there is one reconciler per process.
    pub fn bind(&self, reconciler: ReconcilerHandle) {
        let _ = self.reconciler.set(reconciler);
    }

    /// Whether the exporter is healthy now, and why not.
    #[must_use]
    pub fn snapshot_now(&self) -> (bool, &'static str) {
        self.snapshot(self.clock.now())
    }

    /// Whether the exporter is healthy at `now`, and why not, checked in this order:
    /// - the reconciler completed its first resync, so the caches hold every service and node;
    /// - at least one poll succeeded;
    /// - that poll is no older than `max(3 × poll_delay, 30s)`;
    /// - no requested resync has been outstanding for longer than that same window: a resync
    ///   that keeps failing leaves the caches behind Docker while polls still succeed.
    #[must_use]
    pub fn snapshot(&self, now: Timestamp) -> (bool, &'static str) {
        let Some(reconciler) = self.reconciler.get() else {
            return (false, REASON_INITIAL_RESYNC);
        };
        let (requested, completed, outstanding_since) = reconciler.resync_state();
        if completed == 0 {
            return (false, REASON_INITIAL_RESYNC);
        }
        let last_poll = self.timestamps.last_poll_success();
        if last_poll == 0 {
            return (false, REASON_NO_POLL);
        }
        let window = i128::try_from(self.poll_delay.saturating_mul(3).max(MIN_WINDOW).as_nanos())
            .unwrap_or(i128::MAX);
        let now_nanos = now.as_nanosecond();
        if now_nanos - i128::from(last_poll) > window {
            return (false, REASON_STALE_POLL);
        }
        // Go's zero time when none is recorded: outstanding since forever.
        let since = outstanding_since.map_or(i128::MIN / 2, Timestamp::as_nanosecond);
        if requested > completed && now_nanos - since > window {
            return (false, REASON_RESYNC_OUTSTANDING);
        }
        (true, "")
    }
}

fn exporter_name(name: &str) -> String {
    format!("{NAMESPACE}_{EXPORTER_SUBSYSTEM}_{name}")
}

/// `swarm_exporter_health` and `swarm_exporter_build_info`.
#[derive(Debug)]
pub struct HealthGauges {
    health: Arc<GaugeFunc>,
    build_info: Arc<GaugeVec>,
}

impl HealthGauges {
    /// The health gauge, evaluating `health` on every scrape, and the build info set to 1 with
    /// `version`, `commit` and `date`.
    #[must_use]
    pub fn new(health: Health, version: &str, commit: &str, date: &str) -> Self {
        let build_info = Arc::new(GaugeVec::new(constant_desc(
            &exporter_name("build_info"),
            "Build information for this exporter.",
            Kind::Gauge,
            &["version", "commit", "date"],
        )));
        must(build_info.set(&[version, commit, date], 1.0));
        Self {
            health: Arc::new(GaugeFunc::new(
                constant_desc(
                    &exporter_name("health"),
                    "Exporter health status: 1=healthy, 0=unhealthy.",
                    Kind::Gauge,
                    &[],
                ),
                move || if health.snapshot_now().0 { 1.0 } else { 0.0 },
            )),
            build_info,
        }
    }

    /// Registers both gauges.
    ///
    /// # Errors
    ///
    /// The registry's first refusal.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        registry.register(Arc::clone(&self.health) as Arc<dyn Collector>)?;
        registry.register(Arc::clone(&self.build_info) as Arc<dyn Collector>)
    }

    /// The health gauge.
    #[must_use]
    pub fn health(&self) -> &Arc<GaugeFunc> {
        &self.health
    }

    /// The build info gauge.
    #[must_use]
    pub fn build_info(&self) -> &Arc<GaugeVec> {
        &self.build_info
    }
}

#[cfg(test)]
mod tests;
