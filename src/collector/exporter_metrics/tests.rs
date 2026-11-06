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
use crate::test_support::openmetrics::{self, Exposition};

fn exposition(metrics: &ExporterMetrics) -> Exposition {
    let mut registry = Registry::default();
    metrics.register(&mut registry);
    openmetrics::parse(&crate::metrics::render(&registry))
}

/// The value of the sample `name` with `labels`, none when absent; compared wrapped in `Some`,
/// as the whole suite compares floats.
fn value(exposition: &Exposition, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    exposition.value(name, labels)
}

/// Go: `TestConfigureExporterOpsMetrics_ResyncsStartAtZero`
#[test]
fn resyncs_start_at_zero() {
    let metrics = ExporterMetrics::new(Arc::new(Timestamps::default()));
    let text = exposition(&metrics);
    assert_eq!(text.types["swarm_exporter_resyncs"], "counter");
    assert_eq!(
        text.help["swarm_exporter_resyncs"],
        "Total number of resyncs (service and node list) by result: success or failure."
    );
    for result in ["failure", "success"] {
        assert_eq!(
            text.value("swarm_exporter_resyncs_total", &[("result", result)]),
            Some(0.0),
            "{result}"
        );
    }
    let resyncs: Vec<_> = text
        .samples
        .iter()
        .filter(|sample| sample.name == "swarm_exporter_resyncs_total")
        .collect();
    assert_eq!(resyncs.len(), 2, "only the two pre-seeded series");
}

/// Go: `TestLastPollSuccessTimestampFollowsMarkPollOK`: 0 before any poll, then the fractional
/// unix seconds of the mark.
#[test]
fn last_poll_success_follows_mark_poll_ok() {
    let timestamps = Arc::new(Timestamps::default());
    let metrics = ExporterMetrics::new(Arc::clone(&timestamps));
    let name = "swarm_exporter_last_poll_success_timestamp_seconds";
    assert_eq!(value(&exposition(&metrics), name, &[]), Some(0.0));
    timestamps.mark_poll_ok(Timestamp::new(1_700_000_000, 123_456_789).unwrap());
    let polled_at = 1_700_000_000_123_456_789_f64 / 1e9;
    let got = value(&exposition(&metrics), name, &[]).unwrap();
    assert!((got - polled_at).abs() < 1e-6, "{got} != {polled_at}");
}

/// The last resync timestamp is 0 until a resync completes, then its unix seconds.
#[test]
fn last_resync_success_follows_mark_resync() {
    let timestamps = Arc::new(Timestamps::default());
    let metrics = ExporterMetrics::new(Arc::clone(&timestamps));
    let name = "swarm_exporter_last_resync_success_timestamp_seconds";
    assert_eq!(value(&exposition(&metrics), name, &[]), Some(0.0));
    timestamps.mark_resync_ok(Timestamp::new(1_700_000_060, 0).unwrap());
    assert_eq!(
        value(&exposition(&metrics), name, &[]),
        Some(1_700_000_060.0)
    );
}

/// Every family keeps Go's name, type and HELP text, and the counters count: a counter is a
/// family without `_total` whose sample has it.
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
    let want = [
        (
            "swarm_exporter_events_dropped",
            "counter",
            "Total number of Swarm events dropped because they carried no actor ID.",
        ),
        (
            "swarm_exporter_events_reconnects",
            "counter",
            "Total number of event stream reconnects.",
        ),
        (
            "swarm_exporter_last_poll_success_timestamp_seconds",
            "gauge",
            "Unix time of the latest successfully published replicas-state poll; 0 if none yet.",
        ),
        (
            "swarm_exporter_last_resync_success_timestamp_seconds",
            "gauge",
            "Unix time of the latest completed resync (service and node list); 0 if none yet.",
        ),
        (
            "swarm_exporter_poll_duration_seconds",
            "histogram",
            "Duration of replicas-state polling, in seconds.",
        ),
        (
            "swarm_exporter_poll_errors",
            "counter",
            "Total number of replicas-state polls that resulted in error.",
        ),
        (
            "swarm_exporter_poll_rejections",
            "counter",
            "Total number of per-service poll results not published because the service changed \
             while its tasks were being listed.",
        ),
        (
            "swarm_exporter_polls",
            "counter",
            "Total number of replicas-state polls attempted.",
        ),
        (
            "swarm_exporter_resyncs",
            "counter",
            "Total number of resyncs (service and node list) by result: success or failure.",
        ),
    ];
    let got: Vec<(&str, &str, &str)> = text
        .types
        .iter()
        .map(|(name, kind)| (name.as_str(), kind.as_str(), text.help[name].as_str()))
        .collect();
    assert_eq!(got, want);
    for (name, count) in [
        ("swarm_exporter_polls_total", 2.0),
        ("swarm_exporter_poll_errors_total", 1.0),
        ("swarm_exporter_events_reconnects_total", 1.0),
        ("swarm_exporter_events_dropped_total", 1.0),
        ("swarm_exporter_poll_rejections_total", 1.0),
    ] {
        assert_eq!(value(&text, name, &[]), Some(count), "{name}");
    }
    assert_eq!(
        value(
            &text,
            "swarm_exporter_resyncs_total",
            &[("result", "success")]
        ),
        Some(1.0)
    );
    assert_eq!(
        value(
            &text,
            "swarm_exporter_resyncs_total",
            &[("result", "failure")]
        ),
        Some(0.0)
    );
}

/// The poll duration histogram: 30ms lands in every bucket from 0.05 up and none below, with the
/// sum and the count of the one observation.
#[test]
fn the_poll_duration_histogram_buckets_the_observation() {
    let metrics = ExporterMetrics::new(Arc::new(Timestamps::default()));
    metrics.observe_poll_duration(Duration::from_millis(30));
    let text = exposition(&metrics);
    let name = "swarm_exporter_poll_duration_seconds";
    let buckets: Vec<(f64, f64)> = text
        .samples
        .iter()
        .filter(|sample| sample.name == format!("{name}_bucket"))
        .map(|sample| {
            let (_, bound) = sample
                .labels
                .iter()
                .find(|(label, _)| label == "le")
                .unwrap();
            let bound = if bound == "+Inf" {
                f64::INFINITY
            } else {
                bound.parse().unwrap()
            };
            (bound, sample.value)
        })
        .collect();
    let bounds: Vec<f64> = buckets.iter().map(|&(bound, _)| bound).collect();
    let mut want_bounds = crate::metrics::DEF_BUCKETS.to_vec();
    want_bounds.push(f64::INFINITY);
    assert_eq!(bounds, want_bounds);
    for (bound, count) in buckets {
        assert_eq!(
            Some(count),
            Some(if bound >= 0.05 { 1.0 } else { 0.0 }),
            "le {bound}"
        );
    }
    assert_eq!(value(&text, &format!("{name}_count"), &[]), Some(1.0));
    assert!((value(&text, &format!("{name}_sum"), &[]).unwrap() - 0.03).abs() < 1e-12);
}
