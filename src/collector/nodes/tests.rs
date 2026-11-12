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
use crate::test_support::gather_series;
use crate::test_support::openmetrics::series_id;

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
    gather_series(nodes.family())
}

fn nodes_by_state() -> NodesByState {
    NodesByState::new()
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
    let mut registry = Registry::default();
    nodes.register(&mut registry);
}

fn node_view(metrics: &NodeMetrics) -> BTreeMap<String, f64> {
    crate::test_support::gather_registered(|registry| metrics.register(registry))
}

#[test]
fn per_node_states_are_exhaustive_and_workers_have_no_manager_series() {
    use crate::docker::types::ManagerStatus;
    let metrics = NodeMetrics::new();
    for status in ["ready", "down", "disconnected", "unknown", "", "invalid"] {
        for availability in ["active", "pause", "drain", "", "invalid"] {
            for reachability in ["reachable", "unreachable", "unknown", "", "invalid"] {
                let mut manager = node("manager", availability, status);
                manager.id = "manager".to_owned();
                manager.description.hostname = "same-host".to_owned();
                manager.manager_status = Some(ManagerStatus {
                    leader: true,
                    reachability: reachability.to_owned(),
                });
                let mut worker = manager.clone();
                worker.id = "worker".to_owned();
                worker.spec.role = "worker".to_owned();
                worker.manager_status = None;
                metrics.update_from_slice(&[manager, worker]);
                let view = node_view(&metrics);
                assert_eq!(view.len(), 20);
                for id in ["manager", "worker"] {
                    let identity = [(LABEL_NODE_ID, id), (LABEL_NODE, "same-host")];
                    for (name, label, states, current) in [
                        (
                            "swarm_node_status",
                            LABEL_NODE_STATUS,
                            NODE_STATUSES.as_slice(),
                            known_or_unknown(status, &NODE_STATUSES),
                        ),
                        (
                            "swarm_node_availability",
                            LABEL_NODE_AVAILABILITY,
                            NODE_AVAILABILITIES.as_slice(),
                            availability,
                        ),
                    ] {
                        for state in states {
                            let mut labels = identity.to_vec();
                            labels.push((label, *state));
                            assert_eq!(
                                view.get(&series_id(name, &labels)),
                                Some(&f64::from(*state == current))
                            );
                        }
                    }
                }
                for state in MANAGER_REACHABILITIES {
                    let labels = [
                        (LABEL_NODE_ID, "manager"),
                        (LABEL_NODE, "same-host"),
                        (LABEL_NODE_REACHABILITY, state),
                    ];
                    assert_eq!(
                        view.get(&series_id("swarm_node_manager_reachability", &labels)),
                        Some(&f64::from(
                            state == known_or_unknown(reachability, &MANAGER_REACHABILITIES)
                        ))
                    );
                }
                assert!(
                    view.keys()
                        .filter(|key| key.starts_with("swarm_node_manager_"))
                        .all(|key| !key.contains("node_id=\"worker\""))
                );
            }
        }
    }
}

#[test]
fn node_removal_demotion_and_missing_manager_fields_replace_the_snapshot() {
    let metrics = NodeMetrics::new();
    let mut manager = node("manager", "active", "ready");
    manager.id = "n1".to_owned();
    metrics.update_from_slice(&[manager.clone()]);
    let view = node_view(&metrics);
    let identity = [(LABEL_NODE_ID, "n1"), (LABEL_NODE, "")];
    assert_eq!(
        view.get(&series_id("swarm_node_manager_leader", &identity)),
        Some(&0.0)
    );
    let mut labels = identity.to_vec();
    labels.push((LABEL_NODE_REACHABILITY, "unknown"));
    assert_eq!(
        view.get(&series_id("swarm_node_manager_reachability", &labels)),
        Some(&1.0)
    );
    manager.spec.role = "worker".to_owned();
    metrics.update_from_slice(&[manager]);
    assert!(
        node_view(&metrics)
            .keys()
            .all(|key| !key.starts_with("swarm_node_manager_"))
    );
    metrics.update_from_slice(&[]);
    assert!(node_view(&metrics).is_empty());
}
