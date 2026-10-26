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

use std::collections::BTreeMap;

use super::*;
use crate::docker::types::{NodeSpec, NodeStatus};
use crate::logger::Level;
use crate::metrics::testing::{gather_series, series_id};
use crate::test_support::capture_logs;

fn node(role: &str, availability: &str, status: &str) -> Node {
    Node {
        spec: NodeSpec {
            role: role.to_owned(),
            availability: availability.to_owned(),
            ..NodeSpec::default()
        },
        status: NodeStatus {
            state: status.to_owned(),
            ..NodeStatus::default()
        },
        ..Node::default()
    }
}

/// Go's `nodeSeriesID`.
fn node_series_id(role: &str, availability: &str, status: &str) -> String {
    series_id(
        NODES_BY_STATE_NAME,
        &[
            (LABEL_NODE_ROLE, role),
            (LABEL_NODE_AVAILABILITY, availability),
            (LABEL_NODE_STATUS, status),
        ],
    )
}

fn gathered(nodes: &NodesByState) -> BTreeMap<String, f64> {
    gather_series(Arc::clone(nodes.family()) as Arc<dyn Collector>)
}

fn nodes_by_state() -> NodesByState {
    NodesByState::new(capture_logs(Level::INFO).0)
}

/// Go: `TestUpdateNodesByStateFromSlice_Aggregation`
#[test]
fn counts_nodes_per_combination() {
    let nodes = nodes_by_state();
    nodes.update_from_slice(&[
        node("worker", "active", "ready"),
        node("worker", "active", "ready"),
        node("manager", "drain", "down"),
    ]);
    assert_eq!(
        gathered(&nodes),
        BTreeMap::from([
            (node_series_id("worker", "active", "ready"), 2.0),
            (node_series_id("manager", "drain", "down"), 1.0),
        ])
    );
}

/// Go: `TestUpdateNodesByStateFromSlice_ResetsBetweenCalls`
#[test]
fn each_update_replaces_the_previous_set() {
    let nodes = nodes_by_state();
    nodes.update_from_slice(&[node("worker", "active", "ready")]);
    nodes.update_from_slice(&[node("manager", "active", "ready")]);
    assert_eq!(
        gathered(&nodes),
        BTreeMap::from([(node_series_id("manager", "active", "ready"), 1.0)])
    );
}

/// Go: `TestUpdateNodesByStateFromSlice_Empty`
#[test]
fn no_nodes_publish_no_series() {
    let nodes = nodes_by_state();
    nodes.update_from_slice(&[]);
    assert!(gathered(&nodes).is_empty());
}

fn test_nodes(count: usize, role: &str) -> Vec<Node> {
    (0..count).map(|_| node(role, "active", "ready")).collect()
}

/// Go: `TestUpdateNodesByStateFromSlice_BuildDoesNotPublish`: a scrape sees the whole previous
/// set while the next one is built.
#[test]
fn a_build_publishes_nothing_until_published() {
    let nodes = nodes_by_state();
    let expected_a = BTreeMap::from([(node_series_id("worker", "active", "ready"), 3.0)]);
    let expected_b = BTreeMap::from([(node_series_id("manager", "active", "ready"), 2.0)]);
    nodes.update_from_slice(&test_nodes(3, "worker"));
    assert_eq!(gathered(&nodes), expected_a);
    let builder = build_nodes_by_state(nodes.family(), &test_nodes(2, "manager"));
    assert_eq!(gathered(&nodes), expected_a);
    let families = builder.build().unwrap();
    assert_eq!(gathered(&nodes), expected_a);
    nodes.family().collector().publish(families);
    assert_eq!(gathered(&nodes), expected_b);
}

/// The values are the node's own, whatever they are: unknown and empty states are series too.
#[test]
fn raw_values_are_labels() {
    let nodes = nodes_by_state();
    nodes.update_from_slice(&[node("observer", "", "disconnected")]);
    assert_eq!(
        gathered(&nodes),
        BTreeMap::from([(node_series_id("observer", "", "disconnected"), 1.0)])
    );
}

#[test]
fn family_carries_go_name_help_and_labels() {
    let nodes = nodes_by_state();
    let desc = nodes.family().desc();
    assert_eq!(desc.name(), "swarm_cluster_nodes_by_state");
    assert_eq!(
        desc.help(),
        "Number of Swarm nodes grouped by role, availability, and status."
    );
    assert_eq!(desc.label_names(), ["role", "availability", "status"]);
    let registry = Registry::new();
    nodes.register(&registry).unwrap();
}
