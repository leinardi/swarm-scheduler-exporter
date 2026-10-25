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

//! Services and tasks on the test Swarm, through the manager: deployed the way `docker stack
//! deploy` would, running the workload image by ID, and removed (their tasks reaped) when the
//! test ends.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use bollard::errors::Error as DockerError;
use bollard::models::{
    ServiceSpec, ServiceSpecMode, ServiceSpecModeReplicated, TaskSpec, TaskSpecContainerSpec,
    TaskSpecPlacement, TaskSpecRestartPolicy, TaskSpecRestartPolicyConditionEnum, TaskState,
};
use bollard::query_parameters::{InspectServiceOptions, ListTasksOptions, UpdateServiceOptions};
use jiff::Timestamp;
use testenv::context::{Context, Ended};
use testenv::metrics_text::{Labels, labels};

use crate::testing::{Fatal, OrFatal as _, TestHandle, cluster, swarm};
use crate::wait::{LABEL_SERVICE, LABEL_STACK, poll_until};

const STACK_NAMESPACE_LABEL: &str = "com.docker.stack.namespace";

/// Bounds a single Docker call made by a test.
pub const DOCKER_OP_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounds every teardown step, which runs on a context of its own so it still runs after the
/// test's or the suite's has ended.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_mins(2);
/// Bounds the wait for a removed service's tasks to disappear.
const TASK_REAP_TIMEOUT: Duration = Duration::from_secs(60);

/// The default workload: it idles, and exits promptly on SIGTERM. A bare sleep would run as PID 1
/// with no SIGTERM handler and hold every stop (scale down, update, removal) for the whole grace
/// period.
const STOP_FAST: &[&str] = &[
    "sh",
    "-c",
    "trap 'exit 0' TERM; while true; do sleep 1; done",
];

/// Go's `teardownCtx()`: a context for cleanup work that derives from neither the test's nor
/// the suite's, since those are done when cleanup runs.
pub fn teardown_ctx() -> Context {
    Context::background().with_timeout(TEARDOWN_TIMEOUT)
}

/// A Docker call that failed, or whose context ended first.
#[derive(Debug)]
pub enum CallError {
    Docker(DockerError),
    Ended(Ended),
}

impl CallError {
    /// The daemon answered 404.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::Docker(DockerError::DockerResponseServerError {
                status_code: 404,
                ..
            })
        )
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Docker(error) => error.fmt(formatter),
            Self::Ended(ended) => ended.fmt(formatter),
        }
    }
}

/// One Docker call made by a test, under [`DOCKER_OP_TIMEOUT`] and `ctx`.
pub async fn op<V>(
    ctx: &Context,
    call: impl Future<Output = Result<V, DockerError>>,
) -> Result<V, CallError> {
    ctx.with_timeout(DOCKER_OP_TIMEOUT)
        .run(call)
        .await
        .map_err(CallError::Ended)?
        .map_err(CallError::Docker)
}

/// A service as the exporter labels it: the stack, and the service name without the stack
/// prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceKey {
    pub stack: &'static str,
    pub service: &'static str,
}

impl ServiceKey {
    pub const fn new(stack: &'static str, service: &'static str) -> Self {
        Self { stack, service }
    }

    /// The service's stack and service labels.
    pub fn labels(&self) -> Labels {
        labels(&[(LABEL_STACK, self.stack), (LABEL_SERVICE, self.service)])
    }
}

/// The knobs of [`deploy_service`]. The default is a replicated service with one replica
/// running [`STOP_FAST`].
#[derive(Debug, Clone, Default)]
pub struct ServiceOpts {
    pub global: bool,
    /// Replicated only; 0 means 1 unless `zero_replicas` is set.
    pub replicas: u64,
    pub zero_replicas: bool,
    /// Defaults to [`STOP_FAST`].
    pub command: Option<Vec<String>>,
    pub constraints: Vec<String>,
    /// Swarm's default (any) when `None`.
    pub restart_condition: Option<TaskSpecRestartPolicyConditionEnum>,
    /// Extra service labels, for the exporter's `--label`.
    pub labels: HashMap<String, String>,
}

