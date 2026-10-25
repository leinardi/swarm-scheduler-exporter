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

//! The exporter process itself: its port retry and its `--healthcheck` probe.

use std::time::Duration;

use testenv::context::Context;
use tokio::process::Command;

use crate::launcher::Launch;
use crate::proxy::DockerProxy;
use crate::testing::{OrFatal as _, TestHandle, TestResult, suite};
use crate::wait::{METRIC_HEALTH, eventually, metrics_match, no_labels, want};

/// Go: `TestExporter_PortRetry`. The first port the launcher probes is taken: the exporter's
/// bind fails, and the launcher must notice and succeed on a new port.
pub async fn port_retry(test: TestHandle) -> TestResult {
    let base_url = Launch::seeded(&[], &[])
        .occupying_first_probed_port()
        .start(&test)
        .await?;
    let wants = [want(METRIC_HEALTH, no_labels(), 1.0)];
    eventually(&test, Duration::from_secs(30), async |ctx| {
        metrics_match(ctx, &base_url, "", &wants).await
    })
    .await
}

/// Go: `TestExporter_HealthcheckHealthy`. The binary in `--healthcheck` mode against a ready
/// exporter exits 0.
pub async fn healthcheck_healthy(test: TestHandle) -> TestResult {
    let base_url = Launch::seeded(&[], &[]).start(&test).await?;
    eventually(&test, Duration::from_secs(30), async |ctx| {
        let (code, output) = run_healthcheck_probe(ctx, &base_url).await?;
        if code != 0 {
            return Err(format!("--healthcheck exited {code} ({output:?}): not yet"));
        }
        Ok(())
    })
    .await
}

/// Go: `TestExporter_HealthcheckUnhealthy`. The probe against an exporter whose service list
/// keeps failing exits 1 and prints why.
pub async fn healthcheck_unhealthy(test: TestHandle) -> TestResult {
    let proxy = DockerProxy::start(&test).await?;
    proxy.set_service_list_failing(true);
    let base_url = Launch::serving(&proxy.docker_host(), &[])
        .start(&test)
        .await?;

    let (code, output) = run_healthcheck_probe(test.ctx(), &base_url)
        .await
        .or_fatal(&test)?;
    if code != 1 || !output.contains("initial resync not completed") {
        test.error(format!(
            "--healthcheck during a failing seed exited {code} ({output:?}), want 1 and \
             \"initial resync not completed\""
        ));
    }
    Ok(())
}

/// Runs the exporter binary with `--healthcheck` against the exporter at `base_url`: its exit
/// code and combined output. An error only when the probe could not run.
async fn run_healthcheck_probe(ctx: &Context, base_url: &str) -> Result<(i32, String), String> {
    let address = base_url.trim_start_matches("http://");
    let run = Command::new(&suite().exporter_binary)
        .args(["--healthcheck", "--listen-addr", address])
        .kill_on_drop(true)
        .output();
    let output = ctx
        .run(run)
        .await
        .map_err(|ended| format!("run --healthcheck: {ended}"))?
        .map_err(|error| format!("run --healthcheck: {error}"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let code = output
        .status
        .code()
        .ok_or_else(|| format!("run --healthcheck: {}", output.status))?;
    Ok((code, combined))
}
