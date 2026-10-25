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

//! The steady-state snapshot against the golden file the Go suite shares.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use bollard::models::TaskSpecRestartPolicyConditionEnum;
use testenv::context::Context;
use testenv::metrics_text::{format_sample, format_series, labels};

use crate::cluster_smoke::node_count;
use crate::http::scrape_metrics;
use crate::launcher::Launch;
use crate::swarm::{ServiceKey, ServiceOpts, deploy_service};
use crate::testing::{TestHandle, TestResult};
use crate::wait::{
    LABEL_SERVICE, LABEL_STACK, METRIC_DESIRED, METRIC_NODES_BY_STATE, METRIC_REPLICAS_STATE,
    METRIC_RUNNING, eventually, metrics_match, want, want_service,
};

const SNAPSHOT_STACK: &str = "it-snapshot";
/// The service label the exporter is asked to copy (`--label`).
const TEAM_LABEL: &str = "team";
/// How many consecutive identical snapshots count as converged when rewriting the golden file.
const SNAPSHOT_STABLE_POLLS: usize = 3;
const TIMESTAMP_MASK: &str = "<timestamp>";
/// The golden file's first line. The file is shared with the Go suite and must stay
/// byte-identical, so a rewrite from here keeps the Go suite's header too.
const GOLDEN_HEADER: &str = "# Rewrite with: make go-test-integration UPDATE=1 RUN=TestSnapshot_";
/// Set by `make rust-test-integration UPDATE=1`: rewrite the golden file instead of comparing.
const ENV_UPDATE: &str = "SSE_IT_UPDATE";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../test/integration/testdata/steady_state.golden")
}

/// Go: `TestSnapshot_SteadyState`. Deploys a fixed mix of service shapes and compares
/// everything the exporter says about them, once they have converged, with the golden file: any
/// change to a family, a label or a value in these shapes shows up as a diff.
pub async fn steady_state(test: TestHandle) -> TestResult {
    let shapes = snapshot_services();
    let [web, agent, idle, nowhere, oneshot, custom] = shapes.clone().map(|(service, _)| service);
    for (service, opts) in &shapes {
        deploy_service(&test, SNAPSHOT_STACK, service.service, opts).await?;
    }

    let all: Vec<ServiceKey> = shapes.iter().map(|(service, _)| service.clone()).collect();
    let base_url = Launch::seeded(&all, &["--label", TEAM_LABEL])
        .start(&test)
        .await?;

    let nodes = node_count();
    let wants = [
        want_service(METRIC_RUNNING, &web, 2.0),
        want_service(METRIC_RUNNING, &agent, nodes),
        want_service(METRIC_DESIRED, &idle, 0.0),
        want_service(METRIC_DESIRED, &nowhere, 0.0),
        want(
            METRIC_REPLICAS_STATE,
            labels(&[
                (LABEL_STACK, SNAPSHOT_STACK),
                (LABEL_SERVICE, oneshot.service),
                ("state", "complete"),
            ]),
            1.0,
        ),
        want_service(METRIC_RUNNING, &custom, 1.0),
        want(
            METRIC_DESIRED,
            labels(&[
                (LABEL_STACK, SNAPSHOT_STACK),
                (LABEL_SERVICE, custom.service),
                (TEAM_LABEL, "platform"),
            ]),
            1.0,
        ),
    ];
    eventually(&test, Duration::from_secs(90), async |ctx| {
        metrics_match(ctx, &base_url, SNAPSHOT_STACK, &wants).await
    })
    .await?;

    let path = golden_path();
    if std::env::var(ENV_UPDATE).is_ok_and(|value| value == "1") {
        let snapshot = stable_snapshot(&test, &base_url).await?;
        let content = format!("{GOLDEN_HEADER}\n{}\n", snapshot.join("\n"));
        std::fs::write(&path, content)
            .map_err(|error| test.fatal(format!("write golden file: {error}")))?;
        test.log(format!(
            "rewrote {} ({} series)",
            path.display(),
            snapshot.len()
        ));
        return Ok(());
    }

    let golden = read_golden(&test, &path)?;
    eventually(&test, Duration::from_secs(30), async |ctx| {
        let snapshot = steady_state_snapshot(ctx, &base_url).await?;
        diff_snapshot(&golden, &snapshot)
    })
    .await
}