/// Creates `<stack>_<name>` the way `docker stack deploy` would (the stack namespace label on
/// both the service and its containers, which is the only place the exporter reads stack and
/// service from), running the workload image by ID. Returns the service ID; the service is
/// removed, and its tasks waited for, when the test ends.
pub async fn deploy_service(
    test: &TestHandle,
    stack: &str,
    name: &str,
    opts: &ServiceOpts,
) -> Result<String, Fatal> {
    let spec = service_spec(stack, name, opts);
    let full_name = spec.name.clone().unwrap_or_default();
    // An image referenced by ID: nothing, neither the manager nor a node's executor, ever
    // contacts a registry.
    let created = op(test.ctx(), swarm().create_service(spec, None))
        .await
        .map_err(|error| test.fatal(format!("create service {full_name}: {error}")))?;
    let id = created.id.unwrap_or_default();

    let cleanup_t = test.clone();
    let cleanup_id = id.clone();
    test.cleanup(move || async move {
        if let Err(error) = remove_service_and_wait(&teardown_ctx(), &cleanup_id).await {
            cleanup_t.error(format!("cleanup: {error}"));
        }
    });
    Ok(id)
}

#[expect(
    clippy::zero_sized_map_values,
    reason = "bollard types Swarm's global mode as a HashMap<(), ()>: the empty object the API takes"
)]
fn service_spec(stack: &str, name: &str, opts: &ServiceOpts) -> ServiceSpec {
    let mut service_labels = HashMap::from([(STACK_NAMESPACE_LABEL.to_owned(), stack.to_owned())]);
    service_labels.extend(opts.labels.clone());

    let command = opts
        .command
        .clone()
        .unwrap_or_else(|| STOP_FAST.iter().map(|&part| part.to_owned()).collect());

    let mode = if opts.global {
        ServiceSpecMode {
            global: Some(HashMap::default()),
            ..ServiceSpecMode::default()
        }
    } else {
        let replicas = if opts.replicas == 0 && !opts.zero_replicas {
            1
        } else {
            opts.replicas
        };
        ServiceSpecMode {
            replicated: Some(ServiceSpecModeReplicated {
                replicas: Some(i64::try_from(replicas).unwrap_or(i64::MAX)),
            }),
            ..ServiceSpecMode::default()
        }
    };

    ServiceSpec {
        name: Some(format!("{stack}_{name}")),
        labels: Some(service_labels),
        task_template: Some(TaskSpec {
            container_spec: Some(TaskSpecContainerSpec {
                image: Some(cluster().workload_image_id.clone()),
                command: Some(command),
                labels: Some(HashMap::from([(
                    STACK_NAMESPACE_LABEL.to_owned(),
                    stack.to_owned(),
                )])),
                // One second, in nanoseconds.
                stop_grace_period: Some(1_000_000_000),
                ..TaskSpecContainerSpec::default()
            }),
            placement: Some(TaskSpecPlacement {
                constraints: Some(opts.constraints.clone()),
                ..TaskSpecPlacement::default()
            }),
            restart_policy: opts
                .restart_condition
                .map(|condition| TaskSpecRestartPolicy {
                    condition: Some(condition),
                    ..TaskSpecRestartPolicy::default()
                }),
            ..TaskSpec::default()
        }),
        mode: Some(mode),
        ..ServiceSpec::default()
    }
}

/// Applies `mutate` to the service's current spec and submits it.
pub async fn update_service(
    test: &TestHandle,
    service_id: &str,
    mutate: impl FnOnce(&mut ServiceSpec),
) -> Result<(), Fatal> {
    let ctx = test.ctx();
    let inspected = op(
        ctx,
        swarm().inspect_service(service_id, None::<InspectServiceOptions>),
    )
    .await
    .map_err(|error| test.fatal(format!("inspect service {service_id}: {error}")))?;
    let mut spec = inspected.spec.unwrap_or_default();
    mutate(&mut spec);
    let version = inspected
        .version
        .and_then(|version| version.index)
        .unwrap_or_default();
    let options = UpdateServiceOptions {
        version: i32::try_from(version).or_fatal(test)?,
        ..UpdateServiceOptions::default()
    };
    op(ctx, swarm().update_service(service_id, spec, options, None))
        .await
        .map_err(|error| test.fatal(format!("update service {service_id}: {error}")))?;
    Ok(())
}

