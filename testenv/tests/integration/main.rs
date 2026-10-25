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

//! The integration suite (`harness = false`): it runs the exporter binary under test
//! (`SSE_IT_BINARY`, required) against a throwaway multi-node Swarm of `DinD` containers and checks
//! what it exposes on `/metrics`.
//!
//! As Go's `TestMain` did, one cluster serves the whole run: it is brought up before the first
//! test and torn down after the last, on a context of its own, so teardown still runs after the
//! suite deadline (`SSE_IT_SUITE_TIMEOUT`, default 12m) or an interrupt; with
//! `SSE_IT_KEEP_ON_FAILURE` a failed run keeps it. Tests run one after another, since they share
//! the cluster and several of them disrupt its nodes, and keep their Go names. `--run <pattern>`
//! selects them with Go's `-run` semantics, subtests included, and a pattern that selects no test
//! fails the run. Each test and subtest prints one `--- PASS` or `--- FAIL` line with its
//! seconds, then the run prints the selected, passed and failed counts; any failure exits 1.

mod cluster_smoke;
mod containers;
mod events;
mod exporter_process;
mod global;
mod harness;
mod http;
mod jobs;
mod launcher;
mod node;
mod proxy;
mod replicated;
mod service;
mod snapshot;
mod socket_proxy;
mod swarm;
mod testing;
mod update;
mod wait;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use swarm_scheduler_exporter::gofmt_compat::duration;
use testenv::context::Context;
use testenv::run_filter::RunFilter;
use testenv::{ENV_BINARY, cleanup_command, spec_from_env, up};
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::Instant;

use crate::testing::{Suite, TestResult, cluster, install, run_test, say, suite};
use crate::wait::go_duration;

const ENV_SUITE_TIMEOUT: &str = "SSE_IT_SUITE_TIMEOUT";
const DEFAULT_SUITE_TIMEOUT: Duration = Duration::from_mins(12);

/// One top-level test, under its Go name.
struct Test {
    name: &'static str,
    run: fn(testing::TestHandle) -> LocalBoxFuture<'static, TestResult>,
}

/// Every test, in the order they run.
const TESTS: &[Test] = &[
    Test {
        name: "TestHarness_Smoke",
        run: |test| Box::pin(harness::harness_smoke(test)),
    },
    Test {
        name: "TestCluster_Smoke",
        run: |test| Box::pin(cluster_smoke::cluster_smoke(test)),
    },
    Test {
        name: "TestContainers_OptIn",
        run: |test| Box::pin(containers::containers_opt_in(test)),
    },
    Test {
        name: "TestEvents_ReconnectResyncs",
        run: |test| Box::pin(events::reconnect_resyncs(test)),
    },
    Test {
        name: "TestSeed_FailedThenRecovers",
        run: |test| Box::pin(events::seed_failed_then_recovers(test)),
    },
    Test {
        name: "TestExporter_PortRetry",
        run: |test| Box::pin(exporter_process::port_retry(test)),
    },
    Test {
        name: "TestExporter_HealthcheckHealthy",
        run: |test| Box::pin(exporter_process::healthcheck_healthy(test)),
    },
    Test {
        name: "TestExporter_HealthcheckUnhealthy",
        run: |test| Box::pin(exporter_process::healthcheck_unhealthy(test)),
    },
    Test {
        name: "TestGlobal_Constraints",
        run: |test| Box::pin(global::global_constraints(test)),
    },
    Test {
        name: "TestJobs_Finished",
        run: |test| Box::pin(jobs::jobs_finished(test)),
    },
    Test {
        name: "TestNode_Drain",
        run: |test| Box::pin(node::node_drain(test)),
    },
    Test {
        name: "TestNode_Down",
        run: |test| Box::pin(node::node_down(test)),
    },
    Test {
        name: "TestReplicated_Scale",
        run: |test| Box::pin(replicated::replicated_scale(test)),
    },
    Test {
        name: "TestService_Removed",
        run: |test| Box::pin(service::service_removed(test)),
    },
    Test {
        name: "TestSnapshot_SteadyState",
        run: |test| Box::pin(snapshot::steady_state(test)),
    },
    Test {
        name: "TestSocketProxy_ExampleAllowlist",
        run: |test| Box::pin(socket_proxy::example_allowlist(test)),
    },
    Test {
        name: "TestUpdate_Completed",
        run: |test| Box::pin(update::update_completed(test)),
    },
    Test {
        name: "TestUpdate_StartFirstFailure",
        run: |test| Box::pin(update::update_start_first_failure(test)),
    },
];

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
    let tests: Vec<&Test> = TESTS
        .iter()
        .filter(|test| selected.contains(&test.name))
        .collect();

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
    runtime.block_on(run_suite(filter, &tests))
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

