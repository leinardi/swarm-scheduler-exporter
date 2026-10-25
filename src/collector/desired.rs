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

//! Desired and schedulable replicas per service, with Swarm's placement rules for global
//! services: which nodes a task could be placed on, by node state, platform and constraints.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::docker::types::{Node, Placement, Platform};
use crate::go_compat::float::from_int;
use crate::go_compat::net_ip::{ip_equal, parse_cidr, parse_ip};
use crate::go_compat::strings::{equal_fold, to_lower, trim_space};
use crate::labels::{maybe_warn_high_cardinality, sanitize_label_names, sanitize_metric_labels};
use crate::metrics::{Collector, Kind, RegisterError, Registry, constant_desc};
use crate::metrics::{GaugeVec, LabelsMismatch};

use super::metrics_ids::{
    LABEL_DISPLAY_NAME, LABEL_SERVICE, LABEL_SERVICE_MODE, LABEL_STACK, NAMESPACE,
    SERVICE_MODE_REPLICATED, SERVICE_MODE_REPLICATED_JOB, SERVICE_SUBSYSTEM,
};
use super::types::{CustomLabels, ServiceMetadata, display_name, is_job_mode};

/// The HELP text of `swarm_service_desired_replicas`.
pub const DESIRED_REPLICAS_HELP: &str = "Number of desired replicas for a Swarm service \
    (replicated: configured replicas; replicated-job: total completions; global and global-job: \
    eligible nodes).";

/// The HELP text of `swarm_service_schedulable_replicas`.
pub const SCHEDULABLE_REPLICAS_HELP: &str = "Number of replicas that can currently be scheduled \
    given node availability and placement constraints (replicated: min(configured, \
    eligible_nodes); global: eligible nodes; jobs and restart-condition none services: 0).";

const NODE_STATE_READY: &str = "ready";
const NODE_AVAILABILITY_ACTIVE: &str = "active";

const ARCH_386: &str = "386";
const ARCH_AMD64: &str = "amd64";
const ARCH_ARM: &str = "arm";
const ARCH_ARM64: &str = "arm64";

/// swarmkit's constraint operators, tried in this order.
const CONSTRAINT_OP_EQUAL: &str = "==";
const CONSTRAINT_OP_NOT_EQUAL: &str = "!=";
const NODE_LABELS_PREFIX: &str = "node.labels.";
const ENGINE_LABELS_PREFIX: &str = "engine.labels.";

/// `swarm_service_desired_replicas` and `swarm_service_schedulable_replicas`, labelled by
/// `stack`, `service`, `service_mode`, `display_name` and the custom labels.
///
/// The schedulable value is forced to 0 for jobs and restart-condition `none` services, whose
/// tasks the scheduler does not keep running: a job's tasks run to completion, so their raw
/// placement eligibility would make `running_replicas < schedulable_replicas` alerts fire for
/// good.
#[derive(Debug)]
pub struct ReplicaGauges {
    desired: Arc<GaugeVec>,
    schedulable: Arc<GaugeVec>,
}

impl ReplicaGauges {
    /// The two families, labelled with the base labels plus `custom_labels`.
    #[must_use]
    pub fn new(custom_labels: &CustomLabels) -> Self {
        let mut names = vec![
            LABEL_STACK.to_owned(),
            LABEL_SERVICE.to_owned(),
            LABEL_SERVICE_MODE.to_owned(),
            LABEL_DISPLAY_NAME.to_owned(),
        ];
        names.extend(custom_labels.sanitized_names());
        let names = sanitize_label_names(&names);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let family = |name: &str, help: &str| {
            Arc::new(GaugeVec::new(constant_desc(
                &format!("{NAMESPACE}_{SERVICE_SUBSYSTEM}_{name}"),
                help,
                Kind::Gauge,
                &names,
            )))
        };
        Self {
            desired: family("desired_replicas", DESIRED_REPLICAS_HELP),
            schedulable: family("schedulable_replicas", SCHEDULABLE_REPLICAS_HELP),
        }
    }

    /// Registers both families.
    ///
    /// # Errors
    ///
    /// The registry's refusal.
    pub fn register(&self, registry: &Registry) -> Result<(), RegisterError> {
        registry.register(Arc::clone(&self.desired) as Arc<dyn Collector>)?;
        registry.register(Arc::clone(&self.schedulable) as Arc<dyn Collector>)
    }

    /// The desired replicas family.
    #[must_use]
    pub fn desired(&self) -> &GaugeVec {
        &self.desired
    }

    /// The schedulable replicas family.
    #[must_use]
    pub fn schedulable(&self) -> &GaugeVec {
        &self.schedulable
    }

    /// Sets the desired replicas of the service `metadata` describes.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the metadata's custom labels are not the family's.
    pub fn set_desired(
        &self,
        metadata: &ServiceMetadata,
        value: f64,
    ) -> Result<(), LabelsMismatch> {
        self.desired.set_with(&labels_for_metadata(metadata), value)
    }

