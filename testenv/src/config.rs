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

//! What this suite passes testenv-core: the envid's length, label and resource-name prefix, the
//! labels that record a run's owner, what a run leaves on the host daemon, and the variable that
//! sets the sweep's stale-after window. Free of the Docker SDK and of the `integration` feature,
//! so the test that pins them runs in a plain `cargo test`.

use testenv_core::envid::EnvIds;
use testenv_core::sweep_rules::{Labels, Resources};

/// Four random bytes (eight hex characters) under `swarm-scheduler-exporter.it.envid`, and
/// resources named `sse-it-<envid>-<base>`. Use [`EnvIds::label_filter`] to scope Docker API
/// queries to one run, or [`EnvIds::label`] on its own to find every run.
pub const ENV_IDS: EnvIds = EnvIds {
    bytes: 4,
    label: "swarm-scheduler-exporter.it.envid",
    prefix: "sse-it",
};

/// The envid label, then `<hostname>:<pid>` of the test process that created the resource, when
/// the environment came up (RFC 3339), and the owner's PID namespace (`pid:[4026531836]`).
pub const LABELS: Labels = Labels {
    env_id: ENV_IDS.label,
    owner: "swarm-scheduler-exporter.it.owner",
    started: "swarm-scheduler-exporter.it.started",
    pid_namespace: "swarm-scheduler-exporter.it.pidns",
};

/// A run leaves containers, with their anonymous `/var/lib/docker` volumes, and networks.
pub const RESOURCES: Resources = Resources {
    remove_volumes: true,
    networks: true,
    images: false,
};

/// Overrides how old an abandoned environment must be before the sweep removes it (an hour,
/// testenv-core's `DEFAULT_STALE_AFTER`).
pub const ENV_STALE_AFTER: &str = "SSE_IT_STALE_AFTER";

/// A shell command that removes every container (with its volumes) and network of the run
/// `env_id` from the host.
#[must_use]
pub fn cleanup_command(env_id: &str) -> String {
    RESOURCES.cleanup_command(&ENV_IDS.cli_filter(env_id))
}

#[cfg(test)]
mod tests {
    use super::{ENV_IDS, LABELS, cleanup_command};

    /// The names, labels and cleanup command a run had before testenv-core, which a leaked
    /// environment of an older run is still found and removed by.
    #[test]
    fn the_suite_s_names_and_labels_are_unchanged() {
        let id = ENV_IDS.generate().unwrap();
        assert_eq!(id.len(), 8, "{id}");
        assert_eq!(
            ENV_IDS.resource_name("1a2b3c4d", "manager-0"),
            "sse-it-1a2b3c4d-manager-0"
        );
        assert_eq!(
            ENV_IDS.label_filter("1a2b3c4d"),
            "swarm-scheduler-exporter.it.envid=1a2b3c4d"
        );
        assert_eq!(
            [
                LABELS.env_id,
                LABELS.owner,
                LABELS.started,
                LABELS.pid_namespace
            ],
            [
                "swarm-scheduler-exporter.it.envid",
                "swarm-scheduler-exporter.it.owner",
                "swarm-scheduler-exporter.it.started",
                "swarm-scheduler-exporter.it.pidns",
            ]
        );
        assert_eq!(
            cleanup_command("1a2b3c4d"),
            "docker ps -aq --filter label=swarm-scheduler-exporter.it.envid=1a2b3c4d | xargs -r \
             docker rm -fv; docker network ls -q --filter \
             label=swarm-scheduler-exporter.it.envid=1a2b3c4d | xargs -r docker network rm"
        );
    }
}
