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

//! A removed service's series.

use std::time::Duration;

use testenv::context::Context;
use testenv::metrics_text::{format_sample, labels_include};

use crate::http::scrape_metrics;
use crate::launcher::Launch;
use crate::swarm::{ServiceKey, ServiceOpts, deploy_service, remove_service};
use crate::testing::{TestHandle, TestResult};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_REPLICAS_STATE, METRIC_RUNNING, METRIC_SCHEDULABLE,
    METRIC_UPDATE_COMPLETE, METRIC_UPDATE_STARTED, METRIC_UPDATE_STATE, compare_metrics,
    consistently, eventually, want_service,
};

const STACK: &str = "it-remove";
/// How many exporter polls must pass with no series for the removed service.
const REMOVAL_ABSENCE_POLLS: u32 = 3;

/// Go: `TestService_Removed`. Removing a service deletes every one of its series rather than
/// leaving them at 0: a stale zero would read as "service down" forever.
pub async fn service_removed(test: TestHandle) -> TestResult {
    let doomed = ServiceKey::new(STACK, "doomed");
    let service_id = deploy_service(
        &test,
        STACK,
        doomed.service,
        &ServiceOpts {
            replicas: 1,
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&doomed), &[])
        .start(&test)
        .await?;

    let families = [
        METRIC_DESIRED,
        METRIC_RUNNING,
        METRIC_AT_DESIRED,
        METRIC_SCHEDULABLE,
        METRIC_REPLICAS_STATE,
        METRIC_UPDATE_STATE,
        METRIC_UPDATE_STARTED,
        METRIC_UPDATE_COMPLETE,
    ];

    // Every family must be there first, or its absence afterwards would prove nothing.
    let running = [want_service(METRIC_RUNNING, &doomed, 1.0)];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        let scraped = scrape_metrics(ctx, &base_url).await?;
        if let Some(family) = families
            .iter()
            .find(|family| !scraped.has(family, &doomed.labels()))
        {
            return Err(format!(
                "no {family} series yet for {}: not yet",
                doomed.service
            ));
        }
        compare_metrics(&scraped, STACK, &running)
    })
    .await?;

    remove_service(&test, &service_id).await?;

    eventually(&test, Duration::from_secs(60), async |ctx| {
        no_series_left(ctx, &base_url, &doomed).await
    })
    .await?;

    // Several families are reset and rewritten on every poll, so one scrape that lands
    // mid-rewrite can miss series the exporter still reports: the absence has to hold across
    // polls.
    consistently(&test, &base_url, REMOVAL_ABSENCE_POLLS, async |ctx| {
        no_series_left(ctx, &base_url, &doomed).await
    })
    .await
}

/// Holds when the scrape has no series of the service at all.
async fn no_series_left(ctx: &Context, base_url: &str, doomed: &ServiceKey) -> Result<(), String> {
    let scraped = scrape_metrics(ctx, base_url).await?;
    let wanted = doomed.labels();
    let left: Vec<String> = scraped
        .samples
        .iter()
        .filter(|sample| labels_include(&sample.labels, &wanted))
        .map(format_sample)
        .collect();
    if !left.is_empty() {
        return Err(format!(
            "{} series left for the removed service:\n  {}: not yet",
            left.len(),
            left.join("\n  ")
        ));
    }
    Ok(())
}
