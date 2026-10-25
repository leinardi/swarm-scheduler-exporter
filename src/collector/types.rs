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

//! The metadata the exporter keeps per service to label its series, the caches that hold it and
//! the node snapshot, and the naming rules for stacks and services.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::docker::types::{Node, Platform, ReplicatedJob, Service};
use crate::gofmt_compat::float::from_int;

use super::metrics_ids::{
    SERVICE_MODE_GLOBAL, SERVICE_MODE_GLOBAL_JOB, SERVICE_MODE_REPLICATED,
    SERVICE_MODE_REPLICATED_JOB,
};

/// The service label holding the stack a service was deployed with.
pub const STACK_NAMESPACE_LABEL: &str = "com.docker.stack.namespace";

/// The restart condition of a one-shot service.
const RESTART_CONDITION_NONE: &str = "none";

/// A user-requested custom label: the service label key as it appears in Swarm, and the
/// Prometheus label name it is exported as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomLabel {
    /// The service label key.
    pub raw: String,
    /// The sanitized label name.
    pub sanitized: String,
}

/// The custom labels, in the order they were requested (Go's `SetCustomLabels`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomLabels {
    labels: Vec<CustomLabel>,
}

impl CustomLabels {
    /// Pairs `raw_keys` with `sanitized_keys`, which have the same length and order.
    #[must_use]
    pub fn new(raw_keys: &[String], sanitized_keys: &[String]) -> Self {
        Self {
            labels: raw_keys
                .iter()
                .zip(sanitized_keys)
                .map(|(raw, sanitized)| CustomLabel {
                    raw: raw.clone(),
                    sanitized: sanitized.clone(),
                })
                .collect(),
        }
    }

    /// The sanitized label names, for the family descriptors.
    #[must_use]
    pub fn sanitized_names(&self) -> Vec<String> {
        self.labels
            .iter()
            .map(|label| label.sanitized.clone())
            .collect()
    }
}

/// What the exporter keeps per service to populate its labels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceMetadata {
    /// The Docker stack, from the `com.docker.stack.namespace` label.
    pub stack: String,
    /// The visible service name, without the `<stack>_` prefix.
    pub service: String,
    /// `replicated`, `global`, `replicated-job` or `global-job`.
    pub service_mode: String,
    /// Sanitized label name to the service's label value, `""` when the service lacks it.
    pub custom_labels: BTreeMap<String, String>,
    /// The desired replicas last computed for the service.
    pub desired_replicas: f64,
    /// Replicated: the configured replicas; replicated job: the total completions.
    pub configured_replicas: f64,
    /// Jobs: `JobStatus.JobIteration.Index` of the current run.
    pub job_iteration: u64,
    /// The placement constraint expressions (`node.hostname == pollux`).
    pub constraints: Vec<String>,
    /// The placement platform requirements.
    pub platforms: Vec<Platform>,
    /// Whether `RestartPolicy.Condition` is `none` (one-shot and cron-like services).
    pub restart_condition_none: bool,
}

/// Builds the metadata of `service`, with a value (or `""`) for every custom label.
///
/// # Panics
///
/// For a replicated service without `Replicas`, where Go dereferences a nil pointer; the daemon
/// always sets it, and the reconciler recovers the panic as Go's does.
#[must_use]
pub fn build_metadata(service: &Service, custom_labels: &CustomLabels) -> ServiceMetadata {
    let spec = &service.spec;
    let stack = spec
        .labels
        .get(STACK_NAMESPACE_LABEL)
        .cloned()
        .unwrap_or_default();
    let mut metadata = ServiceMetadata {
        service: short_service_name(&stack, &spec.name).to_owned(),
        stack,
        service_mode: service_mode(service).to_owned(),
        ..ServiceMetadata::default()
    };
    if let Some(replicated) = &spec.mode.replicated {
        let replicas = replicated
            .replicas
            .unwrap_or_else(|| panic!("replicated service {:?} has no Replicas", service.id));
        metadata.configured_replicas = from_int(replicas);
    } else if let Some(job) = &spec.mode.replicated_job {
        metadata.configured_replicas = job_total_completions(job);
    }
    if let Some(job_status) = &service.job_status {
        metadata.job_iteration = job_status.job_iteration.index;
    }
    if let Some(placement) = &spec.task_template.placement {
        metadata.constraints.clone_from(&placement.constraints);
        metadata.platforms.clone_from(&placement.platforms);
    }
    metadata.restart_condition_none = spec
        .task_template
        .restart_policy
        .as_ref()
        .is_some_and(|policy| policy.condition == RESTART_CONDITION_NONE);
    for label in &custom_labels.labels {
        // Every custom label exists, empty when the service lacks it (exhaustive emission).
        let value = spec.labels.get(&label.raw).cloned().unwrap_or_default();
        metadata
            .custom_labels
            .insert(label.sanitized.clone(), value);
    }
    metadata
}

