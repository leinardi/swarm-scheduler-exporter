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

//! Bring-up, teardown and the sweep: everything that creates or removes something on the host
//! daemon or inside the nodes. Cut down from runhold's harness to `DinD` only: no provider
//! interface, no Multipass, a single manager.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::models::{
    ContainerCreateBody, EndpointSettings, HostConfig, NetworkCreateRequest, NetworkingConfig,
    NodeState, PortBinding, SwarmInitRequest, SwarmJoinRequest,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, InspectContainerOptions, ListContainersOptions,
    ListNetworksOptions, ListNodesOptions, RemoveContainerOptions, StartContainerOptions,
};
use futures_util::StreamExt as _;
use jiff::Timestamp;
use swarm_scheduler_exporter::clock::{Clock as _, SystemClock};
use swarm_scheduler_exporter::go_compat::duration;
use swarm_scheduler_exporter::logger::Attr;
use tokio::time::{Instant, MissedTickBehavior};

use crate::Error;
use crate::cluster::{Cluster, DEFAULT_DIND_IMAGE, Node, NodeRole, Spec, labels};
use crate::context::{Context, Ended};
use crate::envid::{LABEL_ENV_ID, label_filter, resource_name};
use crate::images::load_workload_image;
use crate::redact::RedactingLogger;
use crate::sweep_rules::{
    Found, LABEL_OWNER, LABEL_PID_NAMESPACE, LABEL_STARTED, Owner, SweepRules, current_owner,
    stale_after_from_env,
};

const DIND_DAEMON_PORT: &str = "2375/tcp";
const SWARM_MANAGE_PORT: &str = "2377";
const DAEMON_READY_WAIT: Duration = Duration::from_secs(60);
const SWARM_READY_WAIT: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Bounds one ordinary Docker call, so a hung daemon fails that call instead of stalling the run
/// until the suite deadline.
pub(crate) const OP_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounds swarm init and join, which wait for Raft and TLS bootstrap.
const SWARM_OP_TIMEOUT: Duration = Duration::from_secs(90);
/// Bounds one image pull, save or load, which move whole images.
pub(crate) const IMAGE_TRANSFER_TIMEOUT: Duration = Duration::from_mins(5);
/// Bounds the removal of a failed bring-up.
const TEARDOWN_TIMEOUT: Duration = Duration::from_mins(2);

/// Brings up a fresh cluster for `spec`: a bridge network, one manager and `spec.workers` workers
/// as privileged `DinD` containers, the workload image loaded on every node, and a Swarm whose
/// nodes are all Ready. If it fails, everything it created is removed unless
/// `spec.keep_on_failure` is set.
///
/// # Errors
///
/// The step that failed, with the envid.
pub async fn up(ctx: &Context, mut spec: Spec) -> Result<Cluster, Error> {
    if spec.dind_image.is_empty() {
        DEFAULT_DIND_IMAGE.clone_into(&mut spec.dind_image);
    }

    let log = spec
        .logger
        .with_attrs(&[Attr::string("envid", spec.env_id.clone())]);

    // Logged before anything exists, so a run killed at any later point can still be cleaned up
    // by hand.
    log.info(
        "bringing up test swarm; manual cleanup",
        &[Attr::string("cmd", cleanup_command(&spec.env_id))],
    );

    let owner = current_owner()?;
    let rules = default_sweep_rules(&spec.env_id, &owner)?;
    let host_client = host_client()?;
    let started_at = SystemClock.now();

    sweep(ctx, &host_client, &rules, &log).await;

    match bring_up(ctx, &spec, &owner, started_at, &host_client, &log).await {
        Ok((nodes, workload_image_id)) => Ok(Cluster {
            manager: nodes[0].clone(),
            nodes,
            workload_image_id,
            started_at,
            host_client,
            owner,
            spec,
        }),
        Err(error) => {
            if spec.keep_on_failure {
                log.warn(
                    "bring-up failed; keeping the environment",
                    &[Attr::string("cmd", cleanup_command(&spec.env_id))],
                );
            } else {
                let teardown = ctx.without_cancel().with_timeout(TEARDOWN_TIMEOUT);
                if let Err(cleanup_error) = cleanup(&teardown, &host_client, &spec.env_id).await {
                    log.warn(
                        "bring-up failed and cleanup was incomplete",
                        &[Attr::string("err", cleanup_error.to_string())],
                    );
                }
            }
            Err(error.context(format!("bring up test swarm {}", spec.env_id)))
        }
    }
}

