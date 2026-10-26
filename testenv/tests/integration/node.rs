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

//! Node changes under a running exporter: a drained node, and one that goes down while its tasks
//! are still reported running.

use std::time::Duration;

use bollard::models::{NodeSpecAvailabilityEnum, TaskState};
use testenv::Node;
use testenv::context::Context;
use testenv::metrics_text::labels;

use crate::cluster_smoke::{node_count, runs_on_every_node};
use crate::launcher::Launch;
use crate::swarm::{
    Mode, ServiceKey, ServiceOpts, TaskInfo, deploy_service, describe_tasks, list_tasks,
    running_tasks, teardown_ctx, update_node, update_node_spec, wait_nodes_ready_active,
};
use crate::testing::{Fatal, OrFatal as _, TestHandle, TestResult, cluster};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_NODES_BY_STATE, METRIC_RUNNING, METRIC_SCHEDULABLE,
    eventually, metrics_match, wait_for_fresh_poll, want, want_service, want_sum,
};

/// Bounds the waits around a node change. A paused node is only marked down once its heartbeats
/// have lapsed, which takes Swarm tens of seconds.
const NODE_CHANGE_TIMEOUT: Duration = Duration::from_secs(120);

/// The shared setup of the node tests: a global service on every node, a two-replica service
/// with one replica on each worker, an exporter watching both, and the victim: the worker
/// hosting slot 1 of the replicated service.
struct NodeScenario {
    stack: &'static str,
    base_url: String,
    global: ServiceKey,
    global_id: String,
    replicated: ServiceKey,
    replicated_id: String,
    victim: Node,
    /// The other worker, where the victim's replica must land.
    survivor: Node,
}

impl NodeScenario {
    async fn set_up(test: &TestHandle, stack: &'static str) -> Result<Self, Fatal> {
        let workers = cluster().workers();
        if workers.len() != 2 {
            return Err(test.skip(format!(
                "the node scenarios need exactly 2 workers, the cluster has {}",
                workers.len()
            )));
        }
        let global = ServiceKey::new(stack, "agent");
        let replicated = ServiceKey::new(stack, "web");

        let global_id = Box::pin(deploy_on_every_node(test, stack, &global)).await?;

        let replicated_id = deploy_service(
            test,
            stack,
            replicated.service,
            &ServiceOpts {
                replicas: 2,
                constraints: vec!["node.role==worker".to_owned()],
                ..ServiceOpts::default()
            },
        )
        .await?;
        let mut running: Vec<TaskInfo> = Vec::new();
        eventually(test, Duration::from_secs(60), async |ctx| {
            let tasks = list_tasks(ctx, &replicated_id).await?;
            running = running_tasks(&tasks);
            if running.len() != 2 {
                return Err(format!(
                    "want 2 running replicas, have:\n{}: not yet",
                    describe_tasks(&running)
                ));
            }
            Ok(())
        })
        .await?;

        if running[0].node_id == running[1].node_id {
            return Err(test.fatal(format!(
                "Swarm placed both replicas on node {}; the node scenarios need one replica per \
                 worker:\n{}",
                running[0].node_id,
                describe_tasks(&running)
            )));
        }
        let Some(slot_one) = running.iter().find(|task| task.slot == 1) else {
            return Err(test.fatal(format!(
                "no running replica in slot 1:\n{}",
                describe_tasks(&running)
            )));
        };
        let victim = cluster()
            .node_by_swarm_id(&slot_one.node_id)
            .or_fatal(test)?
            .clone();
        let survivor = workers
            .iter()
            .find(|worker| worker.swarm_node_id != victim.swarm_node_id)
            .map(|&worker| worker.clone())
            .ok_or("no survivor among the workers")
            .or_fatal(test)?;

        let base_url = Launch::seeded(&[global.clone(), replicated.clone()], &[])
            .start(test)
            .await?;

        let nodes = node_count();
        let settled = [
            want_service(METRIC_DESIRED, &global, nodes),
            want_service(METRIC_DESIRED, &replicated, 2.0),
            want_service(METRIC_SCHEDULABLE, &replicated, 2.0),
            want_service(METRIC_RUNNING, &replicated, 2.0),
        ];
        eventually(test, Duration::from_secs(60), async |ctx| {
            metrics_match(ctx, &base_url, stack, &settled).await
        })
        .await?;

        test.log(format!(
            "victim {} ({}), survivor {} ({})",
            victim.hostname, victim.swarm_node_id, survivor.hostname, survivor.swarm_node_id
        ));

        // Registered last, so it runs first: the cluster is whole again before the services go.
        let cleanup_test = test.clone();
        let cleanup_victim = victim.clone();
        test.cleanup(move || async move { restore_node(&cleanup_test, &cleanup_victim).await });

        Ok(Self {
            stack,
            base_url,
            global,
            global_id,
            replicated,
            replicated_id,
            victim,
            survivor,
        })
    }

