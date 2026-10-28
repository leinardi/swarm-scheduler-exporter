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
//! Decoding follows Go's `encoding/json` where the daemon's output can tell them apart: a missing
//! field or a JSON `null` is the zero value (`null_default`), unknown fields are ignored, a
//! timestamp is parsed as RFC 3339 into a [`GoTime`] whose zero is Go's `0001-01-01T00:00:00Z`,
//! and every state and other enum-like field is a plain `String`, so a value a newer daemon
//! introduces reaches the collectors' fallback rules instead of failing the whole list. A field
//! Go holds behind a pointer is an `Option`, so its presence stays visible.

use std::collections::BTreeMap;
use std::fmt;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};

use crate::go_compat::time_layout::{go_zero_time, is_go_zero_time};

use super::error::Error;

/// Labels, as the daemon sends them.
pub type Labels = BTreeMap<String, String>;

/// Go decodes a JSON `null` into the zero value; derived serde would refuse it.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

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
        is_go_zero_time(self.0)
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

impl<'de> Deserialize<'de> for GoTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(Self::default()),
            Some(text) => text.parse::<Timestamp>().map(Self).map_err(|error| {
                serde::de::Error::custom(format_args!("parsing time {text:?}: {error}"))
            }),
        }
    }
}

/// A JSON object whose presence is all that is read (Go's pointer to a struct it never looks
/// into).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Present {}

/// swarm `Version`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Version {
    /// `Index`.
    #[serde(rename = "Index", deserialize_with = "null_default")]
    pub index: u64,
}

/// swarm `Service`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Service {
    /// `ID`.
    #[serde(rename = "ID", deserialize_with = "null_default")]
    pub id: String,
    /// `Spec`.
    #[serde(rename = "Spec", deserialize_with = "null_default")]
    pub spec: ServiceSpec,
    /// `UpdateStatus`.
    #[serde(rename = "UpdateStatus")]
    pub update_status: Option<UpdateStatus>,
    /// `JobStatus`.
    #[serde(rename = "JobStatus")]
    pub job_status: Option<JobStatus>,
}

/// swarm `ServiceSpec`, with its `Annotations` inline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ServiceSpec {
    /// `Name`.
    #[serde(rename = "Name", deserialize_with = "null_default")]
    pub name: String,
    /// `Labels`.
    #[serde(rename = "Labels", deserialize_with = "null_default")]
    pub labels: Labels,
    /// `TaskTemplate`.
    #[serde(rename = "TaskTemplate", deserialize_with = "null_default")]
    pub task_template: TaskSpec,
    /// `Mode`.
    #[serde(rename = "Mode", deserialize_with = "null_default")]
    pub mode: ServiceMode,
}

/// swarm `TaskSpec`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TaskSpec {
    /// `Placement`.
    #[serde(rename = "Placement")]
    pub placement: Option<Placement>,
    /// `RestartPolicy`.
    #[serde(rename = "RestartPolicy")]
    pub restart_policy: Option<RestartPolicy>,
}

/// swarm `Placement`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Placement {
    /// `Constraints`.
    #[serde(rename = "Constraints", deserialize_with = "null_default")]
    pub constraints: Vec<String>,
    /// `Platforms`.
    #[serde(rename = "Platforms", deserialize_with = "null_default")]
    pub platforms: Vec<Platform>,
}

/// swarm `Platform`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Platform {
    /// `Architecture`.
    #[serde(rename = "Architecture", deserialize_with = "null_default")]
    pub architecture: String,
    /// `OS`.
    #[serde(rename = "OS", deserialize_with = "null_default")]
    pub os: String,
}

/// swarm `RestartPolicy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RestartPolicy {
    /// `Condition`: `none`, `on-failure`, `any`, or whatever a newer daemon sends.
    #[serde(rename = "Condition", deserialize_with = "null_default")]
    pub condition: String,
}

/// swarm `ServiceMode`: which of the modes is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ServiceMode {
    /// `Replicated`.
    #[serde(rename = "Replicated")]
    pub replicated: Option<ReplicatedService>,
    /// `Global`.
    #[serde(rename = "Global")]
    pub global: Option<Present>,
    /// `ReplicatedJob`.
    #[serde(rename = "ReplicatedJob")]
    pub replicated_job: Option<ReplicatedJob>,
    /// `GlobalJob`.
    #[serde(rename = "GlobalJob")]
    pub global_job: Option<Present>,
}

