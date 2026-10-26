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

//! Rolling updates: one that completes, and a start-first one that fails and pauses.

use std::time::Duration;

use bollard::models::{
    HealthConfig, ServiceSpecUpdateConfig, ServiceSpecUpdateConfigFailureActionEnum,
    ServiceSpecUpdateConfigOrderEnum, ServiceUpdateStatusStateEnum, TaskState,
};
use testenv::metrics_text::Labels;

use crate::launcher::Launch;
use crate::swarm::{
    SECOND_NANOS, ServiceKey, ServiceOpts, check_update_state, deploy_service, describe_tasks,
    running_tasks, task_placement, update_service,
};
use crate::testing::{OrFatal as _, TestHandle, TestResult};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_RUNNING, METRIC_UPDATE_COMPLETE,
    METRIC_UPDATE_STARTED, METRIC_UPDATE_STATE, eventually, metrics_match, want, want_positive,
    want_service,
};

/// Go: `TestUpdate_Completed`. Rolls a new env out to a replicated service: the update is
/// reported completed, with both timestamps set, and the service back at its desired count.
pub async fn update_completed(test: TestHandle) -> TestResult {
    const STACK: &str = "it-update";
    let web = ServiceKey::new(STACK, "web");
    let service_id = deploy_service(
        &test,
        STACK,
        web.service,
        &ServiceOpts {
            replicas: 2,
            update_config: Some(ServiceSpecUpdateConfig {
                parallelism: Some(1),
                monitor: Some(SECOND_NANOS),
                order: Some(ServiceSpecUpdateConfigOrderEnum::STOP_FIRST),
                ..ServiceSpecUpdateConfig::default()
            }),
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&web), &[])
        .start(&test)
        .await?;

    let settled = [
        want_service(METRIC_RUNNING, &web, 2.0),
        want_service(METRIC_AT_DESIRED, &web, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &settled).await
    })
    .await?;

    Box::pin(update_service(&test, &service_id, |spec| {
        if let Some(container) = spec
            .task_template
            .as_mut()
            .and_then(|template| template.container_spec.as_mut())
        {
            container
                .env
                .get_or_insert_with(Vec::new)
                .push("IT_ROLLOUT=2".to_owned());
        }
    }))
    .await?;

    // Before any update the exporter already reports "completed", with zero timestamps;
    // non-zero timestamps are what tie the state to this update.
    let completed = [
        want(
            METRIC_UPDATE_STATE,
            update_state_labels(&web, "completed"),
            1.0,
        ),
        want(
            METRIC_UPDATE_STATE,
            update_state_labels(&web, "updating"),
            0.0,
        ),
        want_positive(METRIC_UPDATE_STARTED, web.labels()),
        want_positive(METRIC_UPDATE_COMPLETE, web.labels()),
        want_service(METRIC_RUNNING, &web, 2.0),
        want_service(METRIC_AT_DESIRED, &web, 1.0),
    ];
    eventually(&test, Duration::from_secs(90), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &completed).await
    })
    .await?;

    check_update_state(
        test.ctx(),
        &service_id,
        ServiceUpdateStatusStateEnum::COMPLETED,
    )
    .await
    .or_fatal(&test)
}

