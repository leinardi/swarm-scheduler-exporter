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

//! Bring-up's cleanup boundary on testenv-core's `DinD` Swarm: a setup step that fails in the
//! workload image load's place removes everything the bring-up made, or keeps it labelled when
//! the spec asks; a cancelled bring-up still tears down; and a kept run survives the sweep of a
//! later run that removes an unkept one.

use std::collections::HashMap;
use std::time::Duration;

use clock::{Clock as _, SystemClock};
use futures_util::future::{Either, select};
use testenv::config::{ENV_IDS, KEEP_LABEL, LABELS};
use testenv::{Error, NAMES, Spec, WORKLOAD_IMAGE, new_logger, up_with};
use testenv_core::bollard::Docker;
use testenv_core::bollard::models::ContainerCreateBody;
use testenv_core::bollard::query_parameters::{
    CreateContainerOptions, ListContainersOptions, ListNetworksOptions,
};
use testenv_core::context::Context;
use testenv_core::dind::{self, KEEP_VALUE, poll_until};
use testenv_core::host::process_alive;
use testenv_core::sweep_rules::{SweepRules, current_owner};

use crate::swarm::{op, teardown_ctx};
use crate::testing::{TestHandle, TestResult, cluster};

/// How long a removal the bring-up hands to a task may take to show.
const TEARDOWN_WAIT: Duration = Duration::from_mins(2);

/// A manager-only cluster of a fresh run, which fails or keeps as `keep_on_failure` says.
fn spec(keep_on_failure: bool) -> Result<Spec, String> {
    Ok(Spec {
        env_id: ENV_IDS.generate().map_err(|error| error.to_string())?,
        binary: String::new(),
        dind_image: String::new(),
        workers: 0,
        keep_on_failure,
        logger: new_logger(),
    })
}

/// A setup step that fails where the workload image load runs.
async fn failing_load(_: &Context, _: &dind::Cluster) -> Result<(), Error> {
    Err(Error::new("injected workload image load failure"))
}

/// The labels of every container and the number of networks the run `env_id` has on the host.
async fn left(
    ctx: &Context,
    host: &Docker,
    env_id: &str,
) -> Result<(Vec<HashMap<String, String>>, usize), String> {
    let filters = HashMap::from([("label".to_owned(), vec![ENV_IDS.label_filter(env_id)])]);
    let containers = op(
        ctx,
        host.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters.clone()),
            ..ListContainersOptions::default()
        })),
    )
    .await
    .map_err(|error| error.to_string())?;
    let networks = op(
        ctx,
        host.list_networks(Some(ListNetworksOptions {
            filters: Some(filters),
        })),
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok((
        containers
            .into_iter()
            .map(|container| container.labels.unwrap_or_default())
            .collect(),
        networks.len(),
    ))
}

/// Waits until the run `env_id` has nothing left on the host.
async fn wait_gone(ctx: &Context, host: &Docker, env_id: &str) -> Result<(), String> {
    poll_until(
        ctx,
        TEARDOWN_WAIT,
        "the run's resources removed",
        async || match left(ctx, host, env_id).await? {
            (containers, 0) if containers.is_empty() => Ok(()),
            (containers, networks) => Err(format!(
                "{} containers and {networks} networks left",
                containers.len()
            )),
        },
    )
    .await
}

/// A failed image load with keep off removes the whole bring-up before `up` returns.
pub async fn failed_load_removes(test: TestHandle) -> TestResult {
    let (ctx, host) = (test.ctx(), cluster().host_client());
    let check = async {
        let spec = spec(false)?;
        let env_id = spec.env_id.clone();
        let Err(error) = up_with(ctx, spec, failing_load).await else {
            return Err("up succeeded with a failing setup step".to_owned());
        };
        if !error
            .to_string()
            .contains("injected workload image load failure")
        {
            return Err(format!("up failed with another error: {error}"));
        }
        match left(ctx, host, &env_id).await? {
            (containers, 0) if containers.is_empty() => Ok(()),
            (containers, networks) => Err(format!(
                "a failed bring-up left {} containers and {networks} networks",
                containers.len()
            )),
        }
    };
    check.await.map_err(|message| test.fatal(message))
}

