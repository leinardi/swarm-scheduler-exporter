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

//! A replicated service scaled up and down.

use std::time::Duration;

use testenv::metrics_text::labels;

use crate::launcher::Launch;
use crate::swarm::{
    ServiceKey, ServiceOpts, deploy_service, describe_tasks, list_tasks, scale_service,
};
use crate::testing::{TestHandle, TestResult};
use crate::wait::{
    LABEL_SERVICE, LABEL_STACK, METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_REPLICAS_STATE,
    METRIC_RUNNING, eventually, metrics_match, wait_for_fresh_poll, want, want_service,
};

const STACK: &str = "it-scale";

/// Go: `TestReplicated_Scale`. Scales a replicated service 1, 3, then 0. desired, running and
/// `at_desired` follow, and at 0 the service keeps its series (at 0, not deleted) although it has
/// no task left to count.
pub async fn replicated_scale(test: TestHandle) -> TestResult {
    let web = ServiceKey::new(STACK, "web");
    let service_id = deploy_service(
        &test,
        STACK,
        web.service,
        &ServiceOpts {
            replicas: 1,
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&web), &[])
        .start(&test)
        .await?;

    let expect_replicas = async |count: f64| {
        let wants = [
            want_service(METRIC_DESIRED, &web, count),
            want_service(METRIC_RUNNING, &web, count),
            want_service(METRIC_AT_DESIRED, &web, 1.0),
            want(
                METRIC_REPLICAS_STATE,
                labels(&[
                    (LABEL_STACK, STACK),
                    (LABEL_SERVICE, web.service),
                    ("state", "running"),
                ]),
                count,
            ),
        ];
        eventually(&test, Duration::from_secs(60), async |ctx| {
            metrics_match(ctx, &base_url, STACK, &wants).await
        })
        .await
    };

    expect_replicas(1.0).await?;

    scale_service(&test, &service_id, 3).await?;
    expect_replicas(3.0).await?;

    scale_service(&test, &service_id, 0).await?;

    // Scaled-down tasks linger in shutdown until Swarm reaps them, and while they exist the
    // exporter counts them the ordinary way. Only a poll that starts after the last one is gone
    // exercises the service-without-tasks path.
    eventually(&test, Duration::from_secs(60), async |ctx| {
        let tasks = list_tasks(ctx, &service_id).await?;
        if !tasks.is_empty() {
            return Err(format!(
                "scaled-down tasks not reaped yet:\n{}: not yet",
                describe_tasks(&tasks)
            ));
        }
        Ok(())
    })
    .await?;

    wait_for_fresh_poll(&test, &base_url).await?;
    expect_replicas(0.0).await
}
