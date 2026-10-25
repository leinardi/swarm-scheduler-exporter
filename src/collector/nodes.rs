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

//! `swarm_cluster_nodes_by_state`: the node count per role, availability and status, rebuilt
//! whole from each node snapshot.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::docker::types::Node;
use crate::logger::{Attr, Logger};
use crate::metrics::{
    Collector, Kind, RegisterError, Registry, SnapshotBuilder, SnapshotFamily, constant_desc,
};

/// The `role` label.
pub const LABEL_NODE_ROLE: &str = "role";
/// The `availability` label.
pub const LABEL_NODE_AVAILABILITY: &str = "availability";
/// The `status` label.
pub const LABEL_NODE_STATUS: &str = "status";

/// The family name.
pub const NODES_BY_STATE_NAME: &str = "swarm_cluster_nodes_by_state";
/// The HELP text.
pub const NODES_BY_STATE_HELP: &str =
    "Number of Swarm nodes grouped by role, availability, and status.";

/// The nodes-by-state family, with nothing published until the first update.
#[derive(Debug)]
pub struct NodesByState {
    family: Arc<SnapshotFamily>,
    logger: Logger,
}

impl NodesByState {
    /// The family, logging a failed build to `logger`.
    #[must_use]
    pub fn new(logger: Logger) -> Self {
        Self {
            family: Arc::new(SnapshotFamily::new(constant_desc(
                NODES_BY_STATE_NAME,
                NODES_BY_STATE_HELP,
                Kind::Gauge,
                &[LABEL_NODE_ROLE, LABEL_NODE_AVAILABILITY, LABEL_NODE_STATUS],
            ))),
            logger,
        }
    }

    /// Registers the family.
    ///
    /// # Errors
    ///
    /// The registry's refusal.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        registry.register(Arc::clone(&self.family) as Arc<dyn Collector>)
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
            Err(error) => self.logger.error(
                "build nodes by state snapshot; keeping previous",
                &[Attr::error("err", &error)],
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
