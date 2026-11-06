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

//! The Engine API objects the exporter reads, with only the fields it uses.
//!
//! Built by `convert` from bollard's models: a missing field or a `null` is the
//! zero value, a timestamp is parsed as RFC 3339 into a [`GoTime`] whose zero is Go's
//! `0001-01-01T00:00:00Z`, and every state and other enum-like field is its wire text. A field Go
//! holds behind a pointer is an `Option`, so its presence stays visible.

use std::fmt;

use jiff::Timestamp;

/// Go's zero `time.Time`, `0001-01-01T00:00:00Z`, in seconds since the epoch.
const GO_ZERO_SECONDS: i64 = -62_135_596_800;

fn go_zero_time() -> Timestamp {
    Timestamp::from_second(GO_ZERO_SECONDS).unwrap_or(Timestamp::MIN)
}

/// Labels, as the daemon sends them.
pub type Labels = std::collections::BTreeMap<String, String>;

/// A Go `time.Time` decoded from the wire: an instant, Go's zero time when missing or `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GoTime(Timestamp);

impl GoTime {
    /// The time at `timestamp`.
    #[must_use]
    pub fn new(timestamp: Timestamp) -> Self {
        Self(timestamp)
    }

    /// The instant.
    #[must_use]
    pub fn timestamp(self) -> Timestamp {
        self.0
    }

    /// Go's `Time.IsZero`.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == go_zero_time()
    }

    /// Go's `Time.Unix`: whole seconds since the epoch, rounded down.
    #[must_use]
    pub fn unix(self) -> i64 {
        let seconds = self.0.as_second();
        if self.0.subsec_nanosecond() < 0 {
            seconds - 1
        } else {
            seconds
        }
    }
}

impl Default for GoTime {
    fn default() -> Self {
        Self(go_zero_time())
    }
}

impl fmt::Display for GoTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A JSON object whose presence is all that is read (Go's pointer to a struct it never looks
/// into).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Present {}

/// swarm `Version`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Version {
    /// `Index`.
    pub index: u64,
}

/// swarm `Service`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Service {
    /// `ID`.
    pub id: String,
    /// `Spec`.
    pub spec: ServiceSpec,
    /// `UpdateStatus`.
    pub update_status: Option<UpdateStatus>,
    /// `JobStatus`.
    pub job_status: Option<JobStatus>,
}

/// swarm `ServiceSpec`, with its `Annotations` inline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceSpec {
    /// `Name`.
    pub name: String,
    /// `Labels`.
    pub labels: Labels,
    /// `TaskTemplate`.
    pub task_template: TaskSpec,
    /// `Mode`.
    pub mode: ServiceMode,
}

/// swarm `TaskSpec`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSpec {
    /// `Placement`.
    pub placement: Option<Placement>,
    /// `RestartPolicy`.
    pub restart_policy: Option<RestartPolicy>,
}

/// swarm `Placement`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// `Constraints`.
    pub constraints: Vec<String>,
    /// `Platforms`.
    pub platforms: Vec<Platform>,
}

/// swarm `Platform`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Platform {
    /// `Architecture`.
    pub architecture: String,
    /// `OS`.
    pub os: String,
}

/// swarm `RestartPolicy`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestartPolicy {
    /// `Condition`: `none`, `on-failure`, `any`, or whatever a newer daemon sends.
    pub condition: String,
}

/// swarm `ServiceMode`: which of the modes is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceMode {
    /// `Replicated`.
    pub replicated: Option<ReplicatedService>,
    /// `Global`.
    pub global: Option<Present>,
    /// `ReplicatedJob`.
    pub replicated_job: Option<ReplicatedJob>,
    /// `GlobalJob`.
    pub global_job: Option<Present>,
}

/// swarm `ReplicatedService`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplicatedService {
    /// `Replicas`.
    pub replicas: Option<u64>,
}

/// swarm `ReplicatedJob`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplicatedJob {
    /// `MaxConcurrent`.
    pub max_concurrent: Option<u64>,
    /// `TotalCompletions`.
    pub total_completions: Option<u64>,
}

/// swarm `UpdateStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateStatus {
    /// `State`: `updating`, `paused`, `completed`, `rollback_started`, …
    pub state: String,
    /// `StartedAt`.
    pub started_at: Option<GoTime>,
    /// `CompletedAt`.
    pub completed_at: Option<GoTime>,
}

/// swarm `JobStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobStatus {
    /// `JobIteration`.
    pub job_iteration: Version,
}

/// swarm `Node`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// `ID`.
    pub id: String,
    /// `Spec`.
    pub spec: NodeSpec,
    /// `Description`.
    pub description: NodeDescription,
    /// `Status`.
    pub status: NodeStatus,
}

/// swarm `NodeSpec`, with its `Annotations` inline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeSpec {
    /// `Labels`.
    pub labels: Labels,
    /// `Role`: `manager`, `worker`, …
    pub role: String,
    /// `Availability`: `active`, `pause`, `drain`, …
    pub availability: String,
}

/// swarm `NodeDescription`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeDescription {
    /// `Hostname`.
    pub hostname: String,
    /// `Platform`.
    pub platform: Platform,
    /// `Engine`.
    pub engine: EngineDescription,
}

/// swarm `EngineDescription`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineDescription {
    /// `Labels`.
    pub labels: Labels,
}

/// swarm `NodeStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeStatus {
    /// `State`: `unknown`, `down`, `ready`, `disconnected`, …
    pub state: String,
    /// `Addr`.
    pub addr: String,
}

/// swarm `Task`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Task {
    /// `Version`.
    pub version: Version,
    /// `CreatedAt`.
    pub created_at: GoTime,
    /// `ServiceID`.
    pub service_id: String,
    /// `Slot`.
    pub slot: i64,
    /// `NodeID`.
    pub node_id: String,
    /// `Status`.
    pub status: TaskStatus,
    /// `DesiredState`.
    pub desired_state: String,
    /// `JobIteration`.
    pub job_iteration: Option<Version>,
}

/// swarm `TaskStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskStatus {
    /// `Timestamp`.
    pub timestamp: GoTime,
    /// `State`: `new`, `pending`, `running`, `complete`, `failed`, …
    pub state: String,
}

/// container `Summary`, one entry of the container list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerSummary {
    /// `Id`.
    pub id: String,
    /// `Names`.
    pub names: Vec<String>,
    /// `Labels`.
    pub labels: Labels,
    /// `State`: `created`, `running`, `paused`, `restarting`, `exited`, …
    pub state: String,
}

/// container `InspectResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerInspect {
    /// `State`.
    pub state: Option<ContainerState>,
    /// `Config`.
    pub config: Option<ContainerConfig>,
}

/// container `State`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerState {
    /// `ExitCode`.
    pub exit_code: i64,
    /// `Health`.
    pub health: Option<Health>,
}

/// container `Health`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// `Status`: `none`, `starting`, `healthy`, `unhealthy`, …
    pub status: String,
}

/// container `Config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerConfig {
    /// `Healthcheck`: only whether one is configured is read.
    pub healthcheck: Option<Present>,
}

/// events `Message`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    /// `Type`: `service`, `node`, … (the stream is filtered to these two).
    pub kind: String,
    /// `Action`.
    pub action: String,
    /// `Actor`.
    pub actor: Actor,
    /// `time`: seconds since the epoch.
    pub time: i64,
    /// `timeNano`: nanoseconds since the epoch.
    pub time_nano: i64,
}

/// events `Actor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actor {
    /// `ID`.
    pub id: String,
}

#[cfg(test)]
mod tests;
