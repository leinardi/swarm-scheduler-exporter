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

//! Aggregate node counts and an atomic per-node identity, state and manager view.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::docker::types::Node;
use crate::metrics::{self, Desc, SnapshotBuilder, SnapshotCollector, SnapshotFamily};
use prometheus_client::registry::Registry;
use tracing::error;

/// The `role` label.
pub const LABEL_NODE_ROLE: &str = "role";
/// The `availability` label.
pub const LABEL_NODE_AVAILABILITY: &str = "availability";
/// The `status` label.
pub const LABEL_NODE_STATUS: &str = "status";
/// The stable node identity.
pub const LABEL_NODE_ID: &str = "node_id";
/// The node hostname (not necessarily unique).
pub const LABEL_NODE: &str = "node";
/// The manager reachability label.
pub const LABEL_NODE_REACHABILITY: &str = "reachability";

const NODE_STATUSES: [&str; 4] = ["ready", "down", "disconnected", "unknown"];
const NODE_AVAILABILITIES: [&str; 3] = ["active", "pause", "drain"];
const MANAGER_REACHABILITIES: [&str; 3] = ["reachable", "unreachable", "unknown"];

/// The complete per-node view, swapped atomically across all five families.
#[derive(Debug)]
pub struct NodeMetrics {
    collector: Arc<SnapshotCollector>,
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeMetrics {
    /// Empty families, until the first node list succeeds.
    #[must_use]
    pub fn new() -> Self {
        use super::metrics_ids::{NAMESPACE, NODE_SUBSYSTEM};
        let desc = |name: &str, help: &str, extra: Option<&str>| {
            let mut labels = vec![LABEL_NODE_ID, LABEL_NODE];
            labels.extend(extra);
            Desc::new(
                &format!("{NAMESPACE}_{NODE_SUBSYSTEM}_{name}"),
                help,
                &labels,
            )
        };
        Self {
            collector: Arc::new(SnapshotCollector::new(vec![
                desc(
                    "info",
                    "Swarm node identity and role.",
                    Some(LABEL_NODE_ROLE),
                ),
                desc(
                    "status",
                    "Swarm node status (one-hot).",
                    Some(LABEL_NODE_STATUS),
                ),
                desc(
                    "availability",
                    "Swarm node availability (one-hot).",
                    Some(LABEL_NODE_AVAILABILITY),
                ),
                desc(
                    "manager_reachability",
                    "Swarm manager reachability (one-hot).",
                    Some(LABEL_NODE_REACHABILITY),
                ),
                desc(
                    "manager_leader",
                    "Whether the Swarm manager is the leader.",
                    None,
                ),
            ])),
        }
    }

    /// Registers the whole node view as one collector.
    pub fn register(&self, registry: &mut Registry) {
        metrics::register(registry, &self.collector);
    }

    /// Publishes a complete node list, retaining the previous view on a build error.
    pub fn update_from_slice(&self, nodes: &[Node]) {
        match build_node_metrics(&self.collector, nodes).build() {
            Ok(families) => self.collector.publish(families),
            Err(error) => error!(%error, "build node metrics snapshot; keeping previous"),
        }
    }
}

// Missing wire enums are empty; arbitrary strings are only a defensive fallback in tests.
fn known_or_unknown<'a>(value: &'a str, known: &[&str]) -> &'a str {
    if known.contains(&value) {
        value
    } else {
        "unknown"
    }
}