impl Cluster {
    /// Removes every container and network of the cluster. Pass a context that is not the one
    /// that may have expired during the tests: teardown must still run.
    ///
    /// # Errors
    ///
    /// Every removal that failed, joined.
    pub async fn down(&self, ctx: &Context) -> Result<(), Error> {
        cleanup(ctx, &self.host_client, &self.spec.env_id).await
    }

    /// Pulls `image_ref` on the host daemon unless it is already there.
    ///
    /// # Errors
    ///
    /// The inspect or the pull failed.
    pub async fn ensure_host_image(&self, ctx: &Context, image_ref: &str) -> Result<(), Error> {
        ensure_host_image(ctx, &self.host_client, image_ref, &self.spec.logger).await
    }

    /// Freezes the node's `DinD` container. Its daemon stops answering, so the manager marks the
    /// node down once heartbeats lapse, while the tasks it ran keep their last reported state.
    ///
    /// # Errors
    ///
    /// The pause failed.
    pub async fn pause_node(&self, ctx: &Context, node: &Node) -> Result<(), Error> {
        with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client.pause_container(&node.container_id),
        )
        .await
        .map_err(|error| Error::new(format!("pause node {:?}: {error}", node.name)))
    }

    /// Resumes a node frozen by [`Cluster::pause_node`]. Unpausing a node that is not paused is
    /// not an error, so cleanup can call it unconditionally.
    ///
    /// # Errors
    ///
    /// The inspect or the unpause failed.
    pub async fn unpause_node(&self, ctx: &Context, node: &Node) -> Result<(), Error> {
        let inspected = with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client
                .inspect_container(&node.container_id, None::<InspectContainerOptions>),
        )
        .await
        .map_err(|error| Error::new(format!("inspect node {:?}: {error}", node.name)))?;

        let paused = inspected
            .state
            .and_then(|state| state.paused)
            .unwrap_or(false);
        if !paused {
            return Ok(());
        }

        with_timeout(
            ctx,
            OP_TIMEOUT,
            self.host_client.unpause_container(&node.container_id),
        )
        .await
        .map_err(|error| Error::new(format!("unpause node {:?}: {error}", node.name)))
    }
}

impl Node {
    /// A client for the node's daemon. It is built from the node's address alone, never from the
    /// environment, so a `DOCKER_*` variable in the caller's shell (TLS, context) cannot redirect
    /// or break it. Its requests are unversioned, as the host client's are, so a node running an
    /// older dind needs no negotiation either.
    ///
    /// # Errors
    ///
    /// The address is not one bollard can dial.
    pub fn client(&self) -> Result<Docker, Error> {
        Docker::connect_with_http(
            &self.docker_host,
            IMAGE_TRANSFER_TIMEOUT.as_secs(),
            bollard::API_DEFAULT_VERSION,
        )
        .map_err(|error| {
            Error::new(format!(
                "create Docker client for node {:?}: {error}",
                self.name
            ))
        })
    }
}

/// A shell command that removes every container (with its volumes) and network of the run
/// `env_id` from the host.
#[must_use]
pub fn cleanup_command(env_id: &str) -> String {
    let filter = format!("label={}", label_filter(env_id));
    format!(
        "docker ps -aq --filter {filter} | xargs -r docker rm -fv; docker network ls -q --filter \
         {filter} | xargs -r docker network rm"
    )
}

