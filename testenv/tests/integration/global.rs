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

//! A global service's desired count against its placement constraints.

use std::collections::HashMap;
use std::time::Duration;

use crate::cluster_smoke::node_count;
use crate::launcher::Launch;
use crate::swarm::{
    Mode, ServiceKey, ServiceOpts, deploy_service, teardown_ctx, update_node, update_node_spec,
    wait_nodes_ready_active,
};
use crate::testing::{OrFatal as _, TestHandle, TestResult, cluster};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_RUNNING, eventually, metrics_match, want_service,
};

const STACK: &str = "it-global";
const ZONE_KEY: &str = "zone";

/// Go: `TestGlobal_Constraints`. A global service's desired count follows its placement
/// constraints the way Swarm evaluates them, including when a node label changes under a
/// running exporter (the node-event path). Swarm's own running count is asserted alongside as
/// ground truth.
pub async fn global_constraints(test: TestHandle) -> TestResult {
    let zone_rule = format!("node.labels.{ZONE_KEY}");
    let nodes = node_count();
    let zoned = ServiceKey::new(STACK, "zoned");
    let elsewhere = ServiceKey::new(STACK, "elsewhere");

    deploy_service(
        &test,
        STACK,
        zoned.service,
        &ServiceOpts {
            mode: Mode::Global,
            constraints: vec![format!("{zone_rule}==a")],
            ..ServiceOpts::default()
        },
    )
    .await?;
    // != against nodes without the label: a missing label compares as empty, so it matches.
    deploy_service(
        &test,
        STACK,
        elsewhere.service,
        &ServiceOpts {
            mode: Mode::Global,
            constraints: vec![format!("{zone_rule}!=a")],
            ..ServiceOpts::default()
        },
    )
    .await?;

    let base_url = Launch::seeded(&[zoned.clone(), elsewhere.clone()], &[])
        .start(&test)
        .await?;

    let before = [
        want_service(METRIC_DESIRED, &zoned, 0.0),
        want_service(METRIC_RUNNING, &zoned, 0.0),
        want_service(METRIC_AT_DESIRED, &zoned, 1.0),
        want_service(METRIC_DESIRED, &elsewhere, nodes),
        want_service(METRIC_RUNNING, &elsewhere, nodes),
        want_service(METRIC_AT_DESIRED, &elsewhere, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &before).await
    })
    .await?;

    // Any node will do; the last one is a worker whenever the cluster has one.
    let zoned_node = cluster()
        .nodes
        .last()
        .ok_or("the cluster has no node")
        .or_fatal(&test)?
        .swarm_node_id
        .clone();

    let cleanup_test = test.clone();
    let cleanup_node = zoned_node.clone();
    test.cleanup(move || async move {
        let ctx = teardown_ctx();
        let unlabel = update_node_spec(&ctx, &cleanup_node, |spec| {
            if let Some(labels) = spec.labels.as_mut() {
                labels.remove(ZONE_KEY);
            }
        });
        if let Err(error) = unlabel.await {
            cleanup_test.error(format!("cleanup: {error}"));
        }
        if let Err(error) = wait_nodes_ready_active(&ctx).await {
            cleanup_test.error(format!("cleanup: {error}"));
        }
    });

    // Upper case on the node, lower case in the constraints: Swarm compares label values
    // case-insensitively, for == and != alike.
    update_node(&test, &zoned_node, |spec| {
        spec.labels
            .get_or_insert_with(HashMap::new)
            .insert(ZONE_KEY.to_owned(), "A".to_owned());
    })
    .await?;

    let after = [
        want_service(METRIC_DESIRED, &zoned, 1.0),
        want_service(METRIC_RUNNING, &zoned, 1.0),
        want_service(METRIC_AT_DESIRED, &zoned, 1.0),
        want_service(METRIC_DESIRED, &elsewhere, nodes - 1.0),
        want_service(METRIC_RUNNING, &elsewhere, nodes - 1.0),
        want_service(METRIC_AT_DESIRED, &elsewhere, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &after).await
    })
    .await
}