/// Builds all node families without publishing any of them.
#[must_use]
pub fn build_node_metrics(collector: &SnapshotCollector, nodes: &[Node]) -> SnapshotBuilder {
    let descs = collector.descs();
    let mut builder = SnapshotBuilder::new();
    for node in nodes {
        let identity = [
            (LABEL_NODE_ID, node.id.as_str()),
            (LABEL_NODE, node.description.hostname.as_str()),
        ];
        let mut info = identity.to_vec();
        info.push((LABEL_NODE_ROLE, node.spec.role.as_str()));
        builder.set(&descs[0], &info, 1.0);
        for (desc, label, states, current) in [
            (
                &descs[1],
                LABEL_NODE_STATUS,
                NODE_STATUSES.as_slice(),
                known_or_unknown(&node.status.state, &NODE_STATUSES),
            ),
            (
                &descs[2],
                LABEL_NODE_AVAILABILITY,
                NODE_AVAILABILITIES.as_slice(),
                node.spec.availability.as_str(),
            ),
        ] {
            for state in states {
                let mut labels = identity.to_vec();
                labels.push((label, *state));
                builder.set(desc, &labels, f64::from(*state == current));
            }
        }
        if node.spec.role == "manager" {
            let reachability = known_or_unknown(
                node.manager_status
                    .as_ref()
                    .map_or("", |status| status.reachability.as_str()),
                &MANAGER_REACHABILITIES,
            );
            for state in MANAGER_REACHABILITIES {
                let mut labels = identity.to_vec();
                labels.push((LABEL_NODE_REACHABILITY, state));
                builder.set(&descs[3], &labels, f64::from(state == reachability));
            }
            builder.set(
                &descs[4],
                &identity,
                f64::from(
                    node.manager_status
                        .as_ref()
                        .is_some_and(|status| status.leader),
                ),
            );
        }
    }
    builder
}

/// The family name.
pub const NODES_BY_STATE_NAME: &str = "swarm_cluster_nodes_by_state";
/// The HELP text.
pub const NODES_BY_STATE_HELP: &str =
    "Number of Swarm nodes grouped by role, availability, and status.";

/// The nodes-by-state family, with nothing published until the first update.
#[derive(Debug)]
pub struct NodesByState {
    family: Arc<SnapshotFamily>,
}

impl Default for NodesByState {
    fn default() -> Self {
        Self::new()
    }
}

impl NodesByState {
    /// The family, logging a failed build.
    #[must_use]
    pub fn new() -> Self {
        Self {
            family: Arc::new(SnapshotFamily::new(Desc::new(
                NODES_BY_STATE_NAME,
                NODES_BY_STATE_HELP,
                &[LABEL_NODE_ROLE, LABEL_NODE_AVAILABILITY, LABEL_NODE_STATUS],
            ))),
        }
    }

    /// Registers the family.
    ///
    pub fn register(&self, registry: &mut Registry) {
        metrics::register(registry, &self.family);
    }

    /// The family.
    #[must_use]
    pub fn family(&self) -> &Arc<SnapshotFamily> {
        &self.family
    }

    /// Publishes the counts of `nodes`, built whole first and swapped in at once, so a scrape
    /// never sees a half-built set; a failed build keeps the previous one.
    pub fn update_from_slice(&self, nodes: &[Node]) {
        match build_nodes_by_state(&self.family, nodes).build() {
            Ok(families) => self.family.collector().publish(families),
            Err(error) => error!(
                error = %error,
                "build nodes by state snapshot; keeping previous"
            ),
        }
    }
}

/// The node count per role, availability and status, as the node reports them, without
/// publishing it. A combination no node has gets no series, so a status no longer seen
/// disappears.
#[must_use]
pub fn build_nodes_by_state(family: &SnapshotFamily, nodes: &[Node]) -> SnapshotBuilder {
    let mut counts: BTreeMap<(&str, &str, &str), f64> = BTreeMap::new();
    for node in nodes {
        let key = (
            node.spec.role.as_str(),
            node.spec.availability.as_str(),
            node.status.state.as_str(),
        );
        *counts.entry(key).or_insert(0.0) += 1.0;
    }
    let mut builder = SnapshotBuilder::new();
    for ((role, availability, status), count) in counts {
        builder.set(
            family.desc(),
            &[
                (LABEL_NODE_ROLE, role),
                (LABEL_NODE_AVAILABILITY, availability),
                (LABEL_NODE_STATUS, status),
            ],
            count,
        );
    }
    builder
}

#[cfg(test)]
mod tests;
