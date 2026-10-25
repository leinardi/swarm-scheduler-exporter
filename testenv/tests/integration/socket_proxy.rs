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

//! The example socket-proxy allowlist, enforced by the real, pinned proxy image.
//!
//! With docker-socket-proxy v0.5.0 (`HAProxy` 3.4.2) an idle event stream is never closed
//! cleanly: a server timeout, a proxy restart and a daemon SIGTERM all cut the body without a
//! terminating chunk, which the exporter logs at warn. Nothing here expects the info line of a
//! clean close; the restart below takes the warn path, as the Go test's does.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::LazyLock;
use std::time::Duration;

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, LogsOptions, RemoveContainerOptions, RestartContainerOptions,
    StartContainerOptions,
};
use futures_util::StreamExt as _;
use hyper::Method;
use regex::Regex;
use serde_json::Value;
use testenv::context::Context;
use testenv::envid::resource_name;

use crate::http::{get, request, scrape_metrics};
use crate::launcher::{Launch, free_port, local_addr};
use crate::swarm::{ServiceKey, ServiceOpts, deploy_service, op, teardown_ctx, update_service};
use crate::testing::{Fatal, OrFatal as _, TestHandle, TestResult, cluster};
use crate::wait::{
    METRIC_DESIRED, METRIC_EVENTS_RECONNECTS, METRIC_HEALTH, eventually, metrics_match, no_labels,
    want, want_service,
};

/// The example this test pins, relative to the repository root.
const COMPOSE_FILE: &str = "deployments/docker/docker-compose.socket-proxy.yaml";
const ENV_FILE: &str = "deployments/docker/socket-proxy.env";

/// The proxy's image line in the compose file.
static IMAGE_LINE: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?m)^\s+image:\s*(ghcr\.io/tecnativa/docker-socket-proxy:\S+)"));
/// The shape of a variable name in socket-proxy.env.
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| pattern(r"^[A-Z][A-Z0-9_]*$"));

/// The variables of the proxy image's environment that configure `HAProxy` rather than open an API
/// section. `HAPROXY_*` (the base image's build metadata) is skipped too; everything else in the
/// image's environment is an ACL variable.
const SETTINGS: &[&str] = &[
    "BIND_CONFIG",
    "DISABLE_IPV6",
    "LOG_LEVEL",
    "PATH",
    "SOCKET_PATH",
];

fn pattern(text: &str) -> Regex {
    Regex::new(text).unwrap_or_else(|error| panic!("invalid pattern {text}: {error}"))
}