/// The host daemon's client, from the environment by bollard's rules, which are narrower than
/// Go's `client.FromEnv`: `DOCKER_HOST` (the local socket by default), and TLS only when
/// `DOCKER_TLS_VERIFY` is set, with the certificates from `DOCKER_CERT_PATH` (or `~/.docker`).
/// Go also turns TLS on, unverified, when `DOCKER_CERT_PATH` alone is set, and honours a
/// `DOCKER_API_VERSION` pin; bollard does neither, and the harness needs neither.
///
/// There is no version negotiation, here or in [`Node::client`], and none is needed: bollard
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

// --- Bring-up ---

async fn bring_up(
    ctx: &Context,
    spec: &Spec,
    owner: &Owner,
    started_at: Timestamp,
    host_client: &Docker,
    log: &RedactingLogger,
) -> Result<(Vec<Node>, String), Error> {
    ensure_host_image(ctx, host_client, &spec.dind_image, log).await?;

    let labels = labels(&spec.env_id, owner, started_at);
    let network_name = resource_name(&spec.env_id, "net");
    create_bridge_network(ctx, host_client, &network_name, &labels).await?;

    let mut nodes = provision_nodes(ctx, host_client, spec, &network_name, &labels).await?;
    log.info(
        "DinD containers started",
        &[Attr::int("count", count(nodes.len()))],
    );

    wait_for_daemons(ctx, &nodes).await?;
    log.info("inner daemons ready", &[]);

    let workload_image_id = load_workload_image(ctx, host_client, &nodes, log).await?;

    let worker_token = init_swarm(ctx, &nodes[0]).await?;
    join_workers(ctx, &nodes[0].bridge_ip, &nodes[1..], &worker_token).await?;
    wait_for_swarm_ready(ctx, &nodes[0], nodes.len()).await?;
    record_swarm_node_ids(ctx, &mut nodes).await?;

    log.info(
        "all swarm nodes ready",
        &[Attr::int("nodes", count(nodes.len()))],
    );
    Ok((nodes, workload_image_id))
}

fn count(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

async fn create_bridge_network(
    ctx: &Context,
    host_client: &Docker,
    name: &str,
    labels: &HashMap<String, String>,
) -> Result<(), Error> {
    with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.create_network(NetworkCreateRequest {
            name: name.to_owned(),
            driver: Some("bridge".to_owned()),
            labels: Some(labels.clone()),
            ..NetworkCreateRequest::default()
        }),
    )
    .await
    .map(|_| ())
    .map_err(|error| Error::new(format!("create bridge network {name:?}: {error}")))
}

async fn provision_nodes(
    ctx: &Context,
    host_client: &Docker,
    spec: &Spec,
    network_name: &str,
    labels: &HashMap<String, String>,
) -> Result<Vec<Node>, Error> {
    let mut nodes = Vec::with_capacity(spec.workers + 1);
    nodes.push(
        start_node(
            ctx,
            host_client,
            spec,
            labels,
            network_name,
            "manager-0",
            NodeRole::Manager,
        )
        .await?,
    );
    for index in 0..spec.workers {
        nodes.push(
            start_node(
                ctx,
                host_client,
                spec,
                labels,
                network_name,
                &format!("worker-{index}"),
                NodeRole::Worker,
            )
            .await?,
        );
    }
    Ok(nodes)
}