    /// Holds when slot 1's only running, wanted task sits on the survivor.
    async fn slot_one_moved(&self, ctx: &Context) -> Result<(), String> {
        let tasks = list_tasks(ctx, &self.replicated_id).await?;
        if running_tasks(&tasks)
            .iter()
            .any(|task| task.slot == 1 && task.node_id == self.survivor.swarm_node_id)
        {
            return Ok(());
        }
        Err(format!(
            "slot 1 not running on {} yet:\n{}: not yet",
            self.survivor.hostname,
            describe_tasks(&tasks)
        ))
    }

    /// Holds once the global service's task on the victim is retired (desired shutdown) while
    /// the victim, being down, still reports it running.
    async fn victim_global_task_retired_running(&self, ctx: &Context) -> Result<(), String> {
        let tasks = list_tasks(ctx, &self.global_id).await?;
        if tasks.iter().any(|task| {
            task.node_id == self.victim.swarm_node_id
                && task.desired_state == Some(TaskState::SHUTDOWN)
                && task.state == Some(TaskState::RUNNING)
        }) {
            return Ok(());
        }
        Err(format!(
            "no retired task still reported running on victim {}:\n{}: not yet",
            self.victim.hostname,
            describe_tasks(&tasks)
        ))
    }

    /// Holds when slot 1 has both tasks at once: the stale one on the victim, still reported
    /// running but retired, and its replacement running on the survivor.
    async fn slot_one_stale_and_replaced(&self, ctx: &Context) -> Result<(), String> {
        let tasks = list_tasks(ctx, &self.replicated_id).await?;
        let mut stale = false;
        let mut replacement = false;
        for task in &tasks {
            if task.slot != 1 || task.state != Some(TaskState::RUNNING) {
                continue;
            }
            if task.node_id == self.victim.swarm_node_id
                && task.desired_state == Some(TaskState::SHUTDOWN)
            {
                stale = true;
            } else if task.node_id == self.survivor.swarm_node_id
                && task.desired_state == Some(TaskState::RUNNING)
            {
                replacement = true;
            }
        }
        if stale && replacement {
            return Ok(());
        }
        Err(format!(
            "slot 1: stale task on victim {stale}, replacement on survivor {replacement}:\n{}: \
             not yet",
            describe_tasks(&tasks)
        ))
    }
}

/// Deploys `global` as a global service and waits until it runs on every node.
async fn deploy_on_every_node(
    test: &TestHandle,
    stack: &str,
    global: &ServiceKey,
) -> Result<String, Fatal> {
    let global_id = deploy_service(
        test,
        stack,
        global.service,
        &ServiceOpts {
            mode: Mode::Global,
            ..ServiceOpts::default()
        },
    )
    .await?;
    eventually(test, Duration::from_secs(60), async |ctx| {
        let tasks = list_tasks(ctx, &global_id).await?;
        runs_on_every_node(&running_tasks(&tasks))
    })
    .await?;
    Ok(global_id)
}

/// Unpauses the node and makes it active again, then waits until every node is Ready and
/// Active: the state the next test expects.
async fn restore_node(test: &TestHandle, node: &Node) {
    let ctx = teardown_ctx();
    if let Err(error) = cluster().unpause_node(&ctx, node).await {
        test.error(format!("cleanup: {error}"));
    }
    let activate = update_node_spec(&ctx, &node.swarm_node_id, |spec| {
        spec.availability = Some(NodeSpecAvailabilityEnum::ACTIVE);
    });
    if let Err(error) = activate.await {
        test.error(format!("cleanup: {error}"));
    }
    if let Err(error) = wait_nodes_ready_active(&ctx).await {
        test.error(format!("cleanup: {error}"));
    }
}

