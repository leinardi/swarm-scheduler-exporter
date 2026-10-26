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

//! Replicated and global jobs, once they have finished.

use std::time::Duration;

use testenv::metrics_text::Labels;

use crate::cluster_smoke::node_count;
use crate::launcher::Launch;
use crate::swarm::{Mode, ServiceKey, ServiceOpts, deploy_service};
use crate::testing::{TestHandle, TestResult};
use crate::wait::{
    METRIC_AT_DESIRED, METRIC_DESIRED, METRIC_REPLICAS_STATE, METRIC_RUNNING, METRIC_SCHEDULABLE,
    MetricWant, eventually, metrics_match, want,
};

const STACK: &str = "it-job";
const COMPLETIONS: u64 = 5;
const LABEL_SERVICE_MODE: &str = "service_mode";
/// Bounds the wait for both jobs to finish: the replicated job runs its completions one at a
/// time (Swarm's default `MaxConcurrent` is 1).
const JOB_COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// Go: `TestJobs_Finished`. Replicated-job and global-job services are reported under their own
/// `service_mode`, with `desired_replicas` at the replicated job's completions or the global
/// job's eligible nodes, and a finished job reads as at desired, with `schedulable_replicas` at
/// 0 so "running below schedulable" never fires for it.
pub async fn jobs_finished(test: TestHandle) -> TestResult {
    let nodes = node_count();
    let replicated = ServiceKey::new(STACK, "batch");
    let global = ServiceKey::new(STACK, "sweep");
    // Exits 0 at once, so each task ends in the complete state.
    let exit_ok = Some(vec!["true".to_owned()]);

    deploy_service(
        &test,
        STACK,
        replicated.service,
        &ServiceOpts {
            mode: Mode::ReplicatedJob,
            completions: COMPLETIONS,
            command: exit_ok.clone(),
            ..ServiceOpts::default()
        },
    )
    .await?;
    deploy_service(
        &test,
        STACK,
        global.service,
        &ServiceOpts {
            mode: Mode::GlobalJob,
            command: exit_ok,
            ..ServiceOpts::default()
        },
    )
    .await?;

    let base_url = Launch::seeded(&[replicated.clone(), global.clone()], &[])
        .start(&test)
        .await?;

    let completions = f64::from(u32::try_from(COMPLETIONS).unwrap_or(u32::MAX));
    let wants = [
        want_job(METRIC_DESIRED, &replicated, "replicated-job", completions),
        want_job_complete(&replicated, "replicated-job", completions),
        want_job(METRIC_RUNNING, &replicated, "replicated-job", 0.0),
        want_job(METRIC_AT_DESIRED, &replicated, "replicated-job", 1.0),
        want_job(METRIC_SCHEDULABLE, &replicated, "replicated-job", 0.0),
        want_job(METRIC_DESIRED, &global, "global-job", nodes),
        want_job_complete(&global, "global-job", nodes),
        want_job(METRIC_RUNNING, &global, "global-job", 0.0),
        want_job(METRIC_AT_DESIRED, &global, "global-job", 1.0),
        want_job(METRIC_SCHEDULABLE, &global, "global-job", 0.0),
    ];
    eventually(&test, JOB_COMPLETION_TIMEOUT, async |ctx| {
        metrics_match(ctx, &base_url, STACK, &wants).await
    })
    .await
}

/// The labels of the service's series under service mode `mode`.
fn job_labels(service: &ServiceKey, mode: &str) -> Labels {
    let mut labels = service.labels();
    labels.insert(LABEL_SERVICE_MODE.to_owned(), mode.to_owned());
    labels
}

fn want_job(name: &'static str, service: &ServiceKey, mode: &str, value: f64) -> MetricWant {
    want(name, job_labels(service, mode), value)
}

/// The service's tasks in the complete state.
fn want_job_complete(service: &ServiceKey, mode: &str, value: f64) -> MetricWant {
    let mut labels = job_labels(service, mode);
    labels.insert("state".to_owned(), "complete".to_owned());
    want(METRIC_REPLICAS_STATE, labels, value)
}
