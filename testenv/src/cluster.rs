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

//! The cluster's types: what is asked for ([`Spec`], from the `SSE_IT_*` variables), and what
//! [`crate::up`] returns ([`Cluster`], testenv-core's `DinD` Swarm with the workload image).

use std::io::{self, Write as _};
use std::num::IntErrorKind;
use std::ops::Deref;

use clock::{Clock as _, SystemClock};
use testenv_core::dind::{self, Node};
use testenv_core::redact::Logger;

use crate::Error;
use crate::config::ENV_IDS;

/// The absolute path of the exporter binary the tests run against the cluster.
pub const ENV_BINARY: &str = "SSE_IT_BINARY";
/// Overrides the pinned docker:dind image used for every node.
pub const ENV_DIND_IMAGE: &str = "SSE_IT_DIND_IMAGE";
/// Overrides the number of worker nodes (default 2).
pub const ENV_WORKERS: &str = "SSE_IT_WORKERS";
/// Keeps the cluster running when bring-up or a test fails, for debugging.
pub const ENV_KEEP_ON_FAILURE: &str = "SSE_IT_KEEP_ON_FAILURE";

const DEFAULT_WORKER_COUNT: usize = 2;

/// The docker:dind image every node runs unless [`ENV_DIND_IMAGE`] says otherwise.
pub(crate) const DEFAULT_DIND_IMAGE: &str = "docker:29.8.1-dind";

/// The harness's log on stderr: one line per record, led by the system clock's time in UTC.
#[must_use]
pub fn new_logger() -> Logger {
    Logger::new(|line| {
        let _ = writeln!(io::stderr().lock(), "time={} {line}", SystemClock.now());
    })
}

/// The requested topology and runtime settings of a test cluster, as the `SSE_IT_*` variables
/// give them; [`crate::up`] turns it into testenv-core's [`dind::Spec`].
#[derive(Debug, Clone)]
pub struct Spec {
    /// Namespaces every resource of the run; [`spec_from_env`] generates one.
    pub env_id: String,
    /// The exporter binary under test ([`ENV_BINARY`]); the harness itself does not use it.
    pub binary: String,
    /// The docker:dind image every node runs.
    pub dind_image: String,
    /// The number of worker nodes; there is always exactly one manager.
    pub workers: usize,
    /// Leaves a failed bring-up or a failed run in place, labelled with [`crate::config::KEEP_LABEL`],
    /// instead of removing it.
    pub keep_on_failure: bool,
    /// Receives the harness's progress.
    pub logger: Logger,
}

/// A Spec built from the `SSE_IT_*` variables `getenv` returns, with a freshly generated envid.
///
/// # Errors
///
/// A worker count that is not a non-negative integer, a keep-on-failure value that is not one of
/// Go's `strconv.ParseBool` spellings, or no random source for the envid.
pub fn spec_from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Spec, Error> {
    let get = |key: &str| getenv(key).unwrap_or_default();

    let mut workers = DEFAULT_WORKER_COUNT;
    let raw_workers = get(ENV_WORKERS);
    if !raw_workers.is_empty() {
        let parsed: i64 = raw_workers
            .parse()
            .map_err(|error: std::num::ParseIntError| {
                let overflow = matches!(
                    error.kind(),
                    IntErrorKind::PosOverflow | IntErrorKind::NegOverflow
                );
                let reason = if overflow {
                    "value out of range"
                } else {
                    "invalid syntax"
                };
                Error::new(format!(
                    "parse {ENV_WORKERS}: strconv.Atoi: parsing {raw_workers:?}: {reason}"
                ))
            })?;
        workers = usize::try_from(parsed).map_err(|_| {
            Error::new(format!(
                "parse {ENV_WORKERS}={parsed}: worker count must not be negative"
            ))
        })?;
    }

    let mut keep_on_failure = false;
    let raw_keep = get(ENV_KEEP_ON_FAILURE);
    if !raw_keep.is_empty() {
        keep_on_failure = parse_bool(&raw_keep).ok_or_else(|| {
            Error::new(format!(
                "parse {ENV_KEEP_ON_FAILURE}: strconv.ParseBool: parsing {raw_keep:?}: invalid syntax"
            ))
        })?;
    }

    let mut dind_image = get(ENV_DIND_IMAGE);
    if dind_image.is_empty() {
        DEFAULT_DIND_IMAGE.clone_into(&mut dind_image);
    }

    Ok(Spec {
        env_id: ENV_IDS.generate()?,
        binary: get(ENV_BINARY),
        dind_image,
        workers,
        keep_on_failure,
        logger: new_logger(),
    })
}

/// Go's `strconv.ParseBool`.
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// A running test Swarm returned by [`crate::up`]: testenv-core's [`dind::Cluster`], which it
/// derefs to (the spec, the nodes, `down`, `pause_node` and the rest), and the workload image
/// loaded on every node. Callers own it and must call `down`.
#[derive(Debug)]
pub struct Cluster {
    pub(crate) swarm: dind::Cluster,
    /// The ID (`sha256:...`) of the workload image loaded on every node. Services reference it
    /// by ID so no node ever contacts a registry.
    pub workload_image_id: String,
}

impl Deref for Cluster {
    type Target = dind::Cluster;

    fn deref(&self) -> &dind::Cluster {
        &self.swarm
    }
}

impl Cluster {
    /// The node whose Swarm node ID is `swarm_node_id`.
    ///
    /// # Errors
    ///
    /// No node of the cluster has it.
    pub fn node_by_swarm_id(&self, swarm_node_id: &str) -> Result<&Node, Error> {
        self.nodes
            .iter()
            .find(|node| node.swarm_node_id == swarm_node_id)
            .ok_or_else(|| {
                Error::new(format!(
                    "swarm node {swarm_node_id:?}: node is not part of the cluster"
                ))
            })
    }
}