/// Go: `TestNode_Drain`. Drains the victim: the global service loses a node, and the replicated
/// service keeps its desired count but can now be scheduled on one worker only, while its slot-1
/// replica moves to the survivor. Setting the victim active again restores both.
pub async fn node_drain(test: TestHandle) -> TestResult {
    let scenario = Box::pin(NodeScenario::set_up(&test, "it-drain")).await?;
    let nodes = node_count();

    update_node(&test, &scenario.victim.swarm_node_id, |spec| {
        spec.availability = Some(NodeSpecAvailabilityEnum::DRAIN);
    })
    .await?;

    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        scenario.slot_one_moved(ctx).await
    })
    .await?;

    // The placement above comes straight from the manager; make sure the exporter has polled it
    // too, or a scrape from before the move could satisfy the checks below.
    wait_for_fresh_poll(&test, &scenario.base_url).await?;

    let drained = [
        want(
            METRIC_NODES_BY_STATE,
            labels(&[("availability", "drain")]),
            1.0,
        ),
        want_service(METRIC_DESIRED, &scenario.global, nodes - 1.0),
        want_service(METRIC_DESIRED, &scenario.replicated, 2.0),
        want_service(METRIC_SCHEDULABLE, &scenario.replicated, 1.0),
        want_service(METRIC_RUNNING, &scenario.replicated, 2.0),
        want_service(METRIC_AT_DESIRED, &scenario.replicated, 1.0),
    ];
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        metrics_match(ctx, &scenario.base_url, scenario.stack, &drained).await
    })
    .await?;

    update_node(&test, &scenario.victim.swarm_node_id, |spec| {
        spec.availability = Some(NodeSpecAvailabilityEnum::ACTIVE);
    })
    .await?;

    let restored = [
        want_sum(
            METRIC_NODES_BY_STATE,
            labels(&[("availability", "drain")]),
            0.0,
        ),
        want_service(METRIC_DESIRED, &scenario.global, nodes),
        want_service(METRIC_SCHEDULABLE, &scenario.replicated, 2.0),
    ];
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        metrics_match(ctx, &scenario.base_url, scenario.stack, &restored).await
    })
    .await
}

/// Go: `TestNode_Down`. Freezes the victim's `DinD` container, so the manager marks it down while
/// its tasks are still reported running. Swarm starts a replacement for slot 1 on the survivor,
/// and the exporter must count one task for that slot, the replacement, not both.
pub async fn node_down(test: TestHandle) -> TestResult {
    let scenario = Box::pin(NodeScenario::set_up(&test, "it-down")).await?;
    let nodes = node_count();

    cluster()
        .pause_node(test.ctx(), &scenario.victim)
        .await
        .or_fatal(&test)?;

    // The slot must hold both tasks at once, or the dedupe is not exercised.
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        scenario.slot_one_stale_and_replaced(ctx).await
    })
    .await?;

    // The global service's task on the victim must be retired while still reported running, or
    // skipping retired tasks that have not stopped is not exercised.
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        scenario.victim_global_task_retired_running(ctx).await
    })
    .await?;

    // A scrape from before the replacement was running would show running 2 even without the
    // dedupe; only a poll of the state above tells counting one task per slot from counting
    // both.
    wait_for_fresh_poll(&test, &scenario.base_url).await?;

    let down = [
        want(METRIC_NODES_BY_STATE, labels(&[("status", "down")]), 1.0),
        want_service(METRIC_DESIRED, &scenario.global, nodes - 1.0),
        want_service(METRIC_RUNNING, &scenario.global, nodes - 1.0),
        want_service(METRIC_AT_DESIRED, &scenario.global, 1.0),
        want_service(METRIC_DESIRED, &scenario.replicated, 2.0),
        want_service(METRIC_SCHEDULABLE, &scenario.replicated, 1.0),
        want_service(METRIC_RUNNING, &scenario.replicated, 2.0),
        want_service(METRIC_AT_DESIRED, &scenario.replicated, 1.0),
    ];
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        metrics_match(ctx, &scenario.base_url, scenario.stack, &down).await
    })
    .await?;

    cluster()
        .unpause_node(test.ctx(), &scenario.victim)
        .await
        .or_fatal(&test)?;

    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        wait_nodes_ready_active(ctx).await
    })
    .await?;

    let restored = [
        want_sum(METRIC_NODES_BY_STATE, labels(&[("status", "down")]), 0.0),
        want_service(METRIC_DESIRED, &scenario.global, nodes),
        want_service(METRIC_SCHEDULABLE, &scenario.replicated, 2.0),
    ];
    eventually(&test, NODE_CHANGE_TIMEOUT, async |ctx| {
        metrics_match(ctx, &scenario.base_url, scenario.stack, &restored).await
    })
    .await
}
