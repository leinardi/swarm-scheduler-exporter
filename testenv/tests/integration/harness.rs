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

//! The harness's own smoke test (new to the Rust suite: the Go suite had none of its own), and
//! the check that `down` leaves nothing behind.

use std::collections::HashMap;

use bollard::models::NodeState;
use bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptions, ListNetworksOptions, ListNodesOptions,
};
use testenv::context::Context;
use testenv::envid::label_filter;
use testenv::{Cluster, NodeRole};

use crate::swarm::op;
use crate::testing::{TestHandle, TestResult, cluster};

/// After `down`, the host has no container or network of the run left.
pub async fn assert_nothing_left(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
    let filters = HashMap::from([("label".to_owned(), vec![label_filter(&cluster.spec.env_id)])]);
    let host = cluster.host_client();
    let containers = timed(
        ctx,
        host.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters.clone()),
            ..ListContainersOptions::default()
        })),
    )
    .await?;
    let networks = timed(
        ctx,
        host.list_networks(Some(ListNetworksOptions {
            filters: Some(filters),
        })),
    )
    .await?;
    if containers.is_empty() && networks.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} containers and {} networks still carry the envid",
        containers.len(),
        networks.len()
    ))
}

/// The harness itself: the topology `up` reports matches what the daemons say, every node
/// carries the run's labels, the workload image is on every node by ID, and a node can be
/// paused and unpaused (twice, since unpausing a running node must succeed).
pub async fn harness_smoke(test: TestHandle) -> TestResult {
    let (ctx, cluster) = (test.ctx(), cluster());
    let checks = async {
        smoke_topology(cluster)?;
        smoke_manager_view(ctx, cluster).await?;
        smoke_workload_image(ctx, cluster).await?;
        smoke_labels(ctx, cluster).await?;
        smoke_pause(ctx, cluster).await
    };
    checks.await.map_err(|message| test.fatal(message))
}

/// One manager first, then the workers, each findable by its Swarm node ID.
fn smoke_topology(cluster: &Cluster) -> Result<(), String> {
    let workers = cluster.spec.workers;
    check(
        cluster.nodes.len() == workers + 1,
        format!("{} nodes, want {}", cluster.nodes.len(), workers + 1),
    )?;
    check(
        cluster.nodes.first() == Some(&cluster.manager)
            && cluster.manager.role == NodeRole::Manager,
        "the manager is not the first node".to_owned(),
    )?;
    let worker_nodes = cluster.workers();
    check(
        worker_nodes.len() == workers
            && worker_nodes
                .iter()
                .all(|node| node.role == NodeRole::Worker),
        format!("workers() returned {worker_nodes:?}"),
    )?;
    for node in &cluster.nodes {
        let found = cluster
            .node_by_swarm_id(&node.swarm_node_id)
            .map_err(|error| error.to_string())?;
        check(
            found == node,
            format!("{} found as {}", node.name, found.name),
        )?;
    }
    check(
        cluster.node_by_swarm_id("not-a-node").is_err(),
        "an unknown Swarm node ID was found".to_owned(),
    )?;
    Ok(())
}

/// The manager sees every node Ready, under the IDs the harness recorded.
async fn smoke_manager_view(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
    let manager = cluster
        .manager
        .client()
        .map_err(|error| error.to_string())?;
    let listed = timed(ctx, manager.list_nodes(None::<ListNodesOptions>)).await?;
    let mut ready: Vec<String> = listed
        .iter()
        .filter(|node| {
            node.status.as_ref().and_then(|status| status.state) == Some(NodeState::READY)
        })
        .filter_map(|node| node.id.clone())
        .collect();
    let mut recorded: Vec<String> = cluster
        .nodes
        .iter()
        .map(|node| node.swarm_node_id.clone())
        .collect();
    ready.sort();
    recorded.sort();
    check(
        ready == recorded,
        format!("the manager lists {ready:?} Ready, the harness recorded {recorded:?}"),
    )?;
    Ok(())
}

/// The workload image is on every node, by the ID services will reference.
async fn smoke_workload_image(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
    check(
        cluster.workload_image_id.starts_with("sha256:"),
        format!("workload image ID {:?}", cluster.workload_image_id),
    )?;
    for node in &cluster.nodes {
        let client = node.client().map_err(|error| error.to_string())?;
        timed(ctx, client.inspect_image(&cluster.workload_image_id))
            .await
            .map_err(|error| format!("{}: {error}", node.name))?;
    }

    Ok(())
}

/// Every node container carries the run's labels, which down and the sweep select by.
async fn smoke_labels(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
    let host = cluster.host_client();
    let containers = timed(
        ctx,
        host.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_owned(),
                vec![label_filter(&cluster.spec.env_id)],
            )])),
            ..ListContainersOptions::default()
        })),
    )
    .await?;
    check(
        containers.len() == cluster.nodes.len(),
        format!(
            "{} labelled containers, want {}",
            containers.len(),
            cluster.nodes.len()
        ),
    )?;
    let want = cluster.labels();
    for container in &containers {
        let labels = container.labels.clone().unwrap_or_default();
        for (key, value) in &want {
            check(
                labels.get(key) == Some(value),
                format!(
                    "{:?} has {key}={:?}, want {value:?}",
                    container.names,
                    labels.get(key)
                ),
            )?;
        }
    }

    Ok(())
}

/// Pause and unpause a worker; a second unpause is a no-op.
async fn smoke_pause(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
    let host = cluster.host_client();
    let worker_nodes = cluster.workers();
    let Some(worker) = worker_nodes.first().copied() else {
        return Ok(());
    };
    let paused = async |want: bool| -> Result<(), String> {
        let inspected = timed(
            ctx,
            host.inspect_container(&worker.container_id, None::<InspectContainerOptions>),
        )
        .await?;
        let paused = inspected
            .state
            .and_then(|state| state.paused)
            .unwrap_or(false);
        check(
            paused == want,
            format!("{} paused={paused}, want {want}", worker.name),
        )
    };
    cluster
        .pause_node(ctx, worker)
        .await
        .map_err(|error| error.to_string())?;
    paused(true).await?;
    cluster
        .unpause_node(ctx, worker)
        .await
        .map_err(|error| error.to_string())?;
    paused(false).await?;
    cluster
        .unpause_node(ctx, worker)
        .await
        .map_err(|error| format!("unpausing a running node: {error}"))?;
    Ok(())
}

fn check(condition: bool, message: String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message) }
}

/// A Docker call, under the test's timeout, with its error as text.
async fn timed<V>(
    ctx: &Context,
    call: impl std::future::Future<Output = Result<V, bollard::errors::Error>>,
) -> Result<V, String> {
    op(ctx, call).await.map_err(|error| error.to_string())
}
