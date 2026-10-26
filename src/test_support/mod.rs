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

//! Test doubles and helpers the unit tests share: the [`FakeDocker`], log capture through the
//! logger's injectable sink, and a bounded [`eventually`].

mod fake_docker;

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

pub(crate) use fake_docker::{FakeDocker, FakeEvent};

use crate::docker::types::{Node, NodeDescription, NodeSpec, NodeStatus, Platform};
use crate::logger::{Config, Format, Level, Logger, MemorySink};

/// How often [`eventually`] checks its condition.
const EVENTUALLY_INTERVAL: Duration = Duration::from_millis(10);

/// A plain-format logger at `level` that writes to the returned sink, without record times: the
/// Go tests' handler swap, without a process-wide logger to race on.
pub(crate) fn capture_logs(level: Level) -> (Logger, MemorySink) {
    let sink = MemorySink::default();
    let logger = Logger::new(
        Config {
            format: Format::Plain,
            level,
            include_time: false,
        },
        Box::new(sink.clone()),
        TimeZone::UTC,
        Arc::new(|| Timestamp::UNIX_EPOCH),
    );
    (logger, sink)
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
