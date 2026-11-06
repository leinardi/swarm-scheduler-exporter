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

use testenv::{ENV_BINARY, cleanup_command, spec_from_env, up};
use testenv_core::run_filter::RunFilter;
use testenv_core::testing::{Runner, RunnerConfig, TallyStyle, Test, TimeoutRule};
use tokio::time::Instant;

use crate::testing::{Suite, cluster, install, suite};
use crate::wait::go_duration;

/// The runner's lines and exit codes, as Go's `TestMain` printed them: a negative
/// `SSE_IT_SUITE_TIMEOUT` is clamped to zero, a test that starts after the deadline still runs
/// (its waits fail on the dead context), and a skipped test counts as passed.
const RUNNER: RunnerConfig = RunnerConfig {
    prefix: "[it]",
    usage: "usage: integration [--run <pattern>]",
    timeout_env: "SSE_IT_SUITE_TIMEOUT",
    default_timeout: Duration::from_mins(12),
    timeout_rule: TimeoutRule::Clamped,
    deadline_check: false,
    tally: TallyStyle::PassedFailed,
};

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
    let runner = Runner::new(RUNNER);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (filter, tests) = match runner.select(&args, TESTS) {
        Ok(selected) => selected,
        Err(code) => return ExitCode::from(code),
    };
    let runtime = match runner.runtime() {
        Ok(runtime) => runtime,
        Err(code) => return ExitCode::from(code),
    };
    runtime.block_on(run_suite(&runner, filter, &tests))
}

async fn run_suite(runner: &Runner, filter: RunFilter, tests: &[&Test]) -> ExitCode {
    // The suite context: ends at the deadline, or on SIGINT or SIGTERM, so the waits return and
    // teardown runs instead of the process being killed mid-test.
    let ctx = match runner
        .suite_timeout(|key| std::env::var(key).ok())
        .and_then(|timeout| runner.suite_context(timeout))
    {
        Ok(ctx) => ctx,
        Err(code) => return ExitCode::from(code),
    };
    let spec = match spec_from_env(|key| std::env::var(key).ok()) {
        Ok(spec) => spec,
        Err(error) => {
            runner.say(&format!("spec from env: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let binary = PathBuf::from(&spec.binary);
    if spec.binary.is_empty() || !binary.exists() {
        runner.say(&format!(
            "{ENV_BINARY} must name the built exporter binary (make test-integration \
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
            runner.say(&format!("cluster up: {error}"));
            return ExitCode::FAILURE;
        }
    };
    runner.say(&format!(
        "cluster {env_id} up in {} ({} nodes)",
        go_duration(Duration::from_millis(
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        )),
        up_cluster.nodes.len()
    ));
    let swarm = match up_cluster.manager.client() {
        Ok(client) => client,
        Err(error) => {
            runner.say(&format!("manager client: {error}"));
            return ExitCode::FAILURE;
        }
    };
    install(Suite {
        cluster: up_cluster,
        swarm,
        exporter_binary: binary,
        ctx: ctx.clone(),
    });

    let failed = runner.run_tests(tests, &suite().ctx, &filter).await.failed;

    if failed > 0 && keep_on_failure {
        runner.say(&format!(
            "tests failed; keeping cluster {env_id} (manager {}). Clean up with:\n  {}",
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
        runner.say(&format!(
            "cluster down: {error}\nClean up with:\n  {}",
            cleanup_command(&env_id)
        ));
        code = ExitCode::FAILURE;
    } else if let Err(message) = harness::assert_nothing_left(&teardown, cluster()).await {
        runner.say(&format!("cluster down left resources behind: {message}"));
        code = ExitCode::FAILURE;
    }
    code
}
