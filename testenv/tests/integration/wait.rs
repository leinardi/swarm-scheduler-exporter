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

//! Waiting for the exporter, never by sleeping: re-checks every [`POLL_INTERVAL`] until a
//! condition holds, so a fast machine passes as soon as it does and a slow one only costs time;
//! and the expectations a check compares a scrape against, with an expected-versus-actual table
//! on failure.

use std::fmt::Write as _;
use std::time::Duration;

use swarm_scheduler_exporter::gofmt_compat::duration;
use testenv::context::Context;
use testenv::metrics_text::{Labels, Scrape, format_series, format_value, labels};
use tokio::time::{Instant, MissedTickBehavior};

use crate::http::scrape_metrics;
use crate::swarm::ServiceKey;
use crate::testing::{Fatal, TestHandle};

/// How often a wait re-checks its condition.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub const METRIC_NODES_BY_STATE: &str = "swarm_cluster_nodes_by_state";
pub const METRIC_DESIRED: &str = "swarm_service_desired_replicas";
pub const METRIC_RUNNING: &str = "swarm_service_running_replicas";
pub const METRIC_AT_DESIRED: &str = "swarm_service_at_desired";
pub const METRIC_REPLICAS_STATE: &str = "swarm_task_replicas_state";
pub const METRIC_HEALTH: &str = "swarm_exporter_health";
pub const METRIC_BUILD_INFO: &str = "swarm_exporter_build_info";
pub const METRIC_EVENTS_RECONNECTS: &str = "swarm_exporter_events_reconnects_total";

pub const LABEL_STACK: &str = "stack";
pub const LABEL_SERVICE: &str = "service";

/// Go's `time.Duration.String` of a std duration.
pub fn go_duration(value: Duration) -> String {
    duration::format(i64::try_from(value.as_nanos()).unwrap_or(i64::MAX))
}

/// Calls `check` every [`POLL_INTERVAL`] until it succeeds, `timeout` elapses or `ctx` ends, and
/// returns its last error otherwise. The sibling of [`eventually`] for code that must not fail
/// the test, such as a cleanup.
pub async fn poll_until(
    ctx: &Context,
    timeout: Duration,
    mut check: impl AsyncFnMut() -> Result<(), String>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;
    loop {
        let Err(last) = check().await else {
            return Ok(());
        };
        let tick = tokio::time::timeout_at(deadline, ticker.tick());
        match ctx.run(tick).await {
            Err(ended) => return Err(format!("{last}\n{ended}")),
            Ok(Err(_)) => {
                return Err(format!(
                    "still failing after {}: {last}",
                    go_duration(timeout)
                ));
            }
            Ok(Ok(_)) => {}
        }
    }
}

/// Re-runs `check` until it succeeds, and fails the test with its last error when `timeout`
/// passes or the test or suite context ends first. `check` gets the context for its own calls.
pub async fn eventually(
    test: &TestHandle,
    timeout: Duration,
    mut check: impl AsyncFnMut(&Context) -> Result<(), String>,
) -> Result<(), Fatal> {
    let ctx = test.ctx().clone();
    poll_until(&ctx, timeout, async || check(&ctx).await)
        .await
        .map_err(|error| test.fatal(format!("condition not met: {error}")))
}

/// One expectation on a scrape.
#[derive(Debug, Clone)]
pub struct MetricWant {
    name: &'static str,
    labels: Labels,
    value: f64,
    /// Compare the sum over every matching series instead of a single series.
    sum: bool,
}

/// A single series with this value.
pub fn want(name: &'static str, labels: Labels, value: f64) -> MetricWant {
    MetricWant {
        name,
        labels,
        value,
        sum: false,
    }
}

/// The sum over every matching series is this value.
pub fn want_sum(name: &'static str, labels: Labels, value: f64) -> MetricWant {
    MetricWant {
        name,
        labels,
        value,
        sum: true,
    }
}

/// The service's single series of `name` has this value.
pub fn want_service(name: &'static str, service: &ServiceKey, value: f64) -> MetricWant {
    want(name, service.labels(), value)
}

/// No label: matches every series of a family.
pub fn no_labels() -> Labels {
    labels(&[])
}

/// Exact float equality, as Go's `==`: the values compared are small integers and sums of them.
fn same(left: f64, right: f64) -> bool {
    left.total_cmp(&right).is_eq()
}

impl MetricWant {
    /// Whether the expectation holds, and the actual value as the failure table shows it.
    fn check(&self, scraped: &Scrape) -> (String, bool) {
        let found = scraped.find(self.name, &self.labels);
        if self.sum {
            // A family that only emits the combinations present (nodes_by_state) drops a series
            // rather than zeroing it, so no series sums to zero.
            let total = scraped.sum(self.name, &self.labels);
            return (format_value(total), same(total, self.value));
        }
        match found.as_slice() {
            [] => ("absent".to_owned(), false),
            [only] => (format_value(only.value), same(only.value, self.value)),
            many => (format!("{} series", many.len()), false),
        }
    }

    fn expected(&self) -> String {
        if self.sum {
            format!("sum {}", format_value(self.value))
        } else {
            format_value(self.value)
        }
    }
}

/// Compares a scrape with every expectation: `Ok` when all hold, else the expected-versus-actual
/// table and the stack's series in the same scrape, so a timeout shows what the exporter said.
pub fn compare_metrics(scraped: &Scrape, stack: &str, wants: &[MetricWant]) -> Result<(), String> {
    let rows: Vec<(bool, String, String, String)> = wants
        .iter()
        .map(|wanted| {
            let (actual, holds) = wanted.check(scraped);
            (
                holds,
                format_series(wanted.name, &wanted.labels),
                wanted.expected(),
                actual,
            )
        })
        .collect();
    if rows.iter().all(|(holds, ..)| *holds) {
        return Ok(());
    }
    let series_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(0).max(6);
    let expected_width = rows.iter().map(|row| row.2.len()).max().unwrap_or(0).max(8);
    let mut table = format!(
        "metrics mismatch:\n    {:series_width$}  {:expected_width$}  actual\n",
        "series", "expected"
    );
    for (holds, series, expected, actual) in rows {
        let marker = if holds { "  " } else { "✗ " };
        let _ = writeln!(
            table,
            "{marker}  {series:series_width$}  {expected:expected_width$}  {actual}"
        );
    }
    if !stack.is_empty() {
        let _ = writeln!(table, "last scrape, stack {stack:?}:");
        for line in scraped.lines_with(LABEL_STACK, stack) {
            let _ = writeln!(table, "  {line}");
        }
    }
    Err(table)
}

/// An [`eventually`] check: scrapes `base_url` and holds when every expectation does. `stack`
/// selects the series printed on failure.
pub async fn metrics_match(
    ctx: &Context,
    base_url: &str,
    stack: &str,
    wants: &[MetricWant],
) -> Result<(), String> {
    let scraped = scrape_metrics(ctx, base_url).await?;
    compare_metrics(&scraped, stack, wants)
}
