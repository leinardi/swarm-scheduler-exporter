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

//! The integration suite's runner (`harness = false`): one cluster per run, brought up before
//! the first test and torn down after the last, as Go's `TestMain` does; tests run one after
//! another, since they share the cluster; `--run <pattern>` selects them with Go's `-run`
//! semantics, and a pattern that selects nothing fails the run.
//!
//! At this point it carries the harness smoke test only; the Go suite's tests are ported onto it
//! next.

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Write as _};
use std::pin::Pin;
use std::process::ExitCode;
use std::time::Duration;

use bollard::models::NodeState;
use bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptions, ListNetworksOptions, ListNodesOptions,
};
use swarm_scheduler_exporter::gofmt_compat::duration;
use testenv::context::Context;
use testenv::envid::label_filter;
use testenv::run_filter::RunFilter;
use testenv::{Cluster, NodeRole, cleanup_command, spec_from_env, up};
use tokio::time::Instant;

const ENV_SUITE_TIMEOUT: &str = "SSE_IT_SUITE_TIMEOUT";
const DEFAULT_SUITE_TIMEOUT: Duration = Duration::from_mins(12);
/// Bounds the teardown, which runs on a context of its own so it still runs after the suite
/// deadline.
const TEARDOWN_TIMEOUT: Duration = Duration::from_mins(2);
/// Bounds a single Docker call made by a test.
const DOCKER_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// What a test gets: the suite's context and the shared cluster.
struct Suite<'suite> {
    ctx: &'suite Context,
    cluster: &'suite Cluster,
}

type Outcome<'suite> = Pin<Box<dyn Future<Output = Result<(), String>> + 'suite>>;

/// One top-level test.
struct Test {
    name: &'static str,
    run: for<'suite> fn(&'suite Suite<'suite>) -> Outcome<'suite>,
}

const TESTS: &[Test] = &[Test {
    name: "TestHarness_Smoke",
    run: |suite| Box::pin(harness_smoke(suite)),
}];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pattern = match run_pattern(&args) {
        Ok(pattern) => pattern,
        Err(message) => {
            say(&format!(
                "[it] {message}\nusage: integration [--run <pattern>]"
            ));
            return ExitCode::from(2);
        }
    };
    let filter = match RunFilter::parse(&pattern) {
        Ok(filter) => filter,
        Err(error) => {
            say(&format!("[it] {error}"));
            return ExitCode::FAILURE;
        }
    };
    let names: Vec<&str> = TESTS.iter().map(|test| test.name).collect();
    let selected = match filter.select(&names) {
        Ok(selected) => selected,
        Err(error) => {
            say(&format!("[it] {error}"));
            return ExitCode::FAILURE;
        }
    };
    say(&format!(
        "[it] selected {} of {} tests",
        selected.len(),
        names.len()
    ));

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            say(&format!("[it] runtime: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let tests: Vec<&Test> = TESTS
        .iter()
        .filter(|test| selected.contains(&test.name))
        .collect();
    runtime.block_on(run_suite(&tests))
}

/// The `--run` value: `--run <pattern>` or `--run=<pattern>`, the last one winning as Go's flag
/// package lets it.
fn run_pattern(args: &[String]) -> Result<String, String> {
    let mut pattern = String::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if let Some(value) = arg.strip_prefix("--run=") {
            value.clone_into(&mut pattern);
        } else if arg == "--run" {
            rest.next()
                .ok_or_else(|| "flag needs an argument: --run".to_owned())?
                .clone_into(&mut pattern);
        } else {
            return Err(format!("unknown argument {arg:?}"));
        }
    }
    Ok(pattern)
}

