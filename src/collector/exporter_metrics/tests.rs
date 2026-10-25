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

use jiff::Timestamp;

use super::*;
use crate::metrics::text::encode;

fn exposition(metrics: &ExporterMetrics) -> String {
    let registry = Registry::new();
    metrics.register(&registry).unwrap();
    let mut out = String::new();
    encode(&mut out, &registry.gather());
    out
}

fn family<'text>(exposition: &'text str, name: &str) -> Vec<&'text str> {
    exposition
        .lines()
        .filter(|line| {
            line.starts_with(name)
                || line.starts_with(&format!("# HELP {name} "))
                || line.starts_with(&format!("# TYPE {name} "))
        })
        .collect()
}

/// Go: `TestConfigureExporterOpsMetrics_ResyncsStartAtZero`
#[test]
fn resyncs_start_at_zero() {
    let metrics = ExporterMetrics::new(Arc::new(Timestamps::default()));
    assert_eq!(
        family(&exposition(&metrics), "swarm_exporter_resyncs_total"),
        [
            "# HELP swarm_exporter_resyncs_total Total number of resyncs (service and node list) \
             by result: success or failure.",
            "# TYPE swarm_exporter_resyncs_total counter",
            "swarm_exporter_resyncs_total{result=\"failure\"} 0",
            "swarm_exporter_resyncs_total{result=\"success\"} 0",
        ]
    );
}

/// Go: `TestLastPollSuccessTimestampFollowsMarkPollOK`: 0 before any poll, then the fractional
/// unix seconds of the mark.
#[test]
fn last_poll_success_follows_mark_poll_ok() {
    let timestamps = Arc::new(Timestamps::default());
    let metrics = ExporterMetrics::new(Arc::clone(&timestamps));
    let name = "swarm_exporter_last_poll_success_timestamp_seconds";
    assert!(family(&exposition(&metrics), name).contains(&format!("{name} 0").as_str()));
    timestamps.mark_poll_ok(Timestamp::new(1_700_000_000, 123_456_789).unwrap());
    let polled_at = 1_700_000_000_123_456_789_f64 / 1e9;
    let value = format!("{name} {}", crate::gofmt_compat::float::format_g(polled_at));
    assert!(
        family(&exposition(&metrics), name).contains(&value.as_str()),
        "{value}"
    );
}

/// Every family keeps Go's name, type and HELP text, and the counters count.
#[test]
fn families_carry_go_names_types_and_help() {
    let metrics = ExporterMetrics::new(Arc::new(Timestamps::default()));
    metrics.inc_polls();
    metrics.inc_polls();
    metrics.inc_poll_errors();
    metrics.inc_event_reconnect();
    metrics.inc_events_dropped();
    metrics.inc_poll_rejections();
    metrics.inc_resync(ResyncResult::Success);
    metrics.observe_poll_duration(Duration::from_millis(30));
    let text = exposition(&metrics);
    let help: Vec<&str> = text.lines().filter(|line| line.starts_with('#')).collect();
    assert_eq!(
        help,
        [
            "# HELP swarm_exporter_events_dropped_total Total number of Swarm events dropped \
             because they carried no actor ID.",
            "# TYPE swarm_exporter_events_dropped_total counter",
            "# HELP swarm_exporter_events_reconnects_total Total number of event stream \
             reconnects.",
            "# TYPE swarm_exporter_events_reconnects_total counter",
            "# HELP swarm_exporter_last_poll_success_timestamp_seconds Unix time of the latest \
             successfully published replicas-state poll; 0 if none yet.",
            "# TYPE swarm_exporter_last_poll_success_timestamp_seconds gauge",
            "# HELP swarm_exporter_last_resync_success_timestamp_seconds Unix time of the latest \
             completed resync (service and node list); 0 if none yet.",
            "# TYPE swarm_exporter_last_resync_success_timestamp_seconds gauge",
            "# HELP swarm_exporter_poll_duration_seconds Duration of replicas-state polling, in \
             seconds.",
            "# TYPE swarm_exporter_poll_duration_seconds histogram",
            "# HELP swarm_exporter_poll_errors_total Total number of replicas-state polls that \
             resulted in error.",
            "# TYPE swarm_exporter_poll_errors_total counter",
            "# HELP swarm_exporter_poll_rejections_total Total number of per-service poll \
             results not published because the service changed while its tasks were being \
             listed.",
            "# TYPE swarm_exporter_poll_rejections_total counter",
            "# HELP swarm_exporter_polls_total Total number of replicas-state polls attempted.",
            "# TYPE swarm_exporter_polls_total counter",
            "# HELP swarm_exporter_resyncs_total Total number of resyncs (service and node list) \
             by result: success or failure.",
            "# TYPE swarm_exporter_resyncs_total counter",
        ]
    );
    for line in [
        "swarm_exporter_polls_total 2",
        "swarm_exporter_poll_errors_total 1",
        "swarm_exporter_events_reconnects_total 1",
        "swarm_exporter_events_dropped_total 1",
        "swarm_exporter_poll_rejections_total 1",
        "swarm_exporter_resyncs_total{result=\"success\"} 1",
        "swarm_exporter_poll_duration_seconds_bucket{le=\"0.05\"} 1",
        "swarm_exporter_poll_duration_seconds_bucket{le=\"0.025\"} 0",
        "swarm_exporter_poll_duration_seconds_count 1",
    ] {
        assert!(
            text.lines().any(|candidate| candidate == line),
            "{line} in {text}"
        );
    }
    let registry = Registry::new();
    metrics.register(&registry).unwrap();
    assert!(metrics.register(&registry).is_err());
}