/// Go: `TestUpdate_StartFirstFailure`. Starts a start-first update whose new tasks exit 1, with
/// failure action pause. The update pauses, the old task keeps serving, and newer tasks that
/// Swarm has given up on sit in the same slot, next to a fresh retry that is not running. The
/// exporter must count the old, running task and so report the service at its desired count,
/// not below it.
///
/// The service has a healthcheck so the new tasks fail before they ever count as running: a task
/// with a healthcheck stays "starting" until its first check passes, and the new command exits
/// before that. Without it the new task is briefly running, which is enough for start-first to
/// stop the old task, and the service really does drop to zero.
pub async fn update_start_first_failure(test: TestHandle) -> TestResult {
    const STACK: &str = "it-update-fail";
    let web = ServiceKey::new(STACK, "web");
    let service_id = deploy_service(
        &test,
        STACK,
        web.service,
        &ServiceOpts {
            replicas: 1,
            healthcheck: Some(passing_healthcheck()),
            update_config: Some(ServiceSpecUpdateConfig {
                parallelism: Some(1),
                monitor: Some(2 * SECOND_NANOS),
                order: Some(ServiceSpecUpdateConfigOrderEnum::START_FIRST),
                failure_action: Some(ServiceSpecUpdateConfigFailureActionEnum::PAUSE),
                ..ServiceSpecUpdateConfig::default()
            }),
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&web), &[])
        .start(&test)
        .await?;

    let settled = [
        want_service(METRIC_RUNNING, &web, 1.0),
        want_service(METRIC_AT_DESIRED, &web, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &settled).await
    })
    .await?;

    let old_tasks = running_tasks(&task_placement(&test, &service_id).await?);
    let [old_task] = old_tasks.as_slice() else {
        return Err(test.fatal(format!(
            "want one running task before the update, have:\n{}",
            describe_tasks(&old_tasks)
        )));
    };

    Box::pin(update_service(&test, &service_id, |spec| {
        if let Some(container) = spec
            .task_template
            .as_mut()
            .and_then(|template| template.container_spec.as_mut())
        {
            container.command = Some(vec!["sh".to_owned(), "-c".to_owned(), "exit 1".to_owned()]);
            // bollard reads the spec's healthcheck under the key HealthCheck, the Engine writes it
            // as Healthcheck: the inspected spec comes back without it, and submitting that as is
            // would drop it, which is the very case this test must avoid.
            container.health_check = Some(passing_healthcheck());
        }
    }))
    .await?;

    eventually(&test, Duration::from_secs(90), async |ctx| {
        check_update_state(ctx, &service_id, ServiceUpdateStatusStateEnum::PAUSED).await
    })
    .await?;

    let placement = task_placement(&test, &service_id).await?;
    test.log(format!(
        "tasks after the paused update:\n{}",
        describe_tasks(&placement)
    ));

    // The slot must hold both the old task, still serving, and a newer one Swarm retired: that
    // pairing is what makes the choice of task per slot matter.
    let mut serving = false;
    let mut retired_newer = false;
    for task in &placement {
        if task.id == old_task.id {
            serving = task.state == Some(TaskState::RUNNING)
                && task.desired_state == Some(TaskState::RUNNING);
        } else if task.slot == old_task.slot
            && task.created_at > old_task.created_at
            && task.desired_state != Some(TaskState::RUNNING)
        {
            retired_newer = true;
        }
    }
    if !serving || !retired_newer {
        return Err(test.fatal(format!(
            "want the old task serving ({serving}) and a retired newer task in slot {} \
             ({retired_newer}), have:\n{}",
            old_task.slot,
            describe_tasks(&placement)
        )));
    }

    let paused = [
        want(
            METRIC_UPDATE_STATE,
            update_state_labels(&web, "paused"),
            1.0,
        ),
        want_service(METRIC_DESIRED, &web, 1.0),
        want_service(METRIC_RUNNING, &web, 1.0),
        want_service(METRIC_AT_DESIRED, &web, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &paused).await
    })
    .await
}

/// A healthcheck that passes, so a task only counts as running once it has run for a second.
fn passing_healthcheck() -> HealthConfig {
    HealthConfig {
        test: Some(vec!["CMD".to_owned(), "true".to_owned()]),
        interval: Some(SECOND_NANOS),
        timeout: Some(SECOND_NANOS),
        retries: Some(1),
        ..HealthConfig::default()
    }
}

fn update_state_labels(service: &ServiceKey, state: &str) -> Labels {
    let mut labels = service.labels();
    labels.insert("state".to_owned(), state.to_owned());
    labels
}