async fn start_node(
    ctx: &Context,
    host_client: &Docker,
    spec: &Spec,
    labels: &HashMap<String, String>,
    network_name: &str,
    hostname: &str,
    role: NodeRole,
) -> Result<Node, Error> {
    let name = resource_name(&spec.env_id, hostname);

    let body = ContainerCreateBody {
        image: Some(spec.dind_image.clone()),
        hostname: Some(hostname.to_owned()),
        // An empty DOCKER_TLS_CERTDIR makes the entrypoint serve plain TCP on 2375. The explicit
        // --tls=false matters too: without it dockerd deliberately stalls its start-up for
        // seconds to warn about unauthenticated TCP.
        env: Some(vec!["DOCKER_TLS_CERTDIR=".to_owned()]),
        cmd: Some(vec!["--tls=false".to_owned()]),
        labels: Some(labels.clone()),
        exposed_ports: Some(vec![DIND_DAEMON_PORT.to_owned()]),
        host_config: Some(HostConfig {
            privileged: Some(true),
            port_bindings: Some(HashMap::from([(
                DIND_DAEMON_PORT.to_owned(),
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_owned()),
                    host_port: Some("0".to_owned()),
                }]),
            )])),
            ..HostConfig::default()
        }),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                network_name.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        ..ContainerCreateBody::default()
    };
    let created = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.create_container(
            Some(CreateContainerOptions {
                name: Some(name.clone()),
                ..CreateContainerOptions::default()
            }),
            body,
        ),
    )
    .await
    .map_err(|error| Error::new(format!("create node {name:?}: {error}")))?;

    with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.start_container(&created.id, None::<StartContainerOptions>),
    )
    .await
    .map_err(|error| Error::new(format!("start node {name:?}: {error}")))?;

    inspect_node(
        ctx,
        host_client,
        created.id,
        name,
        hostname,
        role,
        network_name,
    )
    .await
}

async fn inspect_node(
    ctx: &Context,
    host_client: &Docker,
    container_id: String,
    name: String,
    hostname: &str,
    role: NodeRole,
    network_name: &str,
) -> Result<Node, Error> {
    let inspected = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.inspect_container(&container_id, None::<InspectContainerOptions>),
    )
    .await
    .map_err(|error| Error::new(format!("inspect node {name:?}: {error}")))?;

    let not_attached = || {
        Error::new(format!(
            "node {name:?} on network {network_name:?}: network attachment missing"
        ))
    };
    let settings = inspected.network_settings.ok_or_else(not_attached)?;

    let bridge_ip = settings
        .networks
        .as_ref()
        .and_then(|networks| networks.get(network_name))
        .and_then(|endpoint| endpoint.ip_address.as_deref())
        .filter(|address| address.parse::<IpAddr>().is_ok())
        .ok_or_else(not_attached)?
        .to_owned();

    let host_port_text = settings
        .ports
        .as_ref()
        .and_then(|ports| ports.get(DIND_DAEMON_PORT))
        .and_then(Option::as_ref)
        .and_then(|bindings| bindings.first())
        .map(|binding| binding.host_port.clone().unwrap_or_default())
        .ok_or_else(|| {
            Error::new(format!(
                "node {name:?} port {DIND_DAEMON_PORT}: host port binding missing"
            ))
        })?;
    let host_port: u16 = host_port_text.parse().map_err(|error| {
        Error::new(format!(
            "node {name:?}: parse host port {host_port_text:?}: {error}"
        ))
    })?;

    Ok(Node {
        name,
        container_id,
        hostname: hostname.to_owned(),
        role,
        swarm_node_id: String::new(),
        host_port,
        docker_host: format!("tcp://127.0.0.1:{host_port}"),
        bridge_ip,
    })
}

/// The first readiness phase: every inner daemon answers a ping.
async fn wait_for_daemons(ctx: &Context, nodes: &[Node]) -> Result<(), Error> {
    for node in nodes {
        let client = node.client()?;
        poll_until(
            ctx,
            DAEMON_READY_WAIT,
            &format!("daemon on {} answers ping", node.name),
            async || {
                with_timeout(ctx, OP_TIMEOUT, client.ping())
                    .await
                    .map(|_| ())
                    .map_err(|error| Error::new(error.to_string()))
            },
        )
        .await?;
    }
    Ok(())
}

