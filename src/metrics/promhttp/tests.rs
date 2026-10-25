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

use std::sync::Arc;

use super::{HandlerMetrics, sanitize_code};
use crate::metrics::process::ProcessCollector;
use crate::metrics::{Registry, text};

/// The `process_*` and `promhttp_*` lines of the Go exporter's `/metrics` (v1.3.2, client_golang
/// v1.24.1), from S2's capture `run1/go-v1.3.2-prometheus3.0.body`: every HELP and TYPE line, and
/// the handler's samples after four scrapes, during the fifth.
const GO_RUNTIME_FAMILIES: &str = "\
# HELP process_cpu_seconds_total Total user and system CPU time spent in seconds.
# TYPE process_cpu_seconds_total counter
# HELP process_max_fds Maximum number of open file descriptors.
# TYPE process_max_fds gauge
# HELP process_network_receive_bytes_total Number of bytes received by the process over the network.
# TYPE process_network_receive_bytes_total counter
# HELP process_network_transmit_bytes_total Number of bytes sent by the process over the network.
# TYPE process_network_transmit_bytes_total counter
# HELP process_open_fds Number of open file descriptors.
# TYPE process_open_fds gauge
# HELP process_resident_memory_bytes Resident memory size in bytes.
# TYPE process_resident_memory_bytes gauge
# HELP process_start_time_seconds Start time of the process since unix epoch in seconds.
# TYPE process_start_time_seconds gauge
# HELP process_virtual_memory_bytes Virtual memory size in bytes.
# TYPE process_virtual_memory_bytes gauge
# HELP process_virtual_memory_max_bytes Maximum amount of virtual memory available in bytes.
# TYPE process_virtual_memory_max_bytes gauge
# HELP promhttp_metric_handler_requests_in_flight Current number of scrapes being served.
# TYPE promhttp_metric_handler_requests_in_flight gauge
promhttp_metric_handler_requests_in_flight 1
# HELP promhttp_metric_handler_requests_total Total number of scrapes by HTTP status code.
# TYPE promhttp_metric_handler_requests_total counter
promhttp_metric_handler_requests_total{code=\"200\"} 4
promhttp_metric_handler_requests_total{code=\"500\"} 0
promhttp_metric_handler_requests_total{code=\"503\"} 0
";

/// The family set, types and help texts match Go's capture byte for byte, and so do the handler
/// samples after the same four scrapes, seen from inside the fifth (Q1).
#[test]
fn runtime_families_match_the_go_capture() {
    let registry = Registry::new();
    registry
        .register(Arc::new(ProcessCollector::new()))
        .unwrap();
    let handler = HandlerMetrics::new();
    handler.register(&registry).unwrap();
    for _ in 0..4 {
        handler.begin().finish(200);
    }
    let _scrape = handler.begin();
    let mut out = String::new();
    text::encode(&mut out, &registry.gather());
    let mut kept = String::new();
    for line in out
        .lines()
        .filter(|line| line.starts_with('#') || line.starts_with("promhttp_"))
    {
        kept.push_str(line);
        kept.push('\n');
    }
    assert_eq!(kept, GO_RUNTIME_FAMILIES);
}

/// The guard counts under the sanitised code on finish, and only lowers the gauge when dropped
/// unfinished (a handler that panicked, which Go does not count either).
#[test]
fn in_flight_guard_counts_on_finish_only() {
    let handler = HandlerMetrics::new();
    let first = handler.begin();
    let second = handler.begin();
    assert!((handler.in_flight.get() - 2.0).abs() < f64::EPSILON);
    first.finish(503);
    drop(second);
    assert!(handler.in_flight.get().abs() < f64::EPSILON);
    assert_eq!(handler.requests.get(&["503"]), Some(1.0));
    assert_eq!(handler.requests.get(&["200"]), Some(0.0));
    handler.begin().finish(0);
    handler.begin().finish(404);
    handler.begin().finish(600);
    assert_eq!(handler.requests.get(&["200"]), Some(1.0));
    assert_eq!(handler.requests.get(&["404"]), Some(1.0));
    assert_eq!(handler.requests.get(&["unknown"]), Some(1.0));
}

/// promhttp's `sanitizeCode`.
#[test]
fn sanitize_code_follows_promhttp() {
    for (status, want) in [
        (0, "200"),
        (99, "unknown"),
        (100, "100"),
        (200, "200"),
        (307, "307"),
        (431, "431"),
        (599, "599"),
        (600, "unknown"),
        (999, "unknown"),
    ] {
        assert_eq!(sanitize_code(status), want, "{status}");
    }
}

/// Registering twice is refused, as `prometheus.Register` would (promhttp reuses the existing
/// collectors there; the exporter registers once, so it never reaches that path).
#[test]
fn registering_twice_is_refused() {
    let registry = Registry::new();
    let handler = HandlerMetrics::new();
    handler.register(&registry).unwrap();
    assert!(handler.register(&registry).is_err());
}
