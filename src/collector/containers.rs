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

//! `swarm_container_state`, opt-in: an info-style gauge per container with a `state` label,
//! over Docker's runtime states plus the health overlay states, and the exit code as a label
//! when the container exited (empty otherwise).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::docker::types::{ContainerInspect, Labels};
use crate::docker::{
    CONTAINERS_INSPECT_TIMEOUT, CONTAINERS_LIST_TIMEOUT, DockerApi, with_deadline, with_timeout,
};
use crate::gofmt_compat::strings::{equal_fold, to_lower, trim_space};
use crate::labels::{sanitize_label_names, sanitize_metric_labels};
use crate::logger::{Attr, Logger};
use crate::metrics::{
    Collector, Kind, RegisterError, Registry, SnapshotBuilder, SnapshotFamily, constant_desc,
};

use super::error::CallError;
use super::metrics_ids::{
    LABEL_CONTAINER, LABEL_DISPLAY_NAME, LABEL_SERVICE, LABEL_STACK, LABEL_STATE, NAMESPACE,
};
use super::types::{STACK_NAMESPACE_LABEL, short_service_name};

/// Every state emitted per container: Docker's runtime states, then the health overlay of a
/// running container with a healthcheck.
pub const KNOWN_CONTAINER_STATES: [&str; 10] = [
    "created",
    "restarting",
    CONTAINER_STATE_RUNNING,
    "removing",
    "paused",
    CONTAINER_STATE_EXITED,
    "dead",
    CONTAINER_STATE_HEALTHY,
    CONTAINER_STATE_UNHEALTHY,
    CONTAINER_STATE_HEALTH_STARTING,
];

const CONTAINER_STATE_RUNNING: &str = "running";
const CONTAINER_STATE_EXITED: &str = "exited";
const CONTAINER_STATE_HEALTH_STARTING: &str = "health_starting";
const CONTAINER_STATE_HEALTHY: &str = "healthy";
const CONTAINER_STATE_UNHEALTHY: &str = "unhealthy";

/// The `project` label: the Compose project, or empty.
pub const LABEL_PROJECT: &str = "project";
/// The `orchestrator` label: `compose`, `swarm` or `none`.
pub const LABEL_ORCHESTRATOR: &str = "orchestrator";
/// The `exit_code` label: the exit code of an exited container, else empty.
pub const LABEL_EXIT_CODE: &str = "exit_code";

/// The `orchestrator` of a container with a Compose project.
pub const ORCHESTRATOR_COMPOSE: &str = "compose";
/// The `orchestrator` of a Swarm task's container.
pub const ORCHESTRATOR_SWARM: &str = "swarm";
/// The `orchestrator` of any other container.
pub const ORCHESTRATOR_NONE: &str = "none";

const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";
const SWARM_SERVICE_NAME_LABEL: &str = "com.docker.swarm.service.name";

/// The inspects one poll may attempt, failed ones included: each is still a round trip to the
/// daemon. Only running containers (for their health) and exited ones (for their exit code)
/// are inspected.
pub const CONTAINERS_INSPECT_CAP: usize = 300;

/// The family name.
pub const CONTAINER_STATE_NAME: &str = "swarm_container_state";
/// The HELP text.
pub const CONTAINER_STATE_HELP: &str =
    "Container state info (exactly one state=1 per container). Includes exit_code for exited.";

/// The `--containers` and `--containers-include-swarm` settings, with the poll's bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainersConfig {
    /// When false, the collector is inert: no list, no inspect, nothing published.
    pub enabled: bool,
    /// Whether Swarm task containers are included; off by default, as the task metrics already
    /// cover them.
    pub include_swarm: bool,
    /// The inspects one poll may attempt.
    pub inspect_cap: usize,
    /// The deadline all the inspects of one poll share.
    pub enrich_timeout: Duration,
}

impl ContainersConfig {
    /// The settings, with the default bounds.
    #[must_use]
    pub fn new(enabled: bool, include_swarm: bool) -> Self {
        Self {
            enabled,
            include_swarm,
            inspect_cap: CONTAINERS_INSPECT_CAP,
            enrich_timeout: CONTAINERS_INSPECT_TIMEOUT,
        }
    }
}