/// The suite context: ends at the deadline, or on SIGINT or SIGTERM, so the waits return and
/// teardown runs instead of the process being killed mid-test.
fn suite_context() -> Result<Context, String> {
    let raw = std::env::var(ENV_SUITE_TIMEOUT).unwrap_or_default();
    let timeout = if raw.is_empty() {
        DEFAULT_SUITE_TIMEOUT
    } else {
        let nanos =
            duration::parse(&raw).map_err(|error| format!("parse {ENV_SUITE_TIMEOUT}: {error}"))?;
        Duration::from_nanos(nanos.max(0).unsigned_abs())
    };
    let ctx = Context::background().with_timeout(timeout);
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(|error| format!("watch SIGINT: {error}"))?;
    let mut terminate =
        signal(SignalKind::terminate()).map_err(|error| format!("watch SIGTERM: {error}"))?;
    let cancel = ctx.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        cancel.cancel();
    });
    Ok(ctx)
}

async fn run_suite(filter: RunFilter, tests: &[&Test]) -> ExitCode {
    let ctx = match suite_context() {
        Ok(ctx) => ctx,
        Err(message) => {
            say(&format!("[it] {message}"));
            return ExitCode::FAILURE;
        }
    };
    let spec = match spec_from_env(|key| std::env::var(key).ok()) {
        Ok(spec) => spec,
        Err(error) => {
            say(&format!("[it] spec from env: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let binary = PathBuf::from(&spec.binary);
    if spec.binary.is_empty() || !binary.exists() {
        say(&format!(
            "[it] {ENV_BINARY} must name the built exporter binary (make rust-test-integration \
             sets it): {:?}",
            spec.binary
        ));
        return ExitCode::FAILURE;
    }
    let env_id = spec.env_id.clone();
    let keep_on_failure = spec.keep_on_failure;

    let started = Instant::now();
    let up_cluster = match up(&ctx, spec).await {
        Ok(cluster) => cluster,
        Err(error) => {
            say(&format!("[it] cluster up: {error}"));
            return ExitCode::FAILURE;
        }
    };
    say(&format!(
        "[it] cluster {env_id} up in {} ({} nodes)",
        go_duration(Duration::from_millis(
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        )),
        up_cluster.nodes.len()
    ));
    let swarm = match up_cluster.manager.client() {
        Ok(client) => client,
        Err(error) => {
            say(&format!("[it] manager client: {error}"));
            return ExitCode::FAILURE;
        }
    };
    install(Suite {
        cluster: up_cluster,
        swarm,
        exporter_binary: binary,
        filter,
        ctx: ctx.clone(),
    });

    let mut failed = 0_usize;
    for test in tests {
        if !run_test(test.name, &suite().ctx, test.run).await {
            failed += 1;
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
            cluster().manager.docker_host,
            cleanup_command(&env_id)
        ));
        return ExitCode::FAILURE;
    }

    let teardown = swarm::teardown_ctx();
    let mut code = if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    if let Err(error) = cluster().down(&teardown).await {
        say(&format!(
            "[it] cluster down: {error}\nClean up with:\n  {}",
            cleanup_command(&env_id)
        ));
        code = ExitCode::FAILURE;
    } else if let Err(message) = harness::assert_nothing_left(&teardown, cluster()).await {
        say(&format!(
            "[it] cluster down left resources behind: {message}"
        ));
        code = ExitCode::FAILURE;
    }
    code
}