fn repo_file(relative: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// Go: `TestSocketProxy_ExampleAllowlist`. Runs the real, pinned tecnativa/docker-socket-proxy
/// image with the example's socket-proxy.env in front of the test Swarm's manager: the exporter
/// must work through it, what the allowlist leaves off must be refused, and the breadth the
/// README states for `CONTAINERS=1` must hold. The in-process fault proxy of the other tests
/// forwards everything, so it says nothing about this ACL.
pub async fn example_allowlist(test: TestHandle) -> TestResult {
    let image = socket_proxy_image(&test)?;
    let allowlist = read_socket_proxy_env(&test)?;
    cluster()
        .ensure_host_image(test.ctx(), &image)
        .await
        .or_fatal(&test)?;
    assert_allowlist_complete(&test, &image, &allowlist).await?;
    let proxy = SocketProxy::start(&test, &image, &allowlist).await?;

    test.run("denied", async |test| {
        denied(&test, &proxy, &allowlist).await
    })
    .await;
    test.run("scope", async |test| scope(&test, &proxy).await)
        .await;
    test.run("exporter", async |test| {
        Box::pin(exporter(&test, &proxy)).await
    })
    .await;
    Ok(())
}

/// A write is refused whatever the allowlist says: the exporter is read-only, so `POST=1` is
/// never a deliberate change to this example. A read is refused unless its section is 1 in
/// socket-proxy.env, so enabling one on purpose changes the expectation there, in one place.
async fn denied(
    test: &TestHandle,
    proxy: &SocketProxy,
    allowlist: &BTreeMap<String, String>,
) -> TestResult {
    // (method, path, the variable that opens a read; empty for a write)
    let requests = [
        (Method::POST, "/services/create", ""),
        (Method::DELETE, "/containers/x", ""),
        (Method::GET, "/secrets", "SECRETS"),
        (Method::GET, "/volumes", "VOLUMES"),
        (Method::GET, "/images/json", "IMAGES"),
        (Method::GET, "/configs", "CONFIGS"),
        (Method::GET, "/swarm", "SWARM"),
        // The image enables VERSION by default: these catch that default leaking through.
        (Method::GET, "/version", "VERSION"),
        (Method::GET, "/info", "INFO"),
    ];
    for (method, path, section) in requests {
        let (status, _) = proxy.request(test, method.clone(), path).await?;
        if section.is_empty() {
            if status != 403 {
                test.error(format!(
                    "{method} {path} = {status}, want 403: the exporter never writes, so POST \
                     must stay 0"
                ));
            }
        } else if allowlist.get(section).map(String::as_str) != Some("1") {
            if status != 403 {
                test.error(format!(
                    "{method} {path} = {status}, want 403: {section} is not 1 in socket-proxy.env"
                ));
            }
        } else if status == 403 {
            test.error(format!(
                "{method} {path} = 403, want it forwarded: {section}=1 in socket-proxy.env"
            ));
        }
    }
    Ok(())
}

/// The breadth the README states: every enabled section forwards every GET under its prefix,
/// logs included. The exporter runs as a host process, so there is no exporter container to read
/// from: IDs that do not exist show whether the proxy forwards the request (a 404 from the
/// daemon) or refuses it (its own 403).
async fn scope(test: &TestHandle, proxy: &SocketProxy) -> TestResult {
    for path in [
        // stdout=1: without a stream the daemon answers 400 before looking the object up.
        "/services/does-not-exist/logs?stdout=1",
        "/tasks/does-not-exist/logs?stdout=1",
        "/containers/does-not-exist/logs?stdout=1",
        "/containers/does-not-exist/archive?path=/",
    ] {
        let (status, body) = proxy.request(test, Method::GET, path).await?;
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        if status != 404 || message.is_empty() {
            test.error(format!(
                "GET {path} = {status} {:?}, want the daemon's 404 with a JSON message: the \
                 proxy forwards every GET under an enabled section, as the README states",
                String::from_utf8_lossy(&body)
            ));
        }
    }
    let (status, body) = proxy.request(test, Method::GET, "/containers/json").await?;
    if status != 200 {
        test.error(format!(
            "GET /containers/json = {status} {:?}, want 200",
            String::from_utf8_lossy(&body)
        ));
    }
    Ok(())
}

/// The exporter works through the proxy, and recovers from the proxy's restart.
async fn exporter(test: &TestHandle, proxy: &SocketProxy) -> TestResult {
    const STACK: &str = "it-socket-proxy";
    let web = ServiceKey::new(STACK, "web");
    let service_id = deploy_service(
        test,
        STACK,
        web.service,
        &ServiceOpts {
            replicas: 1,
            ..ServiceOpts::default()
        },
    )
    .await?;
    let base_url = Launch::seeded(std::slice::from_ref(&web), &["--containers"])
        .via(&format!("tcp://{}", proxy.address))
        .start(test)
        .await?;

    let scraped = scrape_metrics(test.ctx(), &base_url).await.or_fatal(test)?;
    let reconnects_before = scraped.sum(METRIC_EVENTS_RECONNECTS, &no_labels());

    // A restart drops the event stream, as HAProxy's idle timeout would.
    proxy.restart(test).await?;

    eventually(test, Duration::from_secs(60), async |ctx| {
        let scraped = scrape_metrics(ctx, &base_url).await?;
        let reconnects = scraped.sum(METRIC_EVENTS_RECONNECTS, &no_labels());
        if reconnects <= reconnects_before {
            return Err(format!(
                "{METRIC_EVENTS_RECONNECTS} = {reconnects}, want above {reconnects_before}: not \
                 yet"
            ));
        }
        Ok(())
    })
    .await?;

    let healthy = [want(METRIC_HEALTH, no_labels(), 1.0)];
    eventually(test, Duration::from_secs(60), async |ctx| {
        metrics_match(ctx, &base_url, "", &healthy).await
    })
    .await?;

    // The reconnected stream carries events again: a scale-up reaches desired_replicas.
    Box::pin(update_service(test, &service_id, |spec| {
        if let Some(replicated) = spec.mode.as_mut().and_then(|mode| mode.replicated.as_mut()) {
            replicated.replicas = Some(2);
        }
    }))
    .await?;
    let scaled = [want_service(METRIC_DESIRED, &web, 2.0)];
    eventually(test, Duration::from_secs(30), async |ctx| {
        metrics_match(ctx, &base_url, STACK, &scaled).await
    })
    .await
}

/// The proxy image the example compose file pins.
fn socket_proxy_image(test: &TestHandle) -> Result<String, Fatal> {
    let compose = std::fs::read_to_string(repo_file(COMPOSE_FILE))
        .map_err(|error| test.fatal(format!("read {COMPOSE_FILE}: {error}")))?;
    IMAGE_LINE
        .captures(&compose)
        .and_then(|captures| captures.get(1))
        .map(|image| image.as_str().to_owned())
        .ok_or_else(|| {
            test.fatal(format!(
                "{COMPOSE_FILE} has no docker-socket-proxy image line"
            ))
        })
}

/// socket-proxy.env: `KEY=VALUE` lines, blank lines and `#` comments, each value 0 or 1, no key
/// twice. Sorted by name, as Go's `slices.Sorted(maps.Keys(...))` passed them to the container.
fn read_socket_proxy_env(test: &TestHandle) -> Result<BTreeMap<String, String>, Fatal> {
    let content = std::fs::read_to_string(repo_file(ENV_FILE))
        .map_err(|error| test.fatal(format!("open {ENV_FILE}: {error}")))?;
    let mut allowlist = BTreeMap::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let parsed = line
            .split_once('=')
            .filter(|(name, value)| ENV_NAME.is_match(name) && (*value == "0" || *value == "1"));
        let Some((name, value)) = parsed else {
            return Err(test.fatal(format!(
                "{ENV_FILE}:{number}: want NAME=0 or NAME=1, have {line:?}"
            )));
        };
        if allowlist
            .insert(name.to_owned(), value.to_owned())
            .is_some()
        {
            return Err(test.fatal(format!("{ENV_FILE}:{number}: {name} set twice")));
        }
    }
    Ok(allowlist)
}

