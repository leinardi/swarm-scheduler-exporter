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

//! The harness and the exporter's basics, end to end.

use std::collections::HashMap;
use std::time::Duration;

use bollard::models::EventMessageTypeEnum;
use bollard::query_parameters::{EventsOptions, InspectServiceOptions};
use futures_util::StreamExt as _;
use swarm_scheduler_exporter::clock::{Clock as _, SystemClock};
use testenv::Node;
use testenv::context::Context;
use testenv::metrics_text::labels;

use crate::http::{get, scrape_metrics};
use crate::launcher::Launch;
use crate::swarm::{
    DOCKER_OP_TIMEOUT, Mode, ServiceKey, ServiceOpts, TaskInfo, deploy_service, describe_tasks,
    list_tasks, op, running_tasks,
};
use crate::testing::{OrFatal as _, TestHandle, TestResult, cluster, swarm};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_BUILD_INFO, METRIC_DESIRED, METRIC_HEALTH, METRIC_NODES_BY_STATE,
    METRIC_RUNNING, eventually, metrics_match, no_labels, want, want_service, want_sum,
};

/// Go: `TestCluster_Smoke`. The harness and the exporter's basics end to end: node accounting,
/// health, build info, and a global service that runs everywhere without any node pulling an
/// image.
pub async fn cluster_smoke(test: TestHandle) -> TestResult {
    const STACK: &str = "it-smoke";
    let everywhere = ServiceKey::new(STACK, "everywhere");
    let service_id = deploy_service(
        &test,
        STACK,
        everywhere.service,
        &ServiceOpts {
            mode: Mode::Global,
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&everywhere), &[])
        .start(&test)
        .await?;
    let node_count = node_count();

    match get(test.ctx(), &format!("{base_url}/healthz")).await {
        Ok((200, _)) => {}
        other => return Err(test.fatal(format!("/healthz: {other:?}; want 200"))),
    }

    eventually(&test, Duration::from_secs(60), async |ctx| {
        let tasks = list_tasks(ctx, &service_id).await?;
        runs_on_every_node(&running_tasks(&tasks))
    })
    .await?;

    let wants = [
        want(
            METRIC_NODES_BY_STATE,
            labels(&[
                ("role", "manager"),
                ("availability", "active"),
                ("status", "ready"),
            ]),
            1.0,
        ),
        want(
            METRIC_NODES_BY_STATE,
            labels(&[
                ("role", "worker"),
                ("availability", "active"),
                ("status", "ready"),
            ]),
            f64::from(u32::try_from(cluster().workers().len()).unwrap_or(u32::MAX)),
        ),
        want_sum(METRIC_NODES_BY_STATE, no_labels(), node_count),
        want(METRIC_HEALTH, no_labels(), 1.0),
        want_service(METRIC_DESIRED, &everywhere, node_count),
        want_service(METRIC_RUNNING, &everywhere, node_count),
        want_service(METRIC_AT_DESIRED, &everywhere, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &wants).await
    })
    .await?;

    let scraped = scrape_metrics(test.ctx(), &base_url)
        .await
        .or_fatal(&test)?;
    if scraped.value(METRIC_BUILD_INFO, &no_labels()) != Some(1.0) {
        return Err(test.fatal(format!(
            "{METRIC_BUILD_INFO}: want exactly one series = 1, got {:?}",
            scraped.find(METRIC_BUILD_INFO, &no_labels())
        )));
    }

    assert_service_image(&test, &service_id, &cluster().workload_image_id).await?;
    for node in &cluster().nodes {
        assert_no_image_pulls(&test, node).await?;
    }
    Ok(())
}

/// The number of cluster nodes, as a metric value.
pub fn node_count() -> f64 {
    f64::from(u32::try_from(cluster().nodes.len()).unwrap_or(u32::MAX))
}

/// Holds when there is exactly one running task on each cluster node.
pub fn runs_on_every_node(running: &[TaskInfo]) -> Result<(), String> {
    let mut per_node: HashMap<&str, usize> = HashMap::new();
    for task in running {
        *per_node.entry(task.node_id.as_str()).or_default() += 1;
    }
    let nodes = &cluster().nodes;
    let one_each = nodes
        .iter()
        .all(|node| per_node.get(node.swarm_node_id.as_str()) == Some(&1));
    if !one_each {
        return Err(format!(
            "want one running task on each of {} nodes, have:\n{}: not yet",
            nodes.len(),
            describe_tasks(running)
        ));
    }
    if running.len() != nodes.len() {
        return Err(format!(
            "want {} running tasks, have:\n{}: not yet",
            nodes.len(),
            describe_tasks(running)
        ));
    }
    Ok(())
}

async fn assert_service_image(test: &TestHandle, service_id: &str, image_id: &str) -> TestResult {
    let inspected = op(
        test.ctx(),
        swarm().inspect_service(service_id, None::<InspectServiceOptions>),
    )
    .await
    .map_err(|error| test.fatal(format!("inspect service: {error}")))?;
    let got = inspected
        .spec
        .and_then(|spec| spec.task_template)
        .and_then(|template| template.container_spec)
        .and_then(|container| container.image)
        .unwrap_or_default();
    if got != image_id {
        return Err(test.fatal(format!(
            "service image = {got:?}, want the workload image ID {image_id:?}"
        )));
    }
    Ok(())
}

/// Reads the node daemon's image events since the cluster started: there must be no pull, and
/// there must be the load that put the workload image there, which also proves the event query
/// itself sees the node's history.
async fn assert_no_image_pulls(test: &TestHandle, node: &Node) -> TestResult {
    let image_events = node_image_events(test.ctx(), node).await.or_fatal(test)?;
    let mut pulls = Vec::new();
    let mut loads = Vec::new();
    for (action, actor) in image_events {
        match action.as_str() {
            "pull" => pulls.push(actor),
            "load" => loads.push(actor),
            _ => {}
        }
    }
    if !pulls.is_empty() {
        test.error(format!(
            "node {} pulled images since the cluster started: {pulls:?}",
            node.hostname
        ));
    }
    if loads.is_empty() {
        test.error(format!(
            "node {} has no image load event since the cluster started; the event query sees \
             nothing",
            node.hostname
        ));
    }
    Ok(())
}

/// The node's image events since the cluster started, as (action, actor ID) pairs. A bounded
/// window (until now) makes the daemon end the stream once it has replayed the history.
async fn node_image_events(ctx: &Context, node: &Node) -> Result<Vec<(String, String)>, String> {
    let client = node
        .client()
        .map_err(|error| format!("client for {}: {error}", node.hostname))?;
    let options = EventsOptions {
        since: Some(cluster().started_at.as_second().to_string()),
        until: Some((SystemClock.now().as_second() + 1).to_string()),
        filters: Some(HashMap::from([(
            "type".to_owned(),
            vec!["image".to_owned()],
        )])),
    };
    let collect = async {
        let mut stream = client.events(Some(options));
        let mut collected = Vec::new();
        while let Some(message) = stream.next().await {
            let message =
                message.map_err(|error| format!("image events of {}: {error}", node.hostname))?;
            if message.typ != Some(EventMessageTypeEnum::IMAGE) {
                continue;
            }
            collected.push((
                message.action.unwrap_or_default(),
                message.actor.and_then(|actor| actor.id).unwrap_or_default(),
            ));
        }
        Ok(collected)
    };
    ctx.with_timeout(DOCKER_OP_TIMEOUT)
        .run(collect)
        .await
        .map_err(|ended| format!("image events of {}: {ended}", node.hostname))?
}
