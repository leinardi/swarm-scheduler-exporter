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

//! The opt-in container metrics against the manager's daemon, and against a worker's, which is
//! not a Swarm manager.

use std::time::Duration;

use testenv::metrics_text::labels;
use testenv_core::bollard::Docker;
use testenv_core::bollard::models::ContainerCreateBody;
use testenv_core::bollard::query_parameters::{
    CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
};

use crate::http::get;
use crate::launcher::Launch;
use crate::swarm::{
    STOP_FAST, ServiceKey, ServiceOpts, deploy_service, describe_tasks, list_tasks, op,
    running_tasks, teardown_ctx,
};
use crate::testing::{Fatal, TestHandle, TestResult, cluster, swarm};
use crate::wait::{
    LABEL_SERVICE, LABEL_STACK, METRIC_CONTAINER_STATE, METRIC_DESIRED, METRIC_NODES_BY_STATE,
    METRIC_SWARM_MANAGER, consistently, eventually, metrics_match, no_labels, want, want_absent,
};

const STACK: &str = "it-containers";
const PLAIN_NAME: &str = "it-plain";
const WORKER_PLAIN_NAME: &str = "it-plain-worker";
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

    run_plain_container(&test, swarm(), PLAIN_NAME).await?;

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

    let plain_reported = [
        want(METRIC_CONTAINER_STATE, plain.clone(), 1.0),
        want(METRIC_SWARM_MANAGER, no_labels(), 1.0),
    ];
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

/// Issue #140: `--containers` against a worker's daemon, which refuses every manager-only call
/// with 503 "This node is not a swarm manager". The plain container is reported, no Swarm series
/// is, `swarm_exporter_swarm_manager` is 0 and the exporter is healthy on its container polls;
/// without `--containers` it is unhealthy, as not a swarm manager.
pub async fn containers_non_manager(test: TestHandle) -> TestResult {
    let Some(worker) = cluster().workers().first().copied() else {
        return Err(test.skip("the non-manager scenario needs a worker, the cluster has none"));
    };
    let docker = worker
        .client()
        .map_err(|error| test.fatal(format!("worker client: {error}")))?;
    run_plain_container(&test, &docker, WORKER_PLAIN_NAME).await?;

    let url = Launch::serving(&worker.docker_host, &["--containers"])
        .start(&test)
        .await?;
    let reported = [
        want(
            METRIC_CONTAINER_STATE,
            labels(&[
                ("container", WORKER_PLAIN_NAME),
                ("orchestrator", "none"),
                ("state", "running"),
            ]),
            1.0,
        ),
        want(METRIC_SWARM_MANAGER, no_labels(), 0.0),
        want_absent(METRIC_NODES_BY_STATE, no_labels()),
        want_absent(METRIC_DESIRED, no_labels()),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &url, STACK, &reported).await?;
        let (status, body) = get(ctx, &format!("{url}/healthz")).await?;
        if status != 200 {
            return Err(format!(
                "/healthz = {status} {:?}, want 200: not yet",
                String::from_utf8_lossy(&body)
            ));
        }
        Ok(())
    })
    .await?;

    let bare_url = Launch::serving(&worker.docker_host, &[])
        .start(&test)
        .await?;
    eventually(&test, Duration::from_secs(60), async |ctx| {
        let (status, body) = get(ctx, &format!("{bare_url}/healthz")).await?;
        let body = String::from_utf8_lossy(&body);
        if status == 503 && body.contains("not a swarm manager") {
            return Ok(());
        }
        Err(format!(
            "/healthz without --containers = {status} {body:?}, want 503 \"not a swarm manager\": \
             not yet"
        ))
    })
    .await
}

/// Starts a container outside Swarm on the daemon of `docker`, removed when the test ends.
async fn run_plain_container(test: &TestHandle, docker: &Docker, name: &str) -> Result<(), Fatal> {
    let options = CreateContainerOptions {
        name: Some(name.to_owned()),
        ..CreateContainerOptions::default()
    };
    let body = ContainerCreateBody {
        image: Some(cluster().workload_image_id.clone()),
        cmd: Some(STOP_FAST.iter().map(|&part| part.to_owned()).collect()),
        ..ContainerCreateBody::default()
    };
    let created = op(test.ctx(), docker.create_container(Some(options), body))
        .await
        .map_err(|error| test.fatal(format!("create container {name}: {error}")))?;

    let cleanup_test = test.clone();
    let cleanup_docker = docker.clone();
    let cleanup_id = created.id.clone();
    let cleanup_name = name.to_owned();
    test.cleanup(move || async move {
        let remove = cleanup_docker.remove_container(
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
        docker.start_container(&created.id, None::<StartContainerOptions>),
    )
    .await
    .map_err(|error| test.fatal(format!("start container {name}: {error}")))
}