/// Fails the test if the pinned image defines an ACL variable that socket-proxy.env does not
/// set: a new default-on section would otherwise widen access silently.
async fn assert_allowlist_complete(
    test: &TestHandle,
    image: &str,
    allowlist: &BTreeMap<String, String>,
) -> TestResult {
    let inspected = op(test.ctx(), cluster().host_client().inspect_image(image))
        .await
        .map_err(|error| test.fatal(format!("inspect {image}: {error}")))?;
    let Some(config) = inspected.config else {
        return Err(test.fatal(format!("inspect {image}: no image config")));
    };
    for entry in config.env.unwrap_or_default() {
        let name = entry
            .split_once('=')
            .map_or(entry.as_str(), |(name, _)| name);
        if SETTINGS.contains(&name) || name.starts_with("HAPROXY_") {
            continue;
        }
        if !allowlist.contains_key(name) {
            test.error(format!(
                "{ENV_FILE} does not set {name}, which the image defaults to {entry:?}: set it \
                 explicitly"
            ));
        }
    }
    Ok(())
}

/// The proxy container this test runs on the host daemon.
struct SocketProxy {
    container_id: String,
    /// The host:port the proxy listens on, on the host's loopback.
    address: String,
}

impl SocketProxy {
    /// Runs `image` on the host daemon with the allowlist, in front of the test Swarm's manager,
    /// and waits until it answers `/_ping`. The container is removed when the test ends, and its
    /// output logged if the test failed.
    async fn start(
        test: &TestHandle,
        image: &str,
        allowlist: &BTreeMap<String, String>,
    ) -> Result<Self, Fatal> {
        let ctx = test.ctx();
        let host = cluster().host_client();
        let port = free_port().or_fatal(test)?;
        let address = local_addr(port).to_string();

        let mut env: Vec<String> = allowlist
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        // Host networking reaches the manager's published daemon port on loopback. HAProxy's
        // server line takes host:port as well as a socket path.
        env.push(format!(
            "SOCKET_PATH={}",
            cluster().manager.docker_host.trim_start_matches("tcp://")
        ));
        env.push(format!("BIND_CONFIG={address}"));

        let body = ContainerCreateBody {
            image: Some(image.to_owned()),
            env: Some(env),
            labels: Some(cluster().labels()),
            host_config: Some(HostConfig {
                network_mode: Some("host".to_owned()),
                cap_drop: Some(vec!["ALL".to_owned()]),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        };
        let options = CreateContainerOptions {
            name: Some(resource_name(&cluster().spec.env_id, "socket-proxy")),
            ..CreateContainerOptions::default()
        };
        let created = op(ctx, host.create_container(Some(options), body))
            .await
            .map_err(|error| test.fatal(format!("create socket proxy: {error}")))?;
        let proxy = Self {
            container_id: created.id,
            address,
        };

        let cleanup_t = test.clone();
        let container_id = proxy.container_id.clone();
        test.cleanup(move || async move {
            let ctx = teardown_ctx();
            if cleanup_t.failed() {
                cleanup_t.log(format!(
                    "socket proxy output:\n{}",
                    container_output(&ctx, &container_id).await
                ));
            }
            let removed = op(
                &ctx,
                cluster().host_client().remove_container(
                    &container_id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..RemoveContainerOptions::default()
                    }),
                ),
            )
            .await;
            if let Err(error) = removed {
                cleanup_t.error(format!("remove socket proxy: {error}"));
            }
        });

        op(
            ctx,
            host.start_container(&proxy.container_id, None::<StartContainerOptions>),
        )
        .await
        .map_err(|error| test.fatal(format!("start socket proxy: {error}")))?;
        proxy.wait_ping(test).await?;
        Ok(proxy)
    }