/// The labels of one container, as a poll returns them: `project`, `stack`, `service`,
/// `container`, `orchestrator`, `display_name`, then the computed `state` (maybe a health
/// overlay) and `exit_code`.
pub type ContainerRow = BTreeMap<String, String>;

/// What an inspect is needed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectNeed {
    None,
    Health,
    Exit,
}

/// The working record of one container during a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    id: String,
    /// The state the list reported.
    state: String,
    labels: ContainerRow,
    need: InspectNeed,
}

impl Row {
    fn keep_base_state(&mut self) {
        self.labels
            .insert(LABEL_STATE.to_owned(), self.state.clone());
    }
}

/// The orchestrator of a container and the names it gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orchestration {
    /// The Compose project, or empty.
    pub project: String,
    /// The Compose service, else the Swarm service without its stack prefix, or empty.
    pub service: String,
    /// The Swarm stack, or empty.
    pub stack: String,
    /// `compose`, `swarm` or `none`.
    pub orchestrator: &'static str,
}

/// The container state family and its settings.
#[derive(Debug)]
pub struct ContainersState {
    config: ContainersConfig,
    family: Arc<SnapshotFamily>,
    logger: Logger,
}

impl ContainersState {
    /// The family, with nothing published, logging to `logger`.
    #[must_use]
    pub fn new(config: ContainersConfig, logger: Logger) -> Self {
        let names = sanitize_label_names(
            &[
                LABEL_PROJECT,
                LABEL_STACK,
                LABEL_SERVICE,
                LABEL_CONTAINER,
                LABEL_ORCHESTRATOR,
                LABEL_DISPLAY_NAME,
                LABEL_STATE,
                LABEL_EXIT_CODE,
            ]
            .map(str::to_owned),
        );
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let desc = constant_desc(
            &format!("{NAMESPACE}_{LABEL_CONTAINER}_{LABEL_STATE}"),
            CONTAINER_STATE_HELP,
            Kind::Gauge,
            &names,
        );
        Self {
            config,
            family: Arc::new(SnapshotFamily::new(desc)),
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

    /// Lists every container (exited ones included) and returns one row per container counted,
    /// enriched by a bounded number of inspects: the health overlay of a running container with
    /// a healthcheck, the exit code of an exited one. Inert, with no rows and no call, when the
    /// collector is disabled.
    ///
    /// # Errors
    ///
    /// `container list: …` when the list fails or outlasts its 5s bound; the caller then keeps
    /// the previous set.
    pub async fn poll(&self, docker: &impl DockerApi) -> Result<Vec<ContainerRow>, CallError> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        let list = with_timeout(CONTAINERS_LIST_TIMEOUT, docker.container_list())
            .await
            .map_err(|error| CallError::new("container list", error))?;

        let mut rows = Vec::with_capacity(list.len());
        for container in list {
            // Swarm task containers are skipped unless asked for.
            let is_swarm =
                !trim_space(label(&container.labels, SWARM_SERVICE_NAME_LABEL)).is_empty();
            if is_swarm && !self.config.include_swarm {
                continue;
            }
            let orchestration = classify_orchestrator(&container.labels);
            let name = first_container_name(&container.names).to_owned();
            let group = if orchestration.project.is_empty() {
                &orchestration.stack
            } else {
                &orchestration.project
            };
            let friendly = display_name_for_container(group, &orchestration.service, &name);
            let labels = ContainerRow::from([
                (LABEL_PROJECT.to_owned(), orchestration.project),
                (LABEL_STACK.to_owned(), orchestration.stack),
                (LABEL_SERVICE.to_owned(), orchestration.service),
                (LABEL_CONTAINER.to_owned(), name),
                (
                    LABEL_ORCHESTRATOR.to_owned(),
                    orchestration.orchestrator.to_owned(),
                ),
                (LABEL_DISPLAY_NAME.to_owned(), friendly),
            ]);
            // The list does not tell whether a running container has a healthcheck: an
            // inspect does.
            let need = match container.state.as_str() {
                CONTAINER_STATE_RUNNING => InspectNeed::Health,
                CONTAINER_STATE_EXITED => InspectNeed::Exit,
                _ => InspectNeed::None,
            };
            rows.push(Row {
                id: container.id,
                state: container.state,
                labels,
                need,
            });
        }

        self.enrich(docker, &mut rows).await;
        Ok(rows.into_iter().map(|row| row.labels).collect())
    }

    /// Publishes the one-hot state series of every row, built whole first and swapped in at
    /// once, so vanished containers are dropped without a scrape seeing a half-built set; a
    /// failed build keeps the previous set. Inert when the collector is disabled.
    pub fn update(&self, rows: &[ContainerRow]) {
        if !self.config.enabled {
            return;
        }
        match build_containers_state(&self.family, rows).build() {
            Ok(families) => self.family.collector().publish(families),
            Err(error) => self.logger.error(
                "build container state snapshot; keeping previous",
                &[Attr::error("err", &error)],
            ),
        }
    }

    /// Inspects the rows that need it, in list order, under one deadline for the whole poll
    /// and at most [`ContainersConfig::inspect_cap`] attempts; a row not reached, or whose
    /// inspect failed, keeps its listed state. Every row ends with a `state` and an `exit_code`.
    /// The caller cancels the rest by dropping the poll, as Go's parent context does.
    async fn enrich(&self, docker: &impl DockerApi, rows: &mut [Row]) {
        let deadline = Instant::now() + self.config.enrich_timeout;
        let mut inspected = 0;
        for row in rows {
            row.labels.insert(LABEL_EXIT_CODE.to_owned(), String::new());
            if row.need == InspectNeed::None || !self.can_inspect(deadline, inspected) {
                row.keep_base_state();
                continue;
            }
            inspected += 1;
            match with_deadline(deadline, docker.container_inspect(&row.id)).await {
                Err(error) => {
                    self.logger.warn(
                        "container inspect failed; using base state",
                        &[
                            Attr::string("container_id", row.id.as_str()),
                            Attr::error("err", &error),
                        ],
                    );
                    row.keep_base_state();
                }
                Ok(inspect) => match row.need {
                    InspectNeed::Health => apply_health_overlay(row, &inspect),
                    InspectNeed::Exit => apply_exit_code(row, &inspect),
                    InspectNeed::None => row.keep_base_state(),
                },
            }
        }
    }

    /// Whether one more inspect fits in this poll: fewer than the cap were attempted and the
    /// deadline has not passed. Logs at debug, for every row skipped, why not.
    fn can_inspect(&self, deadline: Instant, inspected: usize) -> bool {
        if inspected >= self.config.inspect_cap {
            self.logger.debug(
                "containers inspect cap reached, skipping remaining enrichments",
                &[Attr::int(
                    "cap",
                    i64::try_from(self.config.inspect_cap).unwrap_or(i64::MAX),
                )],
            );
            return false;
        }
        if Instant::now() >= deadline {
            self.logger.debug(
                "containers enrichment deadline reached, skipping remaining enrichments",
                &[Attr::duration(
                    "timeout",
                    i64::try_from(self.config.enrich_timeout.as_nanos()).unwrap_or(i64::MAX),
                )],
            );
            return false;
        }
        true
    }
}

/// The one-hot state series of every row, without publishing them: every known state per
/// container, 1 for its own, and `exit_code` always present (empty unless exited) so the label
/// set stays the same.
#[must_use]
pub fn build_containers_state(family: &SnapshotFamily, rows: &[ContainerRow]) -> SnapshotBuilder {
    let mut builder = SnapshotBuilder::new();
    for base in rows {
        let own_state = label(base, LABEL_STATE);
        let exit_code = label(base, LABEL_EXIT_CODE).to_owned();
        for state in KNOWN_CONTAINER_STATES {
            let mut labels = base.clone();
            labels.insert(LABEL_STATE.to_owned(), state.to_owned());
            labels.insert(LABEL_EXIT_CODE.to_owned(), exit_code.clone());
            let labels = sanitize_metric_labels(labels);
            let pairs: Vec<(&String, &String)> = labels.iter().collect();
            let value = if own_state == state { 1.0 } else { 0.0 };
            builder.set(family.desc(), &pairs, value);
        }
    }
    builder
}

/// The value of `key`, empty when missing, as a Go map read.
fn label<'labels>(labels: &'labels BTreeMap<String, String>, key: &str) -> &'labels str {
    labels.get(key).map_or("", String::as_str)
}