async fn init_swarm(ctx: &Context, manager: &Node) -> Result<String, Error> {
    let client = manager.client()?;
    with_timeout(
        ctx,
        SWARM_OP_TIMEOUT,
        client.init_swarm(SwarmInitRequest {
            listen_addr: Some(format!("0.0.0.0:{SWARM_MANAGE_PORT}")),
            advertise_addr: Some(manager.bridge_ip.clone()),
            ..SwarmInitRequest::default()
        }),
    )
    .await
    .map_err(|error| Error::new(format!("swarm init on {:?}: {error}", manager.name)))?;

    let inspected = with_timeout(ctx, OP_TIMEOUT, client.inspect_swarm())
        .await
        .map_err(|error| Error::new(format!("swarm inspect on {:?}: {error}", manager.name)))?;
    Ok(inspected
        .join_tokens
        .and_then(|tokens| tokens.worker)
        .unwrap_or_default())
}

async fn join_workers(
    ctx: &Context,
    manager_bridge_ip: &str,
    workers: &[Node],
    worker_token: &str,
) -> Result<(), Error> {
    let remote_addr = format!("{manager_bridge_ip}:{SWARM_MANAGE_PORT}");
    for worker in workers {
        let client = worker.client()?;
        with_timeout(
            ctx,
            SWARM_OP_TIMEOUT,
            client.join_swarm(SwarmJoinRequest {
                listen_addr: Some(format!("0.0.0.0:{SWARM_MANAGE_PORT}")),
                advertise_addr: Some(worker.bridge_ip.clone()),
                remote_addrs: Some(vec![remote_addr.clone()]),
                join_token: Some(worker_token.to_owned()),
                ..SwarmJoinRequest::default()
            }),
        )
        .await
        .map_err(|error| Error::new(format!("swarm join on {:?}: {error}", worker.name)))?;
    }
    Ok(())
}

/// The second readiness phase: the manager lists every node as Ready.
async fn wait_for_swarm_ready(
    ctx: &Context,
    manager: &Node,
    total_nodes: usize,
) -> Result<(), Error> {
    let client = manager.client()?;
    poll_until(
        ctx,
        SWARM_READY_WAIT,
        &format!("{total_nodes} swarm nodes Ready"),
        async || {
            let listed = with_timeout(ctx, OP_TIMEOUT, client.list_nodes(None::<ListNodesOptions>))
                .await
                .map_err(|error| Error::new(format!("node list: {error}")))?;
            let ready = listed
                .iter()
                .filter(|node| {
                    node.status.as_ref().and_then(|status| status.state) == Some(NodeState::READY)
                })
                .count();
            if ready == total_nodes {
                return Ok(());
            }
            Err(Error::new(format!(
                "{ready} of {total_nodes} nodes Ready: not ready in time"
            )))
        },
    )
    .await
}

async fn record_swarm_node_ids(ctx: &Context, nodes: &mut [Node]) -> Result<(), Error> {
    for node in nodes {
        let client = node.client()?;
        let info = with_timeout(ctx, OP_TIMEOUT, client.info())
            .await
            .map_err(|error| Error::new(format!("info on {:?}: {error}", node.name)))?;
        node.swarm_node_id = info
            .swarm
            .and_then(|swarm| swarm.node_id)
            .unwrap_or_default();
    }
    Ok(())
}

/// Makes `image_ref` available on the host daemon, pulling it only when it is missing.
pub(crate) async fn ensure_host_image(
    ctx: &Context,
    host_client: &Docker,
    image_ref: &str,
    log: &RedactingLogger,
) -> Result<(), Error> {
    match with_timeout(ctx, OP_TIMEOUT, host_client.inspect_image(image_ref)).await {
        Ok(_) => return Ok(()),
        Err(error) if !error.is_not_found() => {
            return Err(Error::new(format!(
                "inspect host image {image_ref:?}: {error}"
            )));
        }
        Err(_) => {}
    }

    log.info("pulling image on host", &[Attr::string("image", image_ref)]);

    // The whole reference goes in fromImage: the daemon splits off the tag or digest itself.
    let pull = async {
        let mut progress = host_client.create_image(
            Some(CreateImageOptions {
                from_image: Some(image_ref.to_owned()),
                ..CreateImageOptions::default()
            }),
            None,
            None,
        );
        while let Some(message) = progress.next().await {
            message?;
        }
        Ok::<(), DockerError>(())
    };
    with_timeout(ctx, IMAGE_TRANSFER_TIMEOUT, pull)
        .await
        .map_err(|error| Error::new(format!("pull host image {image_ref:?}: {error}")))
}