    /// Waits until the proxy answers `GET /_ping` with 200, which also proves it reaches the
    /// manager.
    async fn wait_ping(&self, test: &TestHandle) -> TestResult {
        let url = format!("http://{}/_ping", self.address);
        eventually(test, Duration::from_secs(30), async |ctx| {
            match get(ctx, &url).await {
                Ok((200, _)) => Ok(()),
                Ok((status, _)) => Err(format!("GET /_ping = {status}: not yet")),
                Err(error) => Err(format!("{error}: not yet")),
            }
        })
        .await
    }

    /// Sends `method path` straight to the proxy: the status and the body.
    async fn request(
        &self,
        test: &TestHandle,
        method: Method,
        path: &str,
    ) -> Result<(u16, Vec<u8>), Fatal> {
        request(
            test.ctx(),
            method,
            &format!("http://{}{path}", self.address),
        )
        .await
        .or_fatal(test)
    }

    /// Restarts the proxy container and waits until it answers again.
    async fn restart(&self, test: &TestHandle) -> TestResult {
        op(
            test.ctx(),
            cluster()
                .host_client()
                .restart_container(&self.container_id, None::<RestartContainerOptions>),
        )
        .await
        .map_err(|error| test.fatal(format!("restart socket proxy: {error}")))?;
        self.wait_ping(test).await
    }
}

/// The container's stdout and stderr, or why they could not be read.
async fn container_output(ctx: &Context, container_id: &str) -> String {
    let read = async {
        let mut logs = cluster().host_client().logs(
            container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..LogsOptions::default()
            }),
        );
        let mut combined = String::new();
        while let Some(chunk) = logs.next().await {
            match chunk {
                Ok(output) => combined.push_str(&output.to_string()),
                Err(error) => {
                    let _ = write!(combined, "\n(read logs: {error})");
                    break;
                }
            }
        }
        combined
    };
    ctx.run(read)
        .await
        .unwrap_or_else(|ended| format!("(read logs: {ended})"))
}
