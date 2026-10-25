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

//! The cluster's types: what is asked for ([`Spec`]), and what [`crate::up`] returns
//! ([`Cluster`], made of [`Node`]s).

use std::collections::HashMap;
use std::io;
use std::num::IntErrorKind;

use bollard::Docker;
use jiff::Timestamp;
use swarm_scheduler_exporter::go_compat::quote::quote;

use crate::Error;
use crate::envid::{LABEL_ENV_ID, generate_env_id};
use crate::redact::RedactingLogger;
use crate::sweep_rules::{LABEL_OWNER, LABEL_PID_NAMESPACE, LABEL_STARTED, Owner};

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

/// The harness's log on stderr.
#[must_use]
pub fn new_logger() -> RedactingLogger {
    RedactingLogger::new(Box::new(io::stderr()))
}

/// The Swarm role a node was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// The Swarm manager.
    Manager,
    /// A Swarm worker.
    Worker,
}

/// The requested topology and runtime settings of a test cluster.
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
    /// Leaves a failed bring-up in place instead of removing it.
    pub keep_on_failure: bool,
    /// Receives the harness's progress.
    pub logger: RedactingLogger,
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
                    "parse {ENV_WORKERS}: strconv.Atoi: parsing {}: {reason}",
                    quote(&raw_workers)
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
                "parse {ENV_KEEP_ON_FAILURE}: strconv.ParseBool: parsing {}: invalid syntax",
                quote(&raw_keep)
            ))
        })?;
    }

    let mut dind_image = get(ENV_DIND_IMAGE);
    if dind_image.is_empty() {
        DEFAULT_DIND_IMAGE.clone_into(&mut dind_image);
    }

    Ok(Spec {
        env_id: generate_env_id()?,
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

/// One member of the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The `DinD` container name on the host.
    pub name: String,
    /// The `DinD` container ID on the host, the target of [`Cluster::pause_node`].
    pub container_id: String,
    /// The container hostname, which Swarm reports as the node hostname.
    pub hostname: String,
    /// The Swarm role the node was given at bring-up.
    pub role: NodeRole,
    /// The node's ID inside the test Swarm.
    pub swarm_node_id: String,
    /// The 127.0.0.1 port on the host mapped to the node's daemon.
    pub host_port: u16,
    /// The `DOCKER_HOST` value that reaches the node's daemon from the host.
    pub docker_host: String,
    /// The node's address on the run's bridge network, which Swarm advertises.
    pub(crate) bridge_ip: String,
}

/// A running test Swarm returned by [`crate::up`]. Callers own it and must call
/// [`Cluster::down`].
#[derive(Debug)]
pub struct Cluster {
    /// What was asked for.
    pub spec: Spec,
    /// The manager, also first in [`Cluster::nodes`].
    pub manager: Node,
    /// The manager first, then the workers in order.
    pub nodes: Vec<Node>,
    /// The ID (`sha256:...`) of the workload image loaded on every node. Services reference it
    /// by ID so no node ever contacts a registry.
    pub workload_image_id: String,
    /// When bring-up began, the lower bound for reading the nodes' event logs.
    pub started_at: Timestamp,
    pub(crate) host_client: Docker,
    /// The test process that brought the cluster up, recorded in the owner label.
    pub(crate) owner: Owner,
}

impl Cluster {
    /// The worker nodes, in bring-up order.
    #[must_use]
    pub fn workers(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|node| node.role == NodeRole::Worker)
            .collect()
    }

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

    /// The labels every resource of the cluster carries: its envid, which `down` and the manual
    /// cleanup select by, and the owner and start time the sweep of a later run decides by.
    #[must_use]
    pub fn labels(&self) -> HashMap<String, String> {
        labels(&self.spec.env_id, &self.owner, self.started_at)
    }

    /// The client of the host daemon the nodes run on, for a test that needs a helper container
    /// beside the cluster. Label what it creates with [`Cluster::labels`], so `down` removes it
    /// even when the test's own cleanup never runs, and a later run's sweep can tell whose it is.
    #[must_use]
    pub fn host_client(&self) -> &Docker {
        &self.host_client
    }
}

/// The labels of [`Cluster::labels`], before the cluster exists.
pub(crate) fn labels(
    env_id: &str,
    owner: &Owner,
    started_at: Timestamp,
) -> HashMap<String, String> {
    HashMap::from([
        (LABEL_ENV_ID.to_owned(), env_id.to_owned()),
        (LABEL_OWNER.to_owned(), owner.to_string()),
        (
            LABEL_STARTED.to_owned(),
            started_at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
        ),
        // Empty where /proc cannot be read: a label with no value, which no sweep matches.
        (LABEL_PID_NAMESPACE.to_owned(), owner.pid_namespace.clone()),
    ])
}