/// A failed image load with keep on leaves the cluster, every container labelled kept; the sweep
/// of another run whose rules would remove any stale environment leaves it alone, while it
/// removes an unkept environment the same rules reach.
pub async fn failed_load_keeps(test: TestHandle) -> TestResult {
    let (ctx, host) = (test.ctx(), cluster().host_client());
    let spec = match spec(true) {
        Ok(spec) => spec,
        Err(message) => return Err(test.fatal(message)),
    };
    let env_id = spec.env_id.clone();
    let control = match ENV_IDS.generate() {
        Ok(control) => control,
        Err(error) => return Err(test.fatal(error)),
    };
    let check = async {
        if up_with(ctx, spec, failing_load).await.is_ok() {
            return Err("up succeeded with a failing setup step".to_owned());
        }
        let (containers, networks) = left(ctx, host, &env_id).await?;
        if containers.is_empty() || networks == 0 {
            return Err(format!(
                "a kept bring-up left {} containers and {networks} networks",
                containers.len()
            ));
        }
        if let Some(unkept) = containers
            .iter()
            .find(|labels| labels.get(KEEP_LABEL).map(String::as_str) != Some(KEEP_VALUE))
        {
            return Err(format!(
                "a kept container lacks {KEEP_LABEL}={KEEP_VALUE}: {unkept:?}"
            ));
        }

        // The control: an environment of a dead owner, not kept, which the same sweep removes.
        let owner = current_owner().map_err(|error| error.to_string())?;
        op(
            ctx,
            host.create_container(
                Some(CreateContainerOptions {
                    name: Some(ENV_IDS.resource_name(&control, "control")),
                    ..CreateContainerOptions::default()
                }),
                ContainerCreateBody {
                    image: Some(WORKLOAD_IMAGE.to_owned()),
                    labels: Some(LABELS.run_labels(&control, &owner, SystemClock.now())),
                    ..ContainerCreateBody::default()
                },
            ),
        )
        .await
        .map_err(|error| format!("create the control container: {error}"))?;

        // A later run's sweep, as this process's environments would meet it once this process is
        // gone: everything is stale and this process's PID is dead. The suite's own cluster is
        // that run's current environment, which no sweep touches; every other owner keeps its
        // real liveness, so a concurrent run's environment is not swept here.
        let own_pid = owner.pid;
        let rules = SweepRules {
            now: SystemClock.now(),
            stale_after: Duration::ZERO,
            host: owner.host.clone(),
            pid_namespace: owner.pid_namespace.clone(),
            self_id: cluster().spec.env_id.clone(),
            alive: Box::new(move |pid| pid != own_pid && process_alive(pid)),
        };
        NAMES
            .host_sweep()
            .sweep_keeping(ctx, host, &rules, KEEP_LABEL, &new_logger())
            .await;

        let (kept, _) = left(ctx, host, &env_id).await?;
        if kept.len() != containers.len() {
            return Err(format!(
                "the sweep removed a kept run: {} of {} containers left",
                kept.len(),
                containers.len()
            ));
        }
        let (unkept, _) = left(ctx, host, &control).await?;
        if !unkept.is_empty() {
            return Err("the sweep left the unkept control environment".to_owned());
        }
        Ok(())
    };
    let result = Box::pin(check).await;
    // On a context of its own, so the removal runs even when the test's context has ended; the
    // control goes too, in case the sweep left it.
    let teardown = teardown_ctx();
    let sweep = NAMES.host_sweep();
    let kept = sweep.cleanup(&teardown, host, &env_id).await;
    let unkept = sweep.cleanup(&teardown, host, &control).await;
    result.map_err(|message| test.fatal(message))?;
    kept.map_err(|error| test.fatal(format!("remove the kept run: {error}")))?;
    unkept.map_err(|error| test.fatal(format!("remove the control: {error}")))
}

/// A bring-up dropped while its image load runs still removes what it made.
pub async fn cancelled_load_tears_down(test: TestHandle) -> TestResult {
    let (ctx, host) = (test.ctx(), cluster().host_client());
    let check = async {
        let spec = spec(false)?;
        let env_id = spec.env_id.clone();
        let (started, loading) = tokio::sync::oneshot::channel();
        let up = Box::pin(up_with(
            ctx,
            spec,
            async move |_: &Context, _: &dind::Cluster| {
                let _ = started.send(());
                std::future::pending::<Result<(), Error>>().await
            },
        ));
        match select(up, loading).await {
            Either::Left((result, _)) => {
                return Err(format!("up returned before its setup step ran: {result:?}"));
            }
            Either::Right((_, up)) => drop(up),
        }
        wait_gone(ctx, host, &env_id).await
    };
    check.await.map_err(|message| test.fatal(message))
}
