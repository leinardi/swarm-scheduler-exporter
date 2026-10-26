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

//! The event stream's reconnect and the seed's retry, through the fault proxy.

use std::time::Duration;

use crate::http::{get, scrape_metrics};
use crate::launcher::{Launch, readiness};
use crate::proxy::DockerProxy;
use crate::swarm::{ServiceKey, ServiceOpts, deploy_service, remove_service};
use crate::testing::{OrFatal as _, TestHandle, TestResult};
use crate::wait::{
    METRIC_DESIRED, METRIC_EVENTS_RECONNECTS, METRIC_RUNNING, compare_metrics, eventually,
    no_labels, want_service,
};

/// Go: `TestEvents_ReconnectResyncs`. A service removed while the event stream was down, whose
/// remove event is never replayed, is dropped once the stream reconnects. The replay cannot
/// catch it, since the proxy brings the stream back without its history, and the periodic
/// resync is minutes away: only the resync every reconnect requests can.
pub async fn reconnect_resyncs(test: TestHandle) -> TestResult {
    const STACK: &str = "it-reconnect";
    let kept = ServiceKey::new(STACK, "kept");
    let doomed = ServiceKey::new(STACK, "doomed");
    let one = ServiceOpts {
        replicas: 1,
        ..ServiceOpts::default()
    };
    deploy_service(&test, STACK, kept.service, &one).await?;
    let doomed_id = deploy_service(&test, STACK, doomed.service, &one).await?;

    let proxy = DockerProxy::start(&test).await?;
    let base_url = Launch::seeded(&[kept.clone(), doomed.clone()], &[])
        .via(&proxy.docker_host())
        .start(&test)
        .await?;

    let both = [
        want_service(METRIC_RUNNING, &kept, 1.0),
        want_service(METRIC_RUNNING, &doomed, 1.0),
    ];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        let scraped = scrape_metrics(ctx, &base_url).await?;
        compare_metrics(&scraped, STACK, &both)
    })
    .await?;

    let connects_before = proxy.event_connect_count();
    proxy.cut_event_streams();
    remove_service(&test, &doomed_id).await?;
    proxy.restore_event_streams();

    let kept_running = [want_service(METRIC_RUNNING, &kept, 1.0)];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        if proxy.event_connect_count() <= connects_before {
            return Err("event stream not reconnected yet: not yet".to_owned());
        }
        let scraped = scrape_metrics(ctx, &base_url).await?;
        let reconnects = scraped.sum(METRIC_EVENTS_RECONNECTS, &no_labels());
        if reconnects < 1.0 {
            return Err(format!(
                "{METRIC_EVENTS_RECONNECTS} = {reconnects}, want at least 1: not yet"
            ));
        }
        if scraped.has(METRIC_DESIRED, &doomed.labels()) {
            return Err(format!(
                "{METRIC_DESIRED} still has a series for the removed service: not yet"
            ));
        }
        compare_metrics(&scraped, STACK, &kept_running)
    })
    .await
}

/// Go: `TestSeed_FailedThenRecovers`. A first resync that fails leaves the exporter unhealthy
/// with no service series, and the retried one seeds it once the Docker API recovers.
pub async fn seed_failed_then_recovers(test: TestHandle) -> TestResult {
    const STACK: &str = "it-seed";
    let web = ServiceKey::new(STACK, "web");
    deploy_service(
        &test,
        STACK,
        web.service,
        &ServiceOpts {
            replicas: 1,
            ..ServiceOpts::default()
        },
    )
    .await?;

    let proxy = DockerProxy::start(&test).await?;
    proxy.set_service_list_failing(true);
    let base_url = Launch::serving(&proxy.docker_host(), &[])
        .start(&test)
        .await?;

    // At least two failures: the first resync, and a retry of it.
    eventually(&test, Duration::from_secs(30), async |_| {
        let failures = proxy.service_list_failure_count();
        if failures < 2 {
            return Err(format!(
                "{failures} failed service lists, want a retried seed: not yet"
            ));
        }
        Ok(())
    })
    .await?;

    let (status, body) = get(test.ctx(), &format!("{base_url}/healthz"))
        .await
        .or_fatal(&test)?;
    let body = String::from_utf8_lossy(&body);
    if status != 503 || !body.contains("initial resync not completed") {
        test.error(format!(
            "/healthz during a failing seed = {status} {body:?}, want 503 \"initial resync not \
             completed\""
        ));
    }

    let scraped = scrape_metrics(test.ctx(), &base_url)
        .await
        .or_fatal(&test)?;
    if scraped.has(METRIC_DESIRED, &web.labels()) || scraped.has(METRIC_RUNNING, &web.labels()) {
        test.error("service series published before the first resync succeeded");
    }

    proxy.set_service_list_failing(false);

    let web_running = [want_service(METRIC_RUNNING, &web, 1.0)];
    eventually(&test, Duration::from_secs(60), async |ctx| {
        readiness(ctx, &base_url, std::slice::from_ref(&web)).await?;
        let scraped = scrape_metrics(ctx, &base_url).await?;
        compare_metrics(&scraped, STACK, &web_running)
    })
    .await
}