    /// Sets the schedulable replicas, forced to 0 for jobs and restart-condition `none`
    /// services.
    ///
    /// # Errors
    ///
    /// [`LabelsMismatch`] when the metadata's custom labels are not the family's.
    pub fn set_schedulable(
        &self,
        metadata: &ServiceMetadata,
        value: f64,
    ) -> Result<(), LabelsMismatch> {
        let value = if metadata.restart_condition_none || is_job_mode(&metadata.service_mode) {
            0.0
        } else {
            value
        };
        self.schedulable
            .set_with(&labels_for_metadata(metadata), value)
    }
}

/// The label set of the service `metadata` describes, keys sanitized, the same for setting and
/// deleting its series. A value that looks high-cardinality is warned about once per key.
#[must_use]
pub fn labels_for_metadata(metadata: &ServiceMetadata) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (LABEL_STACK.to_owned(), metadata.stack.clone()),
        (LABEL_SERVICE.to_owned(), metadata.service.clone()),
        (LABEL_SERVICE_MODE.to_owned(), metadata.service_mode.clone()),
        (
            LABEL_DISPLAY_NAME.to_owned(),
            display_name(&metadata.stack, &metadata.service),
        ),
    ]);
    for (key, value) in &metadata.custom_labels {
        labels.insert(key.clone(), value.clone());
        maybe_warn_high_cardinality(key, value);
    }
    sanitize_metric_labels(labels)
}

/// A service's desired and schedulable replicas from its metadata and a node snapshot:
/// replicated, the configured replicas and `min(configured, eligible nodes)`; replicated job,
/// the total completions twice; global and global job, the eligible-node count twice.
/// [`ReplicaGauges::set_schedulable`] then forces schedulable to 0 for jobs and
/// restart-condition `none` services.
#[must_use]
pub fn replica_counts(metadata: &ServiceMetadata, nodes: &[Node]) -> (f64, f64) {
    let placement = placement_from_metadata(metadata);
    let eligible = from_int(
        u64::try_from(count_eligible_nodes(nodes, placement.as_ref())).unwrap_or(u64::MAX),
    );
    match metadata.service_mode.as_str() {
        SERVICE_MODE_REPLICATED => (
            metadata.configured_replicas,
            metadata.configured_replicas.min(eligible),
        ),
        SERVICE_MODE_REPLICATED_JOB => (metadata.configured_replicas, metadata.configured_replicas),
        _ => (eligible, eligible),
    }
}

/// How many nodes of `nodes` a task with `placement` could be placed on.
#[must_use]
pub fn count_eligible_nodes(nodes: &[Node], placement: Option<&Placement>) -> usize {
    nodes
        .iter()
        .filter(|node| node_eligible(Some(node), placement))
        .count()
}

/// Whether a task with `placement` could be placed on `node`: the node is schedulable, matches
/// one of the required platforms (when there are any), and meets every constraint. No placement
/// only requires a schedulable node.
#[must_use]
pub fn node_eligible(node: Option<&Node>, placement: Option<&Placement>) -> bool {
    let Some(node) = node.filter(|node| is_node_schedulable(Some(node))) else {
        return false;
    };
    let Some(placement) = placement else {
        return true;
    };
    if !placement.platforms.is_empty() && !platform_matches(node, &placement.platforms) {
        return false;
    }
    constraints_match(node, &placement.constraints)
}

/// The placement cached in `metadata`, to evaluate eligibility without the service spec; none
/// when the service has neither constraints nor platforms.
#[must_use]
pub fn placement_from_metadata(metadata: &ServiceMetadata) -> Option<Placement> {
    if metadata.constraints.is_empty() && metadata.platforms.is_empty() {
        return None;
    }
    Some(Placement {
        constraints: metadata.constraints.clone(),
        platforms: metadata.platforms.clone(),
    })
}

/// Swarm's scheduling preconditions: the node is `ready` and `active` (not paused or drained).
#[must_use]
pub fn is_node_schedulable(node: Option<&Node>) -> bool {
    node.is_some_and(|node| {
        node.status.state == NODE_STATE_READY && node.spec.availability == NODE_AVAILABILITY_ACTIVE
    })
}

/// Maps the kernel-style architecture names nodes report (`uname -m`) to the Docker manifest
/// (GOARCH) names service platforms use, so a node reporting `x86_64` matches `amd64`.
#[must_use]
pub fn normalize_arch(arch: &str) -> String {
    let lower = to_lower(arch);
    match lower.as_str() {
        "x86_64" | "x86-64" | ARCH_AMD64 => ARCH_AMD64.to_owned(),
        "aarch64" | ARCH_ARM64 => ARCH_ARM64.to_owned(),
        "i386" | "i686" | ARCH_386 => ARCH_386.to_owned(),
        "armv7l" | "armv7" | "armhf" | ARCH_ARM => ARCH_ARM.to_owned(),
        _ => lower,
    }
}

