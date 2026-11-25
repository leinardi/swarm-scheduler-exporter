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

//! Bring-up and the sweep, on testenv-core's `DinD` Swarm (`testenv_core::dind`): this crate passes
//! it the suite's names and keep label, the host's client and sweep rules, and the one setup
//! step of its own, the workload image load, which runs inside the bring-up's cleanup boundary.

use clock::{Clock as _, SystemClock};
use testenv_core::bollard::Docker;
use testenv_core::context::Context;
use testenv_core::dind::{self, IMAGE_TRANSFER_TIMEOUT, Names, OnFailure};
use testenv_core::host;
use testenv_core::sweep_rules::{
    DEFAULT_STALE_AFTER, Owner, SweepRules, current_owner, stale_after_from_env,
};

use crate::Error;
use crate::cluster::{Cluster, DEFAULT_DIND_IMAGE, Spec};
use crate::config::{ENV_IDS, ENV_STALE_AFTER, KEEP_LABEL, LABELS, RESOURCES};
use crate::images::load_workload_image;

/// The suite's names: its envid, labels, keep label and what a run leaves on the host.
pub const NAMES: Names = Names {
    env_ids: ENV_IDS,
    labels: LABELS,
    keep_label: KEEP_LABEL,
    resources: RESOURCES,
};

/// Brings up a fresh cluster for `spec`: a bridge network, one manager and `spec.workers` workers
/// as privileged `DinD` containers, a Swarm whose nodes are all Ready, and the workload image
/// loaded on every node. Runs abandoned by earlier test processes are swept first, except those a
/// failed run kept. If any step fails, the image load included, everything created is removed
/// unless `spec.keep_on_failure` is set: then it stays, labelled with [`KEEP_LABEL`], so a later
/// run's sweep leaves it alone.
///
/// # Errors
///
/// The step that failed, with the envid.
pub async fn up(ctx: &Context, spec: Spec) -> Result<Cluster, Error> {
    let (swarm, workload_image_id) = up_with(ctx, spec, async |ctx, cluster| {
        load_workload_image(
            ctx,
            cluster.host_client(),
            &cluster.nodes,
            &cluster.spec.logger,
        )
        .await
    })
    .await?;
    Ok(Cluster {
        swarm,
        workload_image_id,
    })
}

/// [`up`] with `setup` in the place of the workload image load: testenv-core's bring-up for
/// `spec` on the host daemon, then `setup` on the Ready Swarm, inside the bring-up's cleanup
/// boundary. The integration suite's bring-up tests inject a failing or a hanging step here.
///
/// # Errors
///
/// The step that failed, `setup` included, with the envid.
pub async fn up_with<T>(
    ctx: &Context,
    spec: Spec,
    setup: impl AsyncFnOnce(&Context, &dind::Cluster) -> Result<T, Error>,
) -> Result<(dind::Cluster, T), Error> {
    let dind_image = if spec.dind_image.is_empty() {
        DEFAULT_DIND_IMAGE.to_owned()
    } else {
        spec.dind_image
    };
    let owner = current_owner()?;
    let host = dind::Host {
        rules: default_sweep_rules(&spec.env_id, &owner)?,
        client: host_client()?,
        owner,
    };
    let shared = dind::Spec {
        env_id: spec.env_id,
        dind_image,
        managers: 1,
        workers: spec.workers,
        keep: if spec.keep_on_failure {
            OnFailure::Keep
        } else {
            OnFailure::Remove
        },
        names: NAMES,
        logger: spec.logger,
    };
    dind::up(ctx, shared, host, setup).await
}

/// The host daemon's client, from the environment by bollard's rules, which are narrower than
/// Go's `client.FromEnv`: `DOCKER_HOST` (the local socket by default), and TLS only when
/// `DOCKER_TLS_VERIFY` is set, with the certificates from `DOCKER_CERT_PATH` (or `~/.docker`).
/// Go also turns TLS on, unverified, when `DOCKER_CERT_PATH` alone is set, and honours a
/// `DOCKER_API_VERSION` pin; bollard does neither, and the harness needs neither.
///
/// There is no version negotiation, here or in the nodes' clients, and none is needed: bollard
/// builds each URL as `/v<version><path>` and then joins the absolute path onto it, which drops
/// the version prefix (bollard 0.21.1, `uri.rs`), so every request goes out unversioned and the
/// daemon serves it at its own API version. An older daemon therefore never sees a client
/// version that is too new, and `negotiate_version` would change nothing on the wire (S1: the
/// socket proxy's access log shows `GET /services`, not `/v1.53/services`).
///
/// Its own timeout is set to the longest of the harness's, so the harness's deadlines are the
/// ones that apply.
fn host_client() -> Result<Docker, Error> {
    Docker::connect_with_defaults()
        .map(|client| client.with_timeout(IMAGE_TRANSFER_TIMEOUT))
        .map_err(|error| Error::new(format!("connect to host Docker daemon: {error}")))
}

/// The sweep rules for this process: now, this host, the configured window
/// ([`ENV_STALE_AFTER`], an hour by default) and a PID check on this host.
fn default_sweep_rules(self_id: &str, owner: &Owner) -> Result<SweepRules, Error> {
    let now = SystemClock.now();
    let stale_after = stale_after_from_env(ENV_STALE_AFTER, DEFAULT_STALE_AFTER, |key| {
        std::env::var(key).ok()
    })?;
    Ok(host::sweep_rules(now, stale_after, self_id, owner))
}
