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

//! bollard's models into the exporter's [`types`](super::types): only the fields the collectors
//! read, a missing field or a JSON `null` the zero value, and every enum-like field its wire
//! text, so the collectors' fallback rules keep deciding what an unfamiliar state means.
//!
//! bollard decodes the enums it knows and refuses the rest: a state a newer daemon introduces
//! fails the call that carries it (a `decode` error from the call), where the Go exporter, and
//! this one before `docker-client`, passed it on as text.

use std::collections::HashMap;
use std::fmt::Display;

use docker_client::bollard::models as wire;
use jiff::Timestamp;

use super::error::{Error, ErrorKind};
use super::types::{
    Actor, ContainerConfig, ContainerInspect, ContainerState, ContainerSummary, EngineDescription,
    Event, GoTime, Health, JobStatus, Labels, Node, NodeDescription, NodeSpec, NodeStatus,
    Placement, Platform, Present, ReplicatedJob, ReplicatedService, RestartPolicy, Service,
    ServiceMode, ServiceSpec, Task, TaskSpec, TaskStatus, UpdateStatus, Version,
};

/// The wire text of an enum, empty when it is missing.
fn text<T: Display>(value: Option<T>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn labels(labels: Option<HashMap<String, String>>) -> Labels {
    labels.unwrap_or_default().into_iter().collect()
}

fn count(value: Option<i64>) -> Option<u64> {
    value.map(|value| u64::try_from(value).unwrap_or_default())
}

fn go_time(value: Option<String>) -> Result<GoTime, Error> {
    match value {
        None => Ok(GoTime::default()),
        Some(text) => text.parse::<Timestamp>().map(GoTime::new).map_err(|error| {
            Error::new(ErrorKind::Other, format!("parsing time {text:?}: {error}"))
        }),
    }
}

fn optional_time(value: Option<String>) -> Result<Option<GoTime>, Error> {
    value.map(|value| go_time(Some(value))).transpose()
}

fn version(value: Option<wire::ObjectVersion>) -> Version {
    Version {
        index: value.and_then(|value| value.index).unwrap_or_default(),
    }
}

fn platform(value: Option<wire::Platform>) -> Platform {
    let value = value.unwrap_or_default();
    Platform {
        architecture: value.architecture.unwrap_or_default(),
        os: value.os.unwrap_or_default(),
    }
}

fn task_spec(value: Option<wire::TaskSpec>) -> TaskSpec {
    let value = value.unwrap_or_default();
    TaskSpec {
        placement: value.placement.map(|placement| Placement {
            constraints: placement.constraints.unwrap_or_default(),
            platforms: placement
                .platforms
                .unwrap_or_default()
                .into_iter()
                .map(|value| platform(Some(value)))
                .collect(),
        }),
        restart_policy: value.restart_policy.map(|policy| RestartPolicy {
            condition: text(policy.condition),
        }),
    }
}

fn mode(value: Option<wire::ServiceSpecMode>) -> ServiceMode {
    let value = value.unwrap_or_default();
    ServiceMode {
        replicated: value.replicated.map(|replicated| ReplicatedService {
            replicas: count(replicated.replicas),
        }),
        global: value.global.map(|_| Present {}),
        replicated_job: value.replicated_job.map(|job| ReplicatedJob {
            max_concurrent: count(job.max_concurrent),
            total_completions: count(job.total_completions),
        }),
        global_job: value.global_job.map(|_| Present {}),
    }
}

/// A swarm service, from `GET /services` or `GET /services/{id}`.
///
/// # Errors
///
/// A timestamp that is not RFC 3339.
pub fn service(value: wire::Service) -> Result<Service, Error> {
    let spec = value.spec.unwrap_or_default();
    let update_status = value
        .update_status
        .map(|status| {
            Ok::<_, Error>(UpdateStatus {
                state: text(status.state),
                started_at: optional_time(status.started_at)?,
                completed_at: optional_time(status.completed_at)?,
            })
        })
        .transpose()?;
    Ok(Service {
        id: value.id.unwrap_or_default(),
        spec: ServiceSpec {
            name: spec.name.unwrap_or_default(),
            labels: labels(spec.labels),
            task_template: task_spec(spec.task_template),
            mode: mode(spec.mode),
        },
        update_status,
        job_status: value.job_status.map(|status| JobStatus {
            job_iteration: version(status.job_iteration),
        }),
    })
}

/// A swarm node, from `GET /nodes`.
pub fn node(value: wire::Node) -> Node {
    let spec = value.spec.unwrap_or_default();
    let description = value.description.unwrap_or_default();
    let status = value.status.unwrap_or_default();
    Node {
        id: value.id.unwrap_or_default(),
        spec: NodeSpec {
            labels: labels(spec.labels),
            role: text(spec.role),
            availability: text(spec.availability),
        },
        description: NodeDescription {
            hostname: description.hostname.unwrap_or_default(),
            platform: platform(description.platform),
            engine: EngineDescription {
                labels: labels(description.engine.and_then(|engine| engine.labels)),
            },
        },
        status: NodeStatus {
            state: text(status.state),
            addr: status.addr.unwrap_or_default(),
        },
    }
}

/// A swarm task, from `GET /tasks`.
///
/// # Errors
///
/// A timestamp that is not RFC 3339.
pub fn task(value: wire::Task) -> Result<Task, Error> {
    let status = value.status.unwrap_or_default();
    Ok(Task {
        version: version(value.version),
        created_at: go_time(value.created_at)?,
        service_id: value.service_id.unwrap_or_default(),
        slot: value.slot.unwrap_or_default(),
        node_id: value.node_id.unwrap_or_default(),
        status: TaskStatus {
            timestamp: go_time(status.timestamp)?,
            state: text(status.state),
        },
        desired_state: text(value.desired_state),
        job_iteration: value.job_iteration.map(|value| version(Some(value))),
    })
}

/// A container of `GET /containers/json`.
pub fn container_summary(value: wire::ContainerSummary) -> ContainerSummary {
    ContainerSummary {
        id: value.id.unwrap_or_default(),
        names: value.names.unwrap_or_default(),
        labels: labels(value.labels),
        state: text(value.state),
    }
}

/// `GET /containers/{id}/json`: the state and whether a healthcheck is configured.
pub fn container_inspect(value: wire::ContainerInspectResponse) -> ContainerInspect {
    ContainerInspect {
        state: value.state.map(|state| ContainerState {
            exit_code: state.exit_code.unwrap_or_default(),
            health: state.health.map(|health| Health {
                status: text(health.status),
            }),
        }),
        config: value.config.map(|config| ContainerConfig {
            healthcheck: config.healthcheck.map(|_| Present {}),
        }),
    }
}

/// An event of `GET /events`.
pub fn event(value: wire::EventMessage) -> Event {
    Event {
        kind: text(value.typ),
        action: value.action.unwrap_or_default(),
        actor: Actor {
            id: value.actor.and_then(|actor| actor.id).unwrap_or_default(),
        },
        time: value.time.unwrap_or_default(),
        time_nano: value.time_nano.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;
