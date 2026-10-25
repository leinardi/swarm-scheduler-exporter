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

//! The exporter's self-observability: poll and resync counters, the poll duration histogram and
//! the last-success timestamps, read at scrape time.

use std::sync::Arc;
use std::time::Duration;

use crate::gofmt_compat::float::from_int;
use crate::metrics::{
    Collector, Counter, CounterVec, DEF_BUCKETS, GaugeFunc, Histogram, Kind, RegisterError,
    Registry, constant_desc,
};

use super::health::Timestamps;
use super::metrics_ids::{
    EXPORTER_SUBSYSTEM, LABEL_RESULT, NAMESPACE, RESYNC_RESULT_FAILURE, RESYNC_RESULT_SUCCESS,
};
use super::must;

/// The outcome a resync is counted under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncResult {
    /// Both lists succeeded and were applied.
    Success,
    /// A list failed, or the resync panicked.
    Failure,
}

impl ResyncResult {
    /// The `result` label value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => RESYNC_RESULT_SUCCESS,
            Self::Failure => RESYNC_RESULT_FAILURE,
        }
    }
}

fn exporter_name(name: &str) -> String {
    format!("{NAMESPACE}_{EXPORTER_SUBSYSTEM}_{name}")
}

fn counter(name: &str, help: &str) -> Arc<Counter> {
    Arc::new(Counter::new(constant_desc(
        &exporter_name(name),
        help,
        Kind::Counter,
        &[],
    )))
}

/// A gauge reporting the unix nanoseconds `read` returns, at scrape time, in seconds; 0 stays 0
/// ("never").
fn unix_nano_timestamp_gauge(
    name: &str,
    help: &str,
    read: impl Fn() -> i64 + Send + Sync + 'static,
) -> Arc<GaugeFunc> {
    Arc::new(GaugeFunc::new(
        constant_desc(&exporter_name(name), help, Kind::Gauge, &[]),
        move || from_int(read()) / 1e9,
    ))
}

/// Every self-observability family.
#[derive(Debug)]
pub struct ExporterMetrics {
    poll_duration: Arc<Histogram>,
    polls: Arc<Counter>,
    poll_errors: Arc<Counter>,
    events_reconnects: Arc<Counter>,
    events_dropped: Arc<Counter>,
    poll_rejections: Arc<Counter>,
    resyncs: Arc<CounterVec>,
    last_poll_success: Arc<GaugeFunc>,
    last_resync_success: Arc<GaugeFunc>,
    timestamps: Arc<Timestamps>,
}

impl ExporterMetrics {
    /// The families, the timestamp gauges reading `timestamps`, and both `resyncs_total` series
    /// created at 0, so `increase()` and "never failed" alerts work before the first resync.
    #[must_use]
    pub fn new(timestamps: Arc<Timestamps>) -> Self {
        let resyncs = Arc::new(CounterVec::new(constant_desc(
            &exporter_name("resyncs_total"),
            "Total number of resyncs (service and node list) by result: success or failure.",
            Kind::Counter,
            &[LABEL_RESULT],
        )));
        for result in [ResyncResult::Success, ResyncResult::Failure] {
            must(resyncs.add(&[result.as_str()], 0.0));
        }
        let poll_timestamps = Arc::clone(&timestamps);
        let resync_timestamps = Arc::clone(&timestamps);
        Self {
            poll_duration: Arc::new(Histogram::new(
                constant_desc(
                    &exporter_name("poll_duration_seconds"),
                    "Duration of replicas-state polling, in seconds.",
                    Kind::Histogram,
                    &[],
                ),
                &DEF_BUCKETS,
            )),
            polls: counter(
                "polls_total",
                "Total number of replicas-state polls attempted.",
            ),
            poll_errors: counter(
                "poll_errors_total",
                "Total number of replicas-state polls that resulted in error.",
            ),
            events_reconnects: counter(
                "events_reconnects_total",
                "Total number of event stream reconnects.",
            ),
            events_dropped: counter(
                "events_dropped_total",
                "Total number of Swarm events dropped because they carried no actor ID.",
            ),
            poll_rejections: counter(
                "poll_rejections_total",
                "Total number of per-service poll results not published because the service \
                 changed while its tasks were being listed.",
            ),
            resyncs,
            last_poll_success: unix_nano_timestamp_gauge(
                "last_poll_success_timestamp_seconds",
                "Unix time of the latest successfully published replicas-state poll; 0 if none \
                 yet.",
                move || poll_timestamps.last_poll_success(),
            ),
            last_resync_success: unix_nano_timestamp_gauge(
                "last_resync_success_timestamp_seconds",
                "Unix time of the latest completed resync (service and node list); 0 if none \
                 yet.",
                move || resync_timestamps.last_resync_success(),
            ),
            timestamps,
        }
    }

    /// Registers every family.
    ///
    /// # Errors
    ///
    /// The registry's first refusal; the families before it stay registered.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        let collectors: [Arc<dyn Collector>; 9] = [
            Arc::clone(&self.poll_duration) as Arc<dyn Collector>,
            Arc::clone(&self.polls) as Arc<dyn Collector>,
            Arc::clone(&self.poll_errors) as Arc<dyn Collector>,
            Arc::clone(&self.events_reconnects) as Arc<dyn Collector>,
            Arc::clone(&self.events_dropped) as Arc<dyn Collector>,
            Arc::clone(&self.poll_rejections) as Arc<dyn Collector>,
            Arc::clone(&self.resyncs) as Arc<dyn Collector>,
            Arc::clone(&self.last_poll_success) as Arc<dyn Collector>,
            Arc::clone(&self.last_resync_success) as Arc<dyn Collector>,
        ];
        collectors
            .into_iter()
            .try_for_each(|collector| registry.register(collector))
    }

    /// The timestamps the gauges read.
    #[must_use]
    pub fn timestamps(&self) -> &Arc<Timestamps> {
        &self.timestamps
    }

    /// Records one poll's duration.
    pub fn observe_poll_duration(&self, duration: Duration) {
        self.poll_duration.observe(duration.as_secs_f64());
    }

    /// Counts one replicas-state poll.
    pub fn inc_polls(&self) {
        self.polls.inc();
    }

    /// Counts one failed replicas-state poll.
    pub fn inc_poll_errors(&self) {
        self.poll_errors.inc();
    }

    /// Counts one event stream reconnect.
    pub fn inc_event_reconnect(&self) {
        self.events_reconnects.inc();
    }

    /// Counts one event dropped for its missing actor ID.
    pub fn inc_events_dropped(&self) {
        self.events_dropped.inc();
    }

    /// Counts one per-service poll result not published.
    pub fn inc_poll_rejections(&self) {
        self.poll_rejections.inc();
    }

    /// Counts one resync under `result`.
    pub fn inc_resync(&self, result: ResyncResult) {
        must(self.resyncs.inc(&[result.as_str()]));
    }

    /// `swarm_exporter_resyncs_total{result}`.
    #[must_use]
    pub fn resyncs(&self) -> &Arc<CounterVec> {
        &self.resyncs
    }

    /// `swarm_exporter_events_dropped_total`.
    #[must_use]
    pub fn events_dropped(&self) -> &Arc<Counter> {
        &self.events_dropped
    }

    /// `swarm_exporter_events_reconnects_total`.
    #[must_use]
    pub fn events_reconnects(&self) -> &Arc<Counter> {
        &self.events_reconnects
    }

    /// `swarm_exporter_poll_rejections_total`.
    #[must_use]
    pub fn poll_rejections(&self) -> &Arc<Counter> {
        &self.poll_rejections
    }
}

#[cfg(test)]
mod tests;