/// The orchestrator a container's labels name, and the project, service and stack they give
/// it: Compose when it has a project, Swarm when it has a Swarm service (whose name, without
/// the stack prefix, is the service unless Compose named one), else none.
#[must_use]
pub fn classify_orchestrator(labels: &Labels) -> Orchestration {
    let project = trim_space(label(labels, COMPOSE_PROJECT_LABEL)).to_owned();
    let mut service = trim_space(label(labels, COMPOSE_SERVICE_LABEL)).to_owned();
    let stack = trim_space(label(labels, STACK_NAMESPACE_LABEL)).to_owned();
    let swarm_service = trim_space(label(labels, SWARM_SERVICE_NAME_LABEL));
    let orchestrator = if !project.is_empty() {
        ORCHESTRATOR_COMPOSE
    } else if !swarm_service.is_empty() {
        if service.is_empty() {
            short_service_name(&stack, swarm_service).clone_into(&mut service);
        }
        ORCHESTRATOR_SWARM
    } else {
        ORCHESTRATOR_NONE
    };
    Orchestration {
        project,
        service,
        stack,
        orchestrator,
    }
}

/// The first of `names` without its leading `/`, empty when there is none. Not sanitized: the
/// label value is the name as Docker reports it.
#[must_use]
pub fn first_container_name(names: &[String]) -> &str {
    names
        .first()
        .map_or("", |name| name.strip_prefix('/').unwrap_or(name.as_str()))
}

