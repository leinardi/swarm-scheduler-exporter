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

use crate::num::from_int;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

use crate::metrics::{self, DEF_BUCKETS, ScrapeGauge};

use super::health::Timestamps;
use super::metrics_ids::{
    EXPORTER_SUBSYSTEM, LABEL_RESULT, NAMESPACE, RESYNC_RESULT_FAILURE, RESYNC_RESULT_SUCCESS,
};

/// The label set of `swarm_exporter_resyncs_total`: `result`.
pub type ResultLabels = Vec<(&'static str, &'static str)>;

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

/// A gauge reporting the unix nanoseconds `read` returns, at scrape time, in seconds; 0 stays 0
/// ("never").
fn unix_nano_timestamp_gauge(
    name: &str,
    help: &str,
    read: impl Fn() -> i64 + Send + Sync + 'static,
) -> Arc<ScrapeGauge> {
    ScrapeGauge::new(&exporter_name(name), help, move || from_int(read()) / 1e9)
}

/// Every self-observability family. The counters are registered without `_total`:
/// prometheus-client adds it to the sample.
#[derive(Debug)]
pub struct ExporterMetrics {
    poll_duration: Histogram,
    polls: Counter,
    poll_errors: Counter,
    events_reconnects: Counter,
    events_dropped: Counter,
    poll_rejections: Counter,
    resyncs: Family<ResultLabels, Counter>,
    last_poll_success: Arc<ScrapeGauge>,
    last_resync_success: Arc<ScrapeGauge>,
    timestamps: Arc<Timestamps>,
}

impl ExporterMetrics {
    /// The families, the timestamp gauges reading `timestamps`, and both `resyncs_total` series
    /// created at 0, so `increase()` and "never failed" alerts work before the first resync.
    #[must_use]
    pub fn new(timestamps: Arc<Timestamps>) -> Self {
        let resyncs = Family::<ResultLabels, Counter>::default();
        for result in [ResyncResult::Success, ResyncResult::Failure] {
            // Creates the series at 0.
            let _ = resyncs.get_or_create(&vec![(LABEL_RESULT, result.as_str())]);
        }
        let poll_timestamps = Arc::clone(&timestamps);
        let resync_timestamps = Arc::clone(&timestamps);
        Self {
            poll_duration: Histogram::new(DEF_BUCKETS),
            polls: Counter::default(),
            poll_errors: Counter::default(),
            events_reconnects: Counter::default(),
            events_dropped: Counter::default(),
            poll_rejections: Counter::default(),
            resyncs,
            last_poll_success: unix_nano_timestamp_gauge(
                "last_poll_success_timestamp_seconds",
                LAST_POLL_SUCCESS_HELP,
                move || poll_timestamps.last_poll_success(),
            ),
            last_resync_success: unix_nano_timestamp_gauge(
                "last_resync_success_timestamp_seconds",
                LAST_RESYNC_SUCCESS_HELP,
                move || resync_timestamps.last_resync_success(),
            ),
            timestamps,
        }
    }

    /// Registers every family.
    pub fn register(&self, registry: &mut Registry) {
        registry.register(
            exporter_name("poll_duration_seconds"),
            registry_help(POLL_DURATION_HELP),
            self.poll_duration.clone(),
        );
        for (name, help, counter) in [
            ("polls", POLLS_HELP, &self.polls),
            ("poll_errors", POLL_ERRORS_HELP, &self.poll_errors),
            (
                "events_reconnects",
                EVENTS_RECONNECTS_HELP,
                &self.events_reconnects,
            ),
            ("events_dropped", EVENTS_DROPPED_HELP, &self.events_dropped),
            (
                "poll_rejections",
                POLL_REJECTIONS_HELP,
                &self.poll_rejections,
            ),
        ] {
            registry.register(exporter_name(name), registry_help(help), counter.clone());
        }
        registry.register(
            exporter_name("resyncs"),
            registry_help(RESYNCS_HELP),
            self.resyncs.clone(),
        );
        metrics::register(registry, &self.last_poll_success);
        metrics::register(registry, &self.last_resync_success);
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
        self.resyncs
            .get_or_create(&vec![(LABEL_RESULT, result.as_str())])
            .inc();
    }

    /// The count of resyncs under `result`.
    #[must_use]
    pub fn resyncs(&self, result: ResyncResult) -> u64 {
        self.resyncs
            .get_or_create(&vec![(LABEL_RESULT, result.as_str())])
            .get()
    }

    /// `swarm_exporter_polls_total`.
    #[must_use]
    pub fn polls(&self) -> u64 {
        self.polls.get()
    }

    /// `swarm_exporter_poll_errors_total`.
    #[must_use]
    pub fn poll_errors(&self) -> u64 {
        self.poll_errors.get()
    }

    /// `swarm_exporter_events_dropped_total`.
    #[must_use]
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.get()
    }

    /// `swarm_exporter_events_reconnects_total`.
    #[must_use]
    pub fn events_reconnects(&self) -> u64 {
        self.events_reconnects.get()
    }

    /// `swarm_exporter_poll_rejections_total`.
    #[must_use]
    pub fn poll_rejections(&self) -> u64 {
        self.poll_rejections.get()
    }
}

/// `help` as prometheus-client's `register` takes it: it appends the final period itself.
fn registry_help(help: &str) -> &str {
    help.strip_suffix('.').unwrap_or(help)
}

const POLL_DURATION_HELP: &str = "Duration of replicas-state polling, in seconds.";
const POLLS_HELP: &str = "Total number of replicas-state polls attempted.";
const POLL_ERRORS_HELP: &str = "Total number of replicas-state polls that resulted in error.";
const EVENTS_RECONNECTS_HELP: &str = "Total number of event stream reconnects.";
const EVENTS_DROPPED_HELP: &str =
    "Total number of Swarm events dropped because they carried no actor ID.";
const POLL_REJECTIONS_HELP: &str = "Total number of per-service poll results not published \
    because the service changed while its tasks were being listed.";
const RESYNCS_HELP: &str =
    "Total number of resyncs (service and node list) by result: success or failure.";
const LAST_POLL_SUCCESS_HELP: &str =
    "Unix time of the latest successfully published replicas-state poll; 0 if none yet.";
const LAST_RESYNC_SUCCESS_HELP: &str =
    "Unix time of the latest completed resync (service and node list); 0 if none yet.";

#[cfg(test)]
mod tests;