/// Removes the service now, inside the test, and waits for its tasks to be reaped. The cleanup
/// [`deploy_service`] registered then finds nothing left to do.
pub async fn remove_service(test: &TestHandle, service_id: &str) -> Result<(), Fatal> {
    remove_service_and_wait(test.ctx(), service_id)
        .await
        .or_fatal(test)
}

async fn remove_service_and_wait(ctx: &Context, service_id: &str) -> Result<(), String> {
    match op(ctx, swarm().delete_service(service_id)).await {
        Err(error) if !error.is_not_found() => {
            return Err(format!("remove service {service_id}: {error}"));
        }
        Ok(()) | Err(_) => {}
    }
    poll_until(ctx, TASK_REAP_TIMEOUT, async || {
        let left = count_orphan_tasks(ctx, service_id).await?;
        if left > 0 {
            return Err(format!(
                "removed service {service_id} still has {left} tasks: not yet"
            ));
        }
        Ok(())
    })
    .await
}

/// The tasks a service still leaves. It lists every task: once the service is gone, the manager
/// refuses a task filter that names it.
async fn count_orphan_tasks(ctx: &Context, service_id: &str) -> Result<usize, String> {
    let listed = op(ctx, swarm().list_tasks(None::<ListTasksOptions>))
        .await
        .map_err(|error| format!("list tasks: {error}"))?;
    Ok(listed
        .iter()
        .filter(|task| task.service_id.as_deref() == Some(service_id))
        .count())
}

/// Where one task sits and what state it is in.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: String,
    pub slot: i64,
    pub node_id: String,
    pub state: Option<TaskState>,
    pub desired_state: Option<TaskState>,
    pub created_at: Option<Timestamp>,
}

impl std::fmt::Display for TaskInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state =
            |state: Option<TaskState>| state.map_or_else(String::new, |state| state.to_string());
        write!(
            formatter,
            "slot={} node={} state={} desired={} task={}",
            self.slot,
            self.node_id,
            state(self.state),
            state(self.desired_state),
            self.id
        )
    }
}

/// Every task of the service, retired ones included, oldest first.
pub async fn list_tasks(ctx: &Context, service_id: &str) -> Result<Vec<TaskInfo>, String> {
    let options = ListTasksOptions {
        filters: Some(HashMap::from([(
            "service".to_owned(),
            vec![service_id.to_owned()],
        )])),
    };
    let listed = op(ctx, swarm().list_tasks(Some(options)))
        .await
        .map_err(|error| format!("list tasks of {service_id}: {error}"))?;
    let mut tasks: Vec<TaskInfo> = listed
        .into_iter()
        .map(|task| TaskInfo {
            id: task.id.unwrap_or_default(),
            slot: task.slot.unwrap_or_default(),
            node_id: task.node_id.unwrap_or_default(),
            state: task.status.and_then(|status| status.state),
            desired_state: task.desired_state,
            created_at: task.created_at.and_then(|created| created.parse().ok()),
        })
        .collect();
    tasks.sort_by_key(|task| task.created_at);
    Ok(tasks)
}

/// The tasks that are running and meant to be.
pub fn running_tasks(tasks: &[TaskInfo]) -> Vec<TaskInfo> {
    tasks
        .iter()
        .filter(|task| {
            task.state == Some(TaskState::RUNNING) && task.desired_state == Some(TaskState::RUNNING)
        })
        .cloned()
        .collect()
}

/// One task per line, for failure output.
pub fn describe_tasks(tasks: &[TaskInfo]) -> String {
    tasks
        .iter()
        .map(|task| format!("  {task}"))
        .collect::<Vec<_>>()
        .join("\n")
}