/// Whether the node's platform matches any of `required`; an empty OS or architecture matches
/// any.
#[must_use]
pub fn platform_matches(node: &Node, required: &[Platform]) -> bool {
    let node_os = to_lower(&node.description.platform.os);
    let node_arch = normalize_arch(&node.description.platform.architecture);
    required.iter().any(|platform| {
        let os = to_lower(&platform.os);
        let arch = normalize_arch(&platform.architecture);
        (os.is_empty() || os == node_os) && (arch.is_empty() || arch == node_arch)
    })
}

/// Whether `node` meets every constraint, with swarmkit's `constraint.NodeMatches` semantics, so
/// the eligible-node count agrees with the scheduler. The keys (with `==` and `!=`):
/// `node.id`, `node.hostname`, `node.role`, `node.platform.os`, `node.platform.arch`,
/// `node.ip` (an address or a CIDR), `node.labels.<k>` and `engine.labels.<k>`.
///
/// Keys and values compare case-insensitively (label names after the prefix stay
/// case-sensitive), and a missing label is the empty string, so `node.labels.gpu != true`
/// matches unlabelled nodes. `node.platform.arch` compares the raw architecture the node reports
/// (`x86_64`), not [`normalize_arch`]'s, because Swarm does. An unknown key or a malformed
/// constraint never matches.
#[must_use]
pub fn constraints_match(node: &Node, constraints: &[String]) -> bool {
    constraints.iter().all(|constraint| {
        parse_constraint(constraint).is_some_and(|(key, operator, expected)| {
            node_matches_constraint(node, key, operator, expected)
        })
    })
}

/// Splits `key op value` as swarmkit does: `==` is tried before `!=`, and the expression is
/// split on the first occurrence of the first operator it contains.
fn parse_constraint(constraint: &str) -> Option<(&str, &'static str, &str)> {
    let operator = [CONSTRAINT_OP_EQUAL, CONSTRAINT_OP_NOT_EQUAL]
        .into_iter()
        .find(|operator| constraint.contains(operator))?;
    let (key, expected) = constraint.split_once(operator)?;
    let (key, expected) = (trim_space(key), trim_space(expected));
    (!key.is_empty() && !expected.is_empty()).then_some((key, operator, expected))
}

/// One parsed constraint against `node`, with swarmkit's key dispatch.
fn node_matches_constraint(node: &Node, key: &str, operator: &str, expected: &str) -> bool {
    let description = &node.description;
    let candidate = if equal_fold(key, "node.id") {
        &node.id
    } else if equal_fold(key, "node.hostname") {
        &description.hostname
    } else if equal_fold(key, "node.ip") {
        return match_node_ip_constraint(operator, expected, &node.status.addr);
    } else if equal_fold(key, "node.role") {
        &node.spec.role
    } else if equal_fold(key, "node.platform.os") {
        &description.platform.os
    } else if equal_fold(key, "node.platform.arch") {
        &description.platform.architecture
    } else if let Some(label) = label_key(key, NODE_LABELS_PREFIX) {
        // A missing label is "", as Swarm treats it.
        return match_constraint_value(
            operator,
            expected,
            node.spec.labels.get(label).map_or("", String::as_str),
        );
    } else if let Some(label) = label_key(key, ENGINE_LABELS_PREFIX) {
        return match_constraint_value(
            operator,
            expected,
            description
                .engine
                .labels
                .get(label)
                .map_or("", String::as_str),
        );
    } else {
        return false;
    };
    match_constraint_value(operator, expected, candidate)
}

/// The label name after `prefix`, when `key` is `prefix` (in any case) and a non-empty name: as
/// swarmkit, a bare `node.labels.` is an unknown key, not the empty label, which would make
/// `node.labels. != x` match every node.
fn label_key<'key>(key: &'key str, prefix: &str) -> Option<&'key str> {
    // A prefix cut inside a character cannot fold to the ASCII prefix, as in Go.
    if key.len() <= prefix.len() || !key.is_char_boundary(prefix.len()) {
        return None;
    }
    let (head, label) = key.split_at(prefix.len());
    equal_fold(head, prefix).then_some(label)
}

/// A case-insensitive comparison, inverted for `!=`, as swarmkit's `Constraint.Match`.
fn match_constraint_value(operator: &str, expected: &str, candidate: &str) -> bool {
    let matched = equal_fold(expected, candidate);
    if operator == CONSTRAINT_OP_NOT_EQUAL {
        !matched
    } else {
        matched
    }
}

/// The node address against one IP or a CIDR subnet, as swarmkit; a value that is neither never
/// matches, whatever the operator.
fn match_node_ip_constraint(operator: &str, expected: &str, node_addr: &str) -> bool {
    let node_ip = parse_ip(node_addr);
    let matched = if let Some(expected_ip) = parse_ip(expected) {
        ip_equal(&expected_ip, node_ip.as_ref())
    } else if let Some(subnet) = parse_cidr(expected) {
        subnet.contains(node_ip.as_ref())
    } else {
        return false;
    };
    if operator == CONSTRAINT_OP_NOT_EQUAL {
        !matched
    } else {
        matched
    }
}

#[cfg(test)]
mod tests;
