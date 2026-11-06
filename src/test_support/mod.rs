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

//! Test doubles and helpers the unit tests share: the [`FakeDocker`], log capture through a scoped
//! `tracing` subscriber, and a bounded [`eventually`].

mod fake_docker;
pub(crate) mod openmetrics;

use std::time::Duration;

pub(crate) use fake_docker::{FakeDocker, FakeEvent};

use crate::docker::types::{Node, NodeDescription, NodeSpec, NodeStatus, Platform};
use std::sync::Arc;

use serde_json::Value;
use tracing::dispatcher::DefaultGuard;
use tracing::level_filters::LevelFilter;
use tracing_setup::testing::Capture;

/// The series of `collector` by [`openmetrics::series_id`]: registered on a fresh registry,
/// encoded and read back.
///
/// # Panics
///
/// A family that is not scalar.
pub(crate) fn gather_series(
    collector: &Arc<impl prometheus_client::collector::Collector>,
) -> std::collections::BTreeMap<String, f64> {
    let mut registry = prometheus_client::registry::Registry::default();
    crate::metrics::register(&mut registry, collector);
    openmetrics::parse(&crate::metrics::render(&registry)).series()
}

/// A counter's value as the float the tests compare.
///
/// # Panics
///
/// A count above `u32::MAX`, which no test reaches.
pub(crate) fn count(value: u64) -> f64 {
    f64::from(u32::try_from(value).expect("a small count"))
}

/// The series of what `register` registers, by [`openmetrics::series_id`].
pub(crate) fn gather_registered(
    register: impl FnOnce(&mut prometheus_client::registry::Registry),
) -> std::collections::BTreeMap<String, f64> {
    let mut registry = prometheus_client::registry::Registry::default();
    register(&mut registry);
    openmetrics::parse(&crate::metrics::render(&registry)).series()
}

/// How often [`eventually`] checks its condition.
const EVENTUALLY_INTERVAL: Duration = Duration::from_millis(10);

/// What a test logged, kept by the scoped subscriber [`capture_logs`] installs. Dropping it
/// restores the previous default.
pub(crate) struct Logs {
    capture: Capture,
    _guard: DefaultGuard,
}

impl Logs {
    /// Every record logged so far, as a JSON object: `level`, `message`, `target` and the
    /// record's fields at the top level.
    pub(crate) fn lines(&self) -> Vec<Value> {
        self.capture.json_lines()
    }

    /// The records whose message is `message`.
    pub(crate) fn with_message(&self, message: &str) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line["message"] == message)
            .collect()
    }

    /// The first record at `level` (`"WARN"`) with this message and these fields, a field's value
    /// compared as text (a number numerically).
    pub(crate) fn find(
        &self,
        level: &str,
        message: &str,
        fields: &[(&str, &str)],
    ) -> Option<Value> {
        self.lines().into_iter().find(|line| {
            line["level"] == level
                && line["message"] == message
                && fields.iter().all(|&(name, want)| {
                    line.get(name).is_some_and(|value| {
                        value.as_str().map_or_else(
                            || {
                                value.as_f64().map_or_else(
                                    || value.to_string().eq(want),
                                    |number| {
                                        want.parse::<f64>().is_ok_and(|parsed| {
                                            (parsed - number).abs() < f64::EPSILON
                                        })
                                    },
                                )
                            },
                            |text| text == want,
                        )
                    })
                })
        })
    }

    /// Whether [`Logs::find`] finds a record.
    pub(crate) fn contains(&self, level: &str, message: &str, fields: &[(&str, &str)]) -> bool {
        self.find(level, message, fields).is_some()
    }

    /// Everything logged, for an assertion message.
    pub(crate) fn dump(&self) -> String {
        self.capture.text()
    }

    /// Whether a record with exactly this message was logged.
    pub(crate) fn has(&self, message: &str) -> bool {
        !self.with_message(message).is_empty()
    }
}

/// A scoped subscriber at `level` and above, on this thread, writing flat JSON without record
/// times to the returned [`Logs`]: no process-wide subscriber to race on. A current-thread test
/// sees the records of the tasks it spawns too.
pub(crate) fn capture_logs(level: LevelFilter) -> Logs {
    let capture = Capture::default();
    let dispatch = tracing_setup::build(
        capture.clone(),
        tracing_setup::Format::Json,
        level,
        tracing_setup::Options::new().time(false),
    );
    Logs {
        _guard: tracing_setup::testing::set_default(&dispatch),
        capture,
    }
}

/// Waits until `condition` holds, checking every 10ms of tokio time, and fails the test once
/// `within` has passed: a wait for something that never happens fails instead of hanging, also
/// under a paused clock that keeps advancing.
pub(crate) async fn eventually(what: &str, within: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {within:?} waiting for {what}"
        );
        tokio::time::sleep(EVENTUALLY_INTERVAL).await;
    }
}

/// Go's `makeSchedulableNode`: a ready, active linux/amd64 worker.
pub(crate) fn schedulable_node(id: &str, hostname: &str) -> Node {
    Node {
        id: id.to_owned(),
        description: NodeDescription {
            hostname: hostname.to_owned(),
            platform: Platform {
                architecture: "amd64".to_owned(),
                os: "linux".to_owned(),
            },
            ..NodeDescription::default()
        },
        status: NodeStatus {
            state: "ready".to_owned(),
            ..NodeStatus::default()
        },
        spec: NodeSpec {
            role: "worker".to_owned(),
            availability: "active".to_owned(),
            ..NodeSpec::default()
        },
    }
}

#[cfg(test)]
mod tests;