/// The service shapes the snapshot covers, each with how it is deployed.
fn snapshot_services() -> [(ServiceKey, ServiceOpts); 6] {
    let web = ServiceKey::new(SNAPSHOT_STACK, "web");
    let agent = ServiceKey::new(SNAPSHOT_STACK, "agent");
    let idle = ServiceKey::new(SNAPSHOT_STACK, "idle");
    let nowhere = ServiceKey::new(SNAPSHOT_STACK, "nowhere");
    let oneshot = ServiceKey::new(SNAPSHOT_STACK, "oneshot");
    let custom = ServiceKey::new(SNAPSHOT_STACK, "custom");

    [
        (
            web,
            ServiceOpts {
                replicas: 2,
                ..ServiceOpts::default()
            },
        ),
        (
            agent,
            ServiceOpts {
                global: true,
                ..ServiceOpts::default()
            },
        ),
        (
            idle,
            ServiceOpts {
                zero_replicas: true,
                ..ServiceOpts::default()
            },
        ),
        (
            nowhere,
            ServiceOpts {
                global: true,
                constraints: vec!["node.labels.it-unsatisfiable==true".to_owned()],
                ..ServiceOpts::default()
            },
        ),
        (
            oneshot,
            ServiceOpts {
                command: Some(vec!["true".to_owned()]),
                restart_condition: Some(TaskSpecRestartPolicyConditionEnum::NONE),
                ..ServiceOpts::default()
            },
        ),
        (
            custom,
            ServiceOpts {
                labels: HashMap::from([(TEAM_LABEL.to_owned(), "platform".to_owned())]),
                ..ServiceOpts::default()
            },
        ),
    ]
}

/// The families under test as sorted exposition-style lines: every `swarm_service_*` and
/// `swarm_task_replicas_state` series of the snapshot stack, and every
/// `swarm_cluster_nodes_by_state` series. Timestamps are masked (zero stays zero: it means
/// "never updated").
async fn steady_state_snapshot(ctx: &Context, base_url: &str) -> Result<Vec<String>, String> {
    let scraped = scrape_metrics(ctx, base_url).await?;
    let mut lines: Vec<String> = scraped
        .samples
        .iter()
        .filter(|sample| {
            let in_stack =
                sample.labels.get(LABEL_STACK).map(String::as_str) == Some(SNAPSHOT_STACK);
            let service_family =
                sample.name.starts_with("swarm_service_") || sample.name == METRIC_REPLICAS_STATE;
            (in_stack && service_family) || sample.name == METRIC_NODES_BY_STATE
        })
        .map(|sample| {
            if sample.name.ends_with("_timestamp_seconds") && sample.value != 0.0 {
                format!(
                    "{} {TIMESTAMP_MASK}",
                    format_series(&sample.name, &sample.labels)
                )
            } else {
                format_sample(sample)
            }
        })
        .collect();
    lines.sort();
    Ok(lines)
}

/// Waits until [`SNAPSHOT_STABLE_POLLS`] consecutive snapshots are identical, so the golden file
/// is never written from a state still settling.
async fn stable_snapshot(
    test: &TestHandle,
    base_url: &str,
) -> Result<Vec<String>, crate::testing::Fatal> {
    let mut last: Vec<String> = Vec::new();
    let mut matches = 0;
    eventually(test, Duration::from_secs(30), async |ctx| {
        let snapshot = steady_state_snapshot(ctx, base_url).await?;
        if snapshot == last {
            matches += 1;
        } else {
            last = snapshot;
            matches = 1;
        }
        if matches < SNAPSHOT_STABLE_POLLS {
            return Err(format!("snapshot seen {matches} times in a row: not yet"));
        }
        Ok(())
    })
    .await?;
    Ok(last)
}

fn read_golden(
    test: &TestHandle,
    path: &std::path::Path,
) -> Result<Vec<String>, crate::testing::Fatal> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        test.fatal(format!(
            "read golden file (rewrite it with make rust-test-integration UPDATE=1 \
             RUN=TestSnapshot_): {error}"
        ))
    })?;
    Ok(content
        .split('\n')
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// The lines only in the golden file (`-`) and only in the snapshot (`+`). Both sides are
/// sorted, so a set difference is a complete diff.
fn diff_snapshot(golden: &[String], snapshot: &[String]) -> Result<(), String> {
    let mut diff: Vec<String> = golden
        .iter()
        .filter(|line| !snapshot.contains(line))
        .map(|line| format!("- {line}"))
        .collect();
    diff.extend(
        snapshot
            .iter()
            .filter(|line| !golden.contains(line))
            .map(|line| format!("+ {line}")),
    );
    if diff.is_empty() {
        return Ok(());
    }
    Err(format!(
        "snapshot differs from golden file (- golden, + actual):\n{}",
        diff.join("\n")
    ))
}
