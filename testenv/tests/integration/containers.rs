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

//! The opt-in container metrics against the manager's daemon.

use std::time::Duration;

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
};
use testenv::metrics_text::labels;

use crate::launcher::Launch;
use crate::swarm::{
    STOP_FAST, ServiceKey, ServiceOpts, deploy_service, describe_tasks, list_tasks, op,
    running_tasks, teardown_ctx,
};
use crate::testing::{Fatal, TestHandle, TestResult, cluster, swarm};
use crate::wait::{
    LABEL_SERVICE, LABEL_STACK, METRIC_CONTAINER_STATE, consistently, eventually, metrics_match,
    want, want_absent,
};

const STACK: &str = "it-containers";
const PLAIN_NAME: &str = "it-plain";
/// How many consecutive exporter polls must leave the task container out.
const CONTAINER_ABSENCE_POLLS: u32 = 3;

/// Go: `TestContainers_OptIn`. Checks the opt-in container metrics against the manager's
/// daemon: a plain container is reported with `--containers`, while a Swarm task container on
/// the same daemon is reported only when `--containers-include-swarm` is set as well.
pub async fn containers_opt_in(test: TestHandle) -> TestResult {
    let pinned = ServiceKey::new(STACK, "pinned");
    let service_id = deploy_service(
        &test,
        STACK,
        pinned.service,
        &ServiceOpts {
            constraints: vec!["node.role==manager".to_owned()],
            ..ServiceOpts::default()
        },
    )
    .await?;

    // The exporter only sees the daemon it talks to, the manager's, so the task must run there
    // before its absence or presence means anything.
    let manager_id = &cluster().manager.swarm_node_id;
    eventually(&test, Duration::from_secs(60), async |ctx| {
        let tasks = list_tasks(ctx, &service_id).await?;
        match running_tasks(&tasks).as_slice() {
            [only] if &only.node_id == manager_id => Ok(()),
            _ => Err(format!(
                "want one running task on the manager {manager_id}, have:\n{}: not yet",
                describe_tasks(&tasks)
            )),
        }
    })
    .await?;

    run_plain_container(&test, PLAIN_NAME).await?;

    let plain = labels(&[
        ("container", PLAIN_NAME),
        ("orchestrator", "none"),
        ("state", "running"),
    ]);
    let task = labels(&[
        (LABEL_STACK, STACK),
        (LABEL_SERVICE, pinned.service),
        ("orchestrator", "swarm"),
    ]);
    let task_running = labels(&[
        (LABEL_STACK, STACK),
        (LABEL_SERVICE, pinned.service),
        ("orchestrator", "swarm"),
        ("state", "running"),
    ]);

    // Both containers exist before the exporter starts, so every poll that reports the plain
    // one also saw the task container, and must have skipped it.
    let default_url = Launch::seeded(std::slice::from_ref(&pinned), &["--containers"])
        .start(&test)
        .await?;

    let plain_reported = [want(METRIC_CONTAINER_STATE, plain.clone(), 1.0)];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &default_url, STACK, &plain_reported).await
    })
    .await?;

    // Absence has to hold, not just be seen once: the exporter resets the container gauge and
    // re-adds its series one by one, so a scrape landing mid-rewrite can miss a container that
    // the exporter does report.
    let task_left_out = [want_absent(METRIC_CONTAINER_STATE, task)];
    consistently(&test, &default_url, CONTAINER_ABSENCE_POLLS, async |ctx| {
        metrics_match(ctx, &default_url, STACK, &task_left_out).await
    })
    .await?;

    let include_url = Launch::seeded(
        std::slice::from_ref(&pinned),
        &["--containers", "--containers-include-swarm"],
    )
    .start(&test)
    .await?;

    let both_reported = [
        want(METRIC_CONTAINER_STATE, plain, 1.0),
        want(METRIC_CONTAINER_STATE, task_running, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &include_url, STACK, &both_reported).await
    })
    .await
}

/// Starts a container outside Swarm on the manager's daemon, removed when the test ends.
async fn run_plain_container(test: &TestHandle, name: &str) -> Result<(), Fatal> {
    let options = CreateContainerOptions {
        name: Some(name.to_owned()),
        ..CreateContainerOptions::default()
    };
    let body = ContainerCreateBody {
        image: Some(cluster().workload_image_id.clone()),
        cmd: Some(STOP_FAST.iter().map(|&part| part.to_owned()).collect()),
        ..ContainerCreateBody::default()
    };
    let created = op(test.ctx(), swarm().create_container(Some(options), body))
        .await
        .map_err(|error| test.fatal(format!("create container {name}: {error}")))?;

    let cleanup_test = test.clone();
    let cleanup_id = created.id.clone();
    let cleanup_name = name.to_owned();
    test.cleanup(move || async move {
        let remove = swarm().remove_container(
            &cleanup_id,
            Some(RemoveContainerOptions {
                force: true,
                ..RemoveContainerOptions::default()
            }),
        );
        match op(&teardown_ctx(), remove).await {
            Err(error) if !error.is_not_found() => {
                cleanup_test.error(format!("cleanup: remove container {cleanup_name}: {error}"));
            }
            Ok(()) | Err(_) => {}
        }
    });

    op(
        test.ctx(),
        swarm().start_container(&created.id, None::<StartContainerOptions>),
    )
    .await
    .map_err(|error| test.fatal(format!("start container {name}: {error}")))
}
