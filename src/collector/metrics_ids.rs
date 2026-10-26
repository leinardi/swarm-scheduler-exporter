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

//! The Prometheus namespace, subsystems, label names and service modes every collector shares.

/// The namespace of every metric.
pub const NAMESPACE: &str = "swarm";

/// The subsystem of the exporter's own metrics.
pub const EXPORTER_SUBSYSTEM: &str = "exporter";
/// The subsystem of the per-service metrics.
pub const SERVICE_SUBSYSTEM: &str = "service";
/// The subsystem of the task metrics.
pub const TASK_SUBSYSTEM: &str = "task";
/// The subsystem of the node metrics.
pub const CLUSTER_SUBSYSTEM: &str = "cluster";

/// The `container` label.
pub const LABEL_CONTAINER: &str = "container";
/// The `display_name` label.
pub const LABEL_DISPLAY_NAME: &str = "display_name";
/// The `result` label.
pub const LABEL_RESULT: &str = "result";
/// The `service` label.
pub const LABEL_SERVICE: &str = "service";
/// The `service_mode` label.
pub const LABEL_SERVICE_MODE: &str = "service_mode";
/// The `stack` label.
pub const LABEL_STACK: &str = "stack";
/// The `state` label.
pub const LABEL_STATE: &str = "state";

/// The `result` of a successful resync on `swarm_exporter_resyncs_total`.
pub const RESYNC_RESULT_SUCCESS: &str = "success";
/// The `result` of a failed resync.
pub const RESYNC_RESULT_FAILURE: &str = "failure";

/// The service modes, spelled as in the MODE column of `docker service ls`.
pub const SERVICE_MODE_REPLICATED: &str = "replicated";
/// `global`.
pub const SERVICE_MODE_GLOBAL: &str = "global";
/// `replicated-job`.
pub const SERVICE_MODE_REPLICATED_JOB: &str = "replicated-job";
/// `global-job`.
pub const SERVICE_MODE_GLOBAL_JOB: &str = "global-job";

/// The label names the per-service families already carry, which a custom label must not reuse:
/// a duplicate label name would make the family's descriptor invalid.
#[must_use]
pub fn reserved_label_names() -> [&'static str; 5] {
    [
        LABEL_STACK,
        LABEL_SERVICE,
        LABEL_SERVICE_MODE,
        LABEL_DISPLAY_NAME,
        LABEL_STATE,
    ]
}