/// swarm `ReplicatedService`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReplicatedService {
    /// `Replicas`.
    #[serde(rename = "Replicas")]
    pub replicas: Option<u64>,
}

/// swarm `ReplicatedJob`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReplicatedJob {
    /// `MaxConcurrent`.
    #[serde(rename = "MaxConcurrent")]
    pub max_concurrent: Option<u64>,
    /// `TotalCompletions`.
    #[serde(rename = "TotalCompletions")]
    pub total_completions: Option<u64>,
}

/// swarm `UpdateStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UpdateStatus {
    /// `State`: `updating`, `paused`, `completed`, `rollback_started`, …
    #[serde(rename = "State", deserialize_with = "null_default")]
    pub state: String,
    /// `StartedAt`.
    #[serde(rename = "StartedAt")]
    pub started_at: Option<GoTime>,
    /// `CompletedAt`.
    #[serde(rename = "CompletedAt")]
    pub completed_at: Option<GoTime>,
}

/// swarm `JobStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct JobStatus {
    /// `JobIteration`.
    #[serde(rename = "JobIteration", deserialize_with = "null_default")]
    pub job_iteration: Version,
}

/// swarm `Node`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Node {
    /// `ID`.
    #[serde(rename = "ID", deserialize_with = "null_default")]
    pub id: String,
    /// `Spec`.
    #[serde(rename = "Spec", deserialize_with = "null_default")]
    pub spec: NodeSpec,
    /// `Description`.
    #[serde(rename = "Description", deserialize_with = "null_default")]
    pub description: NodeDescription,
    /// `Status`.
    #[serde(rename = "Status", deserialize_with = "null_default")]
    pub status: NodeStatus,
}

/// swarm `NodeSpec`, with its `Annotations` inline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NodeSpec {
    /// `Labels`.
    #[serde(rename = "Labels", deserialize_with = "null_default")]
    pub labels: Labels,
    /// `Role`: `manager`, `worker`, …
    #[serde(rename = "Role", deserialize_with = "null_default")]
    pub role: String,
    /// `Availability`: `active`, `pause`, `drain`, …
    #[serde(rename = "Availability", deserialize_with = "null_default")]
    pub availability: String,
}

/// swarm `NodeDescription`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NodeDescription {
    /// `Hostname`.
    #[serde(rename = "Hostname", deserialize_with = "null_default")]
    pub hostname: String,
    /// `Platform`.
    #[serde(rename = "Platform", deserialize_with = "null_default")]
    pub platform: Platform,
    /// `Engine`.
    #[serde(rename = "Engine", deserialize_with = "null_default")]
    pub engine: EngineDescription,
}

/// swarm `EngineDescription`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct EngineDescription {
    /// `Labels`.
    #[serde(rename = "Labels", deserialize_with = "null_default")]
    pub labels: Labels,
}

/// swarm `NodeStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NodeStatus {
    /// `State`: `unknown`, `down`, `ready`, `disconnected`, …
    #[serde(rename = "State", deserialize_with = "null_default")]
    pub state: String,
    /// `Addr`.
    #[serde(rename = "Addr", deserialize_with = "null_default")]
    pub addr: String,
}

/// swarm `Task`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Task {
    /// `Version`.
    #[serde(rename = "Version", deserialize_with = "null_default")]
    pub version: Version,
    /// `CreatedAt`.
    #[serde(rename = "CreatedAt", deserialize_with = "null_default")]
    pub created_at: GoTime,
    /// `ServiceID`.
    #[serde(rename = "ServiceID", deserialize_with = "null_default")]
    pub service_id: String,
    /// `Slot`.
    #[serde(rename = "Slot", deserialize_with = "null_default")]
    pub slot: i64,
    /// `NodeID`.
    #[serde(rename = "NodeID", deserialize_with = "null_default")]
    pub node_id: String,
    /// `Status`.
    #[serde(rename = "Status", deserialize_with = "null_default")]
    pub status: TaskStatus,
    /// `DesiredState`.
    #[serde(rename = "DesiredState", deserialize_with = "null_default")]
    pub desired_state: String,
    /// `JobIteration`.
    #[serde(rename = "JobIteration")]
    pub job_iteration: Option<Version>,
}