/// The stack/service rule of `display_name` (the group alone when it equals the service, else
/// `<group> <service>`), falling back to the container name without a group and a service.
#[must_use]
pub fn display_name_for_container(group: &str, service: &str, container_name: &str) -> String {
    if group.is_empty() || service.is_empty() {
        return container_name.to_owned();
    }
    if group == service {
        group.to_owned()
    } else {
        format!("{group} {service}")
    }
}

/// Overlays the health of a container with a healthcheck: `healthy`, `unhealthy`, or
/// `health_starting` for `starting`, read case-insensitively and trimmed; anything else, or no
/// healthcheck, keeps the listed state.
fn apply_health_overlay(row: &mut Row, inspect: &ContainerInspect) {
    let has_healthcheck = inspect
        .config
        .as_ref()
        .is_some_and(|config| config.healthcheck.is_some());
    let health = inspect
        .state
        .as_ref()
        .and_then(|state| state.health.as_ref());
    if let Some(health) = health.filter(|_| has_healthcheck) {
        let overlay = match to_lower(trim_space(&health.status)).as_str() {
            CONTAINER_STATE_HEALTHY => Some(CONTAINER_STATE_HEALTHY),
            CONTAINER_STATE_UNHEALTHY => Some(CONTAINER_STATE_UNHEALTHY),
            "starting" => Some(CONTAINER_STATE_HEALTH_STARTING),
            _ => None,
        };
        if let Some(overlay) = overlay {
            row.labels
                .insert(LABEL_STATE.to_owned(), overlay.to_owned());
            return;
        }
    }
    row.keep_base_state();
}

/// Keeps the listed state and, for an exited container whose inspect has a state, records the
/// exit code.
fn apply_exit_code(row: &mut Row, inspect: &ContainerInspect) {
    row.keep_base_state();
    let Some(state) = &inspect.state else {
        return;
    };
    if !equal_fold(&row.state, CONTAINER_STATE_EXITED) {
        return;
    }
    row.labels
        .insert(LABEL_EXIT_CODE.to_owned(), state.exit_code.to_string());
}

#[cfg(test)]
mod tests;