async fn run_suite(tests: &[&Test]) -> ExitCode {
    let raw_timeout = std::env::var(ENV_SUITE_TIMEOUT).unwrap_or_default();
    let suite_timeout = if raw_timeout.is_empty() {
        DEFAULT_SUITE_TIMEOUT
    } else {
        match duration::parse(&raw_timeout) {
            Ok(nanos) => Duration::from_nanos(nanos.max(0).unsigned_abs()),
            Err(error) => {
                say(&format!("[it] parse {ENV_SUITE_TIMEOUT}: {error}"));
                return ExitCode::FAILURE;
            }
        }
    };
    let suite_ctx = Context::background().with_timeout(suite_timeout);

    let spec = match spec_from_env(|key| std::env::var(key).ok()) {
        Ok(spec) => spec,
        Err(error) => {
            say(&format!("[it] spec from env: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let env_id = spec.env_id.clone();
    let keep_on_failure = spec.keep_on_failure;

    let started = Instant::now();
    let cluster = match up(&suite_ctx, spec).await {
        Ok(cluster) => cluster,
        Err(error) => {
            say(&format!("[it] cluster up: {error}"));
            return ExitCode::FAILURE;
        }
    };
    say(&format!(
        "[it] cluster {env_id} up in {} ({} nodes)",
        go_duration(started.elapsed().as_millis() * 1_000_000),
        cluster.nodes.len()
    ));

    let suite = Suite {
        ctx: &suite_ctx,
        cluster: &cluster,
    };
    let mut failed = 0_usize;
    for test in tests {
        let started = Instant::now();
        let outcome = (test.run)(&suite).await;
        let elapsed = started.elapsed().as_secs_f64();
        match outcome {
            Ok(()) => say(&format!("--- PASS: {} ({elapsed:.2}s)", test.name)),
            Err(message) => {
                failed += 1;
                say(&format!(
                    "--- FAIL: {} ({elapsed:.2}s)\n    {message}",
                    test.name
                ));
            }
        }
    }
    say(&format!(
        "[it] {} selected, {} passed, {failed} failed",
        tests.len(),
        tests.len() - failed
    ));

    if failed > 0 && keep_on_failure {
        say(&format!(
            "[it] tests failed; keeping cluster {env_id} (manager {}). Clean up with:\n  {}",
            cluster.manager.docker_host,
            cleanup_command(&env_id)
        ));
        return ExitCode::FAILURE;
    }

    let teardown = Context::background().with_timeout(TEARDOWN_TIMEOUT);
    let mut code = if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    if let Err(error) = cluster.down(&teardown).await {
        say(&format!(
            "[it] cluster down: {error}\nClean up with:\n  {}",
            cleanup_command(&env_id)
        ));
        code = ExitCode::FAILURE;
    } else if let Err(message) = assert_nothing_left(&teardown, &cluster).await {
        say(&format!(
            "[it] cluster down left resources behind: {message}"
        ));
        code = ExitCode::FAILURE;
    }
    code
}

/// After `down`, the host has no container or network of the run left.
async fn assert_nothing_left(ctx: &Context, cluster: &Cluster) -> Result<(), String> {
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
async fn harness_smoke(suite: &Suite<'_>) -> Result<(), String> {
    smoke_topology(suite.cluster)?;
    smoke_manager_view(suite.ctx, suite.cluster).await?;
    smoke_workload_image(suite.ctx, suite.cluster).await?;
    smoke_labels(suite.ctx, suite.cluster).await?;
    smoke_pause(suite.ctx, suite.cluster).await
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

/// A Docker call made by a test, under [`DOCKER_OP_TIMEOUT`] and the suite's context.
async fn timed<T, E: std::fmt::Display>(
    ctx: &Context,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    ctx.with_timeout(DOCKER_OP_TIMEOUT)
        .run(call)
        .await
        .map_err(|ended| ended.to_string())?
        .map_err(|error| error.to_string())
}

fn go_duration(nanos: u128) -> String {
    duration::format(i64::try_from(nanos).unwrap_or(i64::MAX))
}

/// One line to stderr, where the harness's log goes too.
fn say(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}