/// swarm `TaskStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TaskStatus {
    /// `Timestamp`.
    #[serde(rename = "Timestamp", deserialize_with = "null_default")]
    pub timestamp: GoTime,
    /// `State`: `new`, `pending`, `running`, `complete`, `failed`, …
    #[serde(rename = "State", deserialize_with = "null_default")]
    pub state: String,
}

/// container `Summary`, one entry of the container list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContainerSummary {
    /// `Id`.
    #[serde(rename = "Id", deserialize_with = "null_default")]
    pub id: String,
    /// `Names`.
    #[serde(rename = "Names", deserialize_with = "null_default")]
    pub names: Vec<String>,
    /// `Labels`.
    #[serde(rename = "Labels", deserialize_with = "null_default")]
    pub labels: Labels,
    /// `State`: `created`, `running`, `paused`, `restarting`, `exited`, …
    #[serde(rename = "State", deserialize_with = "null_default")]
    pub state: String,
}

/// container `InspectResponse`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContainerInspect {
    /// `State`.
    #[serde(rename = "State")]
    pub state: Option<ContainerState>,
    /// `Config`.
    #[serde(rename = "Config")]
    pub config: Option<ContainerConfig>,
}

/// container `State`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContainerState {
    /// `ExitCode`.
    #[serde(rename = "ExitCode", deserialize_with = "null_default")]
    pub exit_code: i64,
    /// `Health`.
    #[serde(rename = "Health")]
    pub health: Option<Health>,
}

/// container `Health`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Health {
    /// `Status`: `none`, `starting`, `healthy`, `unhealthy`, …
    #[serde(rename = "Status", deserialize_with = "null_default")]
    pub status: String,
}

/// container `Config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContainerConfig {
    /// `Healthcheck`: only whether one is configured is read.
    #[serde(rename = "Healthcheck")]
    pub healthcheck: Option<Present>,
}

/// events `Message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Event {
    /// `Type`: `service`, `node`, … (the stream is filtered to these two).
    #[serde(rename = "Type", deserialize_with = "null_default")]
    pub kind: String,
    /// `Action`.
    #[serde(rename = "Action", deserialize_with = "null_default")]
    pub action: String,
    /// `Actor`.
    #[serde(rename = "Actor", deserialize_with = "null_default")]
    pub actor: Actor,
    /// `time`: seconds since the epoch.
    #[serde(rename = "time", deserialize_with = "null_default")]
    pub time: i64,
    /// `timeNano`: nanoseconds since the epoch.
    #[serde(rename = "timeNano", deserialize_with = "null_default")]
    pub time_nano: i64,
}

/// events `Actor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Actor {
    /// `ID`.
    #[serde(rename = "ID", deserialize_with = "null_default")]
    pub id: String,
}

/// Decodes the first JSON value of `body` the way Go's `json.NewDecoder(body).Decode(&out)`
/// does: leading whitespace skipped, whatever follows the value ignored, a `null` the zero value,
/// and an empty body `EOF`.
pub(crate) fn decode_first<T>(body: &[u8]) -> Result<T, Error>
where
    T: Default + for<'de> Deserialize<'de>,
{
    match serde_json::Deserializer::from_slice(body)
        .into_iter::<Option<T>>()
        .next()
    {
        None => Err(Error::decode("EOF".to_owned())),
        Some(Ok(value)) => Ok(value.unwrap_or_default()),
        Some(Err(error)) => {
            let message = if error.is_eof() {
                "unexpected EOF".to_owned()
            } else {
                error.to_string()
            };
            Err(Error::decode(message).with_source(error))
        }
    }
}

/// Decodes a list the way Go decodes it into a slice: `null` is an empty list and a `null`
/// element the zero value.
pub(crate) fn decode_list<T>(body: &[u8]) -> Result<Vec<T>, Error>
where
    T: Default + for<'de> Deserialize<'de>,
{
    decode_first::<Vec<Nullable<T>>>(body)
        .map(|items| items.into_iter().map(|item| item.0).collect())
}

/// A list element Go would decode from `null` into the zero value.
#[derive(Debug, Default)]
struct Nullable<T>(T);

impl<'de, T: Default + Deserialize<'de>> Deserialize<'de> for Nullable<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        null_default(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests;