// --- Teardown ---

/// The sweep rules for this process: now, this host, the configured window and a PID check on
/// this host.
fn default_sweep_rules(self_id: &str, owner: &Owner) -> Result<SweepRules, Error> {
    Ok(SweepRules {
        now: SystemClock.now(),
        stale_after: stale_after_from_env(|key| std::env::var(key).ok())?,
        host: owner.host.clone(),
        pid_namespace: owner.pid_namespace.clone(),
        self_id: self_id.to_owned(),
        alive: Box::new(process_alive),
    })
}

/// Whether `pid` exists on this host: signal 0 checks without delivering, and a permission error
/// still means the process is there. A PID outside `pid_t` names no process: Linux keeps PIDs
/// under 2^22.
fn process_alive(pid: i64) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(errno) => errno == rustix::io::Errno::PERM,
    }
}

/// Removes the environments of earlier runs that `rules` finds abandoned: leaks from runs that
/// were killed before their teardown ran. An environment that may still belong to a running test,
/// on this host or another, is left alone, and the reason is logged.
async fn sweep(ctx: &Context, host_client: &Docker, rules: &SweepRules, log: &RedactingLogger) {
    for env in list_environments(ctx, host_client, log).await {
        let verdict = rules.decide(env);
        if !verdict.remove {
            if verdict.env.id != rules.self_id {
                log.debug(
                    "sweep: keeping environment",
                    &[
                        Attr::string("other_envid", verdict.env.id.clone()),
                        Attr::string("reason", verdict.reason.clone()),
                    ],
                );
            }
            continue;
        }

        log.info(
            "sweep: removing leaked environment",
            &[
                Attr::string("leaked_envid", verdict.env.id.clone()),
                Attr::string("reason", verdict.reason.clone()),
            ],
        );

        if let Err(error) = cleanup(ctx, host_client, &verdict.env.id).await {
            log.warn(
                "sweep: cleanup incomplete",
                &[
                    Attr::string("leaked_envid", verdict.env.id.clone()),
                    Attr::string("err", error.to_string()),
                ],
            );
        }
    }
}

/// Adds one resource's labels to the environment they name.
fn record(by_id: &mut BTreeMap<String, Found>, labels: Option<&HashMap<String, String>>) {
    let Some(labels) = labels else {
        return;
    };
    let Some(env_id) = labels.get(LABEL_ENV_ID).filter(|id| !id.is_empty()) else {
        return;
    };
    let found = by_id.entry(env_id.clone()).or_insert_with(|| Found {
        id: env_id.clone(),
        ..Found::default()
    });
    // A helper container may carry the envid alone; the owner and start time come from
    // whichever resource has them.
    for (field, key) in [
        (&mut found.owner, LABEL_OWNER),
        (&mut found.started, LABEL_STARTED),
        (&mut found.pid_namespace, LABEL_PID_NAMESPACE),
    ] {
        if field.is_empty()
            && let Some(value) = labels.get(key)
        {
            value.clone_into(field);
        }
    }
}

/// Every environment with a container or network on the host daemon, from their labels. A
/// listing that fails is logged and leaves that kind of resource out: the sweep is best-effort,
/// and `down` still removes the run's own resources by envid.
async fn list_environments(
    ctx: &Context,
    host_client: &Docker,
    log: &RedactingLogger,
) -> Vec<Found> {
    let filters = HashMap::from([("label".to_owned(), vec![LABEL_ENV_ID.to_owned()])]);
    let mut by_id = BTreeMap::new();

    match with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters.clone()),
            ..ListContainersOptions::default()
        })),
    )
    .await
    {
        Ok(containers) => {
            for container in &containers {
                record(&mut by_id, container.labels.as_ref());
            }
        }
        Err(error) => log.warn(
            "sweep: list containers",
            &[Attr::string("err", error.to_string())],
        ),
    }

    match with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.list_networks(Some(ListNetworksOptions {
            filters: Some(filters),
        })),
    )
    .await
    {
        Ok(networks) => {
            for network in &networks {
                record(&mut by_id, network.labels.as_ref());
            }
        }
        Err(error) => log.warn(
            "sweep: list networks",
            &[Attr::string("err", error.to_string())],
        ),
    }

    by_id.into_values().collect()
}

