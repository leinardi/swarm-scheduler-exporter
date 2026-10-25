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
pub const METRIC_SCHEDULABLE: &str = "swarm_service_schedulable_replicas";
pub const METRIC_UPDATE_STATE: &str = "swarm_service_update_state_info";
pub const METRIC_UPDATE_STARTED: &str = "swarm_service_update_started_timestamp_seconds";
pub const METRIC_UPDATE_COMPLETE: &str = "swarm_service_update_completed_timestamp_seconds";
pub const METRIC_CONTAINER_STATE: &str = "swarm_container_state";
pub const METRIC_POLLS_TOTAL: &str = "swarm_exporter_polls_total";
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

/// Waits until the exporter has completed a poll that started after this call, so the next
/// scrape reflects the Swarm state as of now. A poll already running when this was called can
/// finish first, hence the second one.
pub async fn wait_for_fresh_poll(test: &TestHandle, base_url: &str) -> Result<(), Fatal> {
    let mut start: Option<f64> = None;
    eventually(test, Duration::from_secs(30), async |ctx| {
        let scraped = scrape_metrics(ctx, base_url).await?;
        let polls = scraped
            .value(METRIC_POLLS_TOTAL, &no_labels())
            .ok_or_else(|| format!("no {METRIC_POLLS_TOTAL} series: not yet"))?;
        let first = *start.get_or_insert(polls);
        if polls < first + 2.0 {
            return Err(format!(
                "{METRIC_POLLS_TOTAL} at {}, waiting for {}: not yet",
                format_value(polls),
                format_value(first + 2.0)
            ));
        }
        Ok(())
    })
    .await
}

/// Checks a condition that must keep holding, such as "X is absent": re-runs `check` every
/// [`POLL_INTERVAL`] until the exporter has completed `polls` polls that started after the
/// call, and fails the test on the first error. [`eventually`] would be wrong here: one passing
/// scrape proves little for an absence, while one failing scrape proves the exporter reported X.
pub async fn consistently(
    test: &TestHandle,
    base_url: &str,
    polls: u32,
    mut check: impl AsyncFnMut(&Context) -> Result<(), String>,
) -> Result<(), Fatal> {
    let ctx = test.ctx().clone();
    let deadline = Instant::now() + Duration::from_secs(30) * (polls + 1);
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;
    let mut start: Option<f64> = None;
    loop {
        check(&ctx)
            .await
            .map_err(|error| test.fatal(format!("condition stopped holding: {error}")))?;
        let scraped = scrape_metrics(&ctx, base_url)
            .await
            .map_err(|error| test.fatal(error))?;
        let completed = scraped
            .value(METRIC_POLLS_TOTAL, &no_labels())
            .ok_or_else(|| test.fatal(format!("no {METRIC_POLLS_TOTAL} series")))?;
        let first = *start.get_or_insert(completed);
        // +1: the poll running when this started may have begun before it.
        if completed >= first + f64::from(polls) + 1.0 {
            return Ok(());
        }
        match ctx
            .run(tokio::time::timeout_at(deadline, ticker.tick()))
            .await
        {
            Err(ended) => {
                return Err(test.fatal(format!(
                    "condition held, but the test ended before {polls} exporter polls: {ended}"
                )));
            }
            Ok(Err(_)) => {
                return Err(test.fatal(format!(
                    "condition held, but the exporter did not complete {polls} polls in time"
                )));
            }
            Ok(Ok(_)) => {}
        }
    }
}

/// One expectation on a scrape.
#[derive(Debug, Clone)]
pub struct MetricWant {
    name: &'static str,
    labels: Labels,
    kind: WantKind,
}

#[derive(Debug, Clone, Copy)]
enum WantKind {
    /// A single series with this value.
    Value(f64),
    /// The sum over every matching series is this value.
    Sum(f64),
    /// A single series with any value above zero.
    Positive,
    /// No matching series at all.
    Absent,
}

/// A single series with this value.
pub fn want(name: &'static str, labels: Labels, value: f64) -> MetricWant {
    MetricWant {
        name,
        labels,
        kind: WantKind::Value(value),
    }
}

/// The sum over every matching series is this value.
pub fn want_sum(name: &'static str, labels: Labels, value: f64) -> MetricWant {
    MetricWant {
        name,
        labels,
        kind: WantKind::Sum(value),
    }
}

/// A single series with any value above zero.
pub fn want_positive(name: &'static str, labels: Labels) -> MetricWant {
    MetricWant {
        name,
        labels,
        kind: WantKind::Positive,
    }
}

/// No series matches.
pub fn want_absent(name: &'static str, labels: Labels) -> MetricWant {
    MetricWant {
        name,
        labels,
        kind: WantKind::Absent,
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
        match (self.kind, found.as_slice()) {
            (WantKind::Absent, []) => ("absent".to_owned(), true),
            (WantKind::Absent, many) => (format!("{} series", many.len()), false),
            (WantKind::Sum(value), _) => {
                // A family that only emits the combinations present (nodes_by_state) drops a
                // series rather than zeroing it, so no series sums to zero.
                let total = scraped.sum(self.name, &self.labels);
                (format_value(total), same(total, value))
            }
            (WantKind::Value(_) | WantKind::Positive, []) => ("absent".to_owned(), false),
            (WantKind::Value(value), [only]) => (format_value(only.value), same(only.value, value)),
            (WantKind::Positive, [only]) => (format_value(only.value), only.value > 0.0),
            (WantKind::Value(_) | WantKind::Positive, many) => {
                (format!("{} series", many.len()), false)
            }
        }
    }

    fn expected(&self) -> String {
        match self.kind {
            WantKind::Value(value) => format_value(value),
            WantKind::Sum(value) => format!("sum {}", format_value(value)),
            WantKind::Positive => "> 0".to_owned(),
            WantKind::Absent => "absent".to_owned(),
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