/// The effective mode of `service`: `global` unless another mode is set.
#[must_use]
pub fn service_mode(service: &Service) -> &'static str {
    let mode = &service.spec.mode;
    if mode.replicated.is_some() {
        SERVICE_MODE_REPLICATED
    } else if mode.replicated_job.is_some() {
        SERVICE_MODE_REPLICATED_JOB
    } else if mode.global_job.is_some() {
        SERVICE_MODE_GLOBAL_JOB
    } else {
        SERVICE_MODE_GLOBAL
    }
}

/// Whether `mode` is a job mode, whose tasks run to completion instead of being kept running.
#[must_use]
pub fn is_job_mode(mode: &str) -> bool {
    mode == SERVICE_MODE_REPLICATED_JOB || mode == SERVICE_MODE_GLOBAL_JOB
}

/// How many tasks a replicated job must complete: `TotalCompletions`, else `MaxConcurrent`,
/// else 1, the defaults documented on the API's `ReplicatedJob`. The daemon fills both in on
/// create, so the fallbacks only guard against a spec that lacks them.
#[must_use]
pub fn job_total_completions(job: &ReplicatedJob) -> f64 {
    job.total_completions
        .or(job.max_concurrent)
        .map_or(1.0, from_int)
}

/// The visible service name without the `<stack>_` prefix, when there is a stack and the name
/// carries it; otherwise the name as it is.
#[must_use]
pub fn short_service_name<'name>(stack: &str, full_name: &'name str) -> &'name str {
    if stack.is_empty() {
        return full_name;
    }
    full_name
        .strip_prefix(stack)
        .and_then(|rest| rest.strip_prefix('_'))
        .unwrap_or(full_name)
}

/// A friendly label: the stack when it equals the service, the service without a stack, else
/// `<stack> <service>`.
#[must_use]
pub fn display_name(stack: &str, service: &str) -> String {
    if stack == service {
        stack.to_owned()
    } else if stack.is_empty() {
        service.to_owned()
    } else {
        format!("{stack} {service}")
    }
}

/// The service metadata, keyed by service ID: the Go exporter's `metadataCache`. The reconciler
/// owns it and is its only writer, so it needs no lock (Go's `metadataMu`).
#[derive(Debug, Default)]
pub struct MetadataCache {
    entries: HashMap<String, ServiceMetadata>,
}

impl MetadataCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `metadata`, keeping the desired replicas of an entry already there.
    pub fn set(&mut self, service_id: &str, mut metadata: ServiceMetadata) {
        if let Some(previous) = self.entries.get(service_id) {
            metadata.desired_replicas = previous.desired_replicas;
        }
        self.entries.insert(service_id.to_owned(), metadata);
    }

    /// Stores `metadata` as it is, desired replicas included.
    pub fn replace(&mut self, service_id: &str, metadata: ServiceMetadata) {
        self.entries.insert(service_id.to_owned(), metadata);
    }

    /// The metadata of `service_id`.
    #[must_use]
    pub fn get(&self, service_id: &str) -> Option<&ServiceMetadata> {
        self.entries.get(service_id)
    }

    /// Forgets `service_id`.
    pub fn delete(&mut self, service_id: &str) {
        self.entries.remove(service_id);
    }

    /// Updates the desired replicas of a known service; an unknown one is left alone.
    pub fn set_desired_replicas(&mut self, service_id: &str, desired: f64) {
        if let Some(metadata) = self.entries.get_mut(service_id) {
            metadata.desired_replicas = desired;
        }
    }

    /// A point-in-time copy of every entry.
    #[must_use]
    pub fn all(&self) -> HashMap<String, ServiceMetadata> {
        self.entries.clone()
    }

    /// The cached service IDs.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// How many services are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no service is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The latest node snapshot: the Go exporter's `cachedNodes`. Owned by the reconciler, like the
/// metadata, so it needs no lock (Go's `nodesMu`); shared as an `Arc`, so a step reading it
/// while it writes the caches takes no copy.
#[derive(Debug, Default)]
pub struct NodeCache {
    nodes: Arc<[Node]>,
}

impl NodeCache {
    /// An empty snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the snapshot with a copy of `nodes`.
    pub fn set(&mut self, nodes: &[Node]) {
        self.nodes = nodes.into();
    }

    /// The snapshot.
    #[must_use]
    pub fn get(&self) -> &[Node] {
        &self.nodes
    }

    /// The snapshot, shared.
    #[must_use]
    pub fn shared(&self) -> Arc<[Node]> {
        Arc::clone(&self.nodes)
    }
}

#[cfg(test)]
mod tests;