/// Removes every container (with its anonymous /var/lib/docker volume) and every network labeled
/// with `env_id`. It keeps going past individual failures and returns them joined.
async fn cleanup(ctx: &Context, host_client: &Docker, env_id: &str) -> Result<(), Error> {
    let filters = HashMap::from([("label".to_owned(), vec![label_filter(env_id)])]);
    let mut errors = Vec::new();

    let containers = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters.clone()),
            ..ListContainersOptions::default()
        })),
    )
    .await
    .unwrap_or_else(|error| {
        errors.push(Error::new(format!("list containers: {error}")));
        Vec::new()
    });
    for container in containers {
        let id = container.id.unwrap_or_default();
        let removed = with_timeout(
            ctx,
            OP_TIMEOUT,
            host_client.remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..RemoveContainerOptions::default()
                }),
            ),
        )
        .await;
        if let Err(error) = removed
            && !error.is_not_found()
        {
            errors.push(Error::new(format!("remove container {id}: {error}")));
        }
    }

    let networks = with_timeout(
        ctx,
        OP_TIMEOUT,
        host_client.list_networks(Some(ListNetworksOptions {
            filters: Some(filters),
        })),
    )
    .await
    .unwrap_or_else(|error| {
        errors.push(Error::new(format!("list networks: {error}")));
        Vec::new()
    });
    for network in networks {
        let id = network.id.unwrap_or_default();
        let removed = with_timeout(ctx, OP_TIMEOUT, host_client.remove_network(&id)).await;
        if let Err(error) = removed
            && !error.is_not_found()
        {
            errors.push(Error::new(format!("remove network {id}: {error}")));
        }
    }

    Error::join(errors).map_or(Ok(()), Err)
}

// --- Helpers ---

/// A Docker call that failed, or the context it ran under ended first.
#[derive(Debug)]
pub(crate) enum OpError {
    Docker(DockerError),
    Ended(Ended),
}

impl OpError {
    /// containerd's `errdefs.IsNotFound`: the daemon answered 404.
    fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::Docker(DockerError::DockerResponseServerError {
                status_code: 404,
                ..
            })
        )
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Docker(error) => error.fmt(formatter),
            Self::Ended(ended) => ended.fmt(formatter),
        }
    }
}

impl From<DockerError> for OpError {
    fn from(error: DockerError) -> Self {
        Self::Docker(error)
    }
}

/// Runs one Docker call under its own deadline derived from `ctx`.
pub(crate) async fn with_timeout<T, E: Into<OpError>>(
    ctx: &Context,
    timeout: Duration,
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, OpError> {
    ctx.with_timeout(timeout)
        .run(call)
        .await
        .map_err(OpError::Ended)?
        .map_err(Into::into)
}

/// Calls `check` every [`POLL_INTERVAL`] until it succeeds, `timeout` elapses, or `ctx` ends.
/// The error names what was awaited and carries the check's last error.
async fn poll_until(
    ctx: &Context,
    timeout: Duration,
    what: &str,
    mut check: impl AsyncFnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // The first tick is immediate; Go's ticker first fires after one interval.
    ticker.tick().await;

    loop {
        let Err(last) = check().await else {
            return Ok(());
        };
        if Instant::now() > deadline {
            return Err(Error::new(format!(
                "{what} within {}: not ready in time: {last}",
                duration::format(i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX))
            )));
        }
        if let Err(ended) = ctx.run(ticker.tick()).await {
            return Err(Error::new(format!("{what}: {ended}: {last}")));
        }
    }
}
