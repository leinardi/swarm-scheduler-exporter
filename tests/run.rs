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

//! The wiring end to end: the workers as `run` starts them, against a fake Docker, and the
//! binary's exit codes and shutdown.

use std::future::Future;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use service_kit::tokio_util::sync::CancellationToken;
use tokio::time::Instant;

use clock::{Clock, SystemClock};
use service_kit::http::Lifecycle;
use service_kit::prometheus_client::registry::Registry;
use swarm_scheduler_exporter::collector::containers::ContainersConfig;
use swarm_scheduler_exporter::collector::reconciler::Role;
use swarm_scheduler_exporter::collector::types::CustomLabels;
use swarm_scheduler_exporter::docker::types::{
    ContainerInspect, ContainerSummary, Event, Node, ReplicatedService, Service, ServiceMode,
    ServiceSpec, Task,
};
use swarm_scheduler_exporter::docker::{DockerApi, Error, EventStream, Filters, StreamError};
use swarm_scheduler_exporter::run::{
    Metrics, MetricsOptions, SHUTDOWN_BUDGET, Workers, app, register_metrics, start_workers,
};
use swarm_scheduler_exporter::server;

/// How long a lifecycle test waits for what it expects, as Go's `waitUntil` and `waitWorkers`.
const WAIT: Duration = Duration::from_secs(5);

/// The calls [`LifecycleDocker`] saw.
#[derive(Debug, Default)]
struct Calls {
    service_list: usize,
    resynced: bool,
    task_list: usize,
    task_list_before_resync: bool,
    events: usize,
}

/// Go's `lifecycleDocker`: a swarm of one replicated service whose service list fails its first
/// `resync_failures` calls, and whose event stream stays open until it is dropped.
#[derive(Debug)]
struct LifecycleDocker {
    resync_failures: usize,
    // Every service list fails with Docker's 503 "This node is not a swarm manager".
    not_manager: bool,
    // Once set, the service, node and task lists fail with it too: a demoted manager.
    demoted: AtomicBool,
    containers: Vec<ContainerSummary>,
    calls: Mutex<Calls>,
}

impl LifecycleDocker {
    fn new(resync_failures: usize) -> Self {
        Self {
            resync_failures,
            not_manager: false,
            demoted: AtomicBool::new(false),
            containers: Vec::new(),
            calls: Mutex::new(Calls::default()),
        }
    }

    /// A daemon that is not a Swarm manager: a worker, or Swarm inactive.
    fn not_manager() -> Self {
        Self {
            not_manager: true,
            ..Self::new(0)
        }
    }

    /// The container list answers `containers`.
    fn with_containers(mut self, containers: Vec<ContainerSummary>) -> Self {
        self.containers = containers;
        self
    }

    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An event stream that never yields: the listener only leaves it when cancelled.
struct Quiet;

impl EventStream for Quiet {
    async fn next(&mut self) -> Result<Event, StreamError> {
        std::future::pending().await
    }
}

/// Docker's 503 on a worker.
fn not_manager_error() -> Error {
    Error::not_manager(
        "This node is not a swarm manager. Worker nodes can't be used to view or modify cluster \
         state.",
    )
}

fn one_service() -> Vec<Service> {
    vec![Service {
        id: "svc1".to_owned(),
        spec: ServiceSpec {
            name: "web".to_owned(),
            mode: ServiceMode {
                replicated: Some(ReplicatedService { replicas: Some(1) }),
                ..ServiceMode::default()
            },
            ..ServiceSpec::default()
        },
        ..Service::default()
    }]
}

impl DockerApi for LifecycleDocker {
    type Events = Quiet;

    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send {
        let mut calls = self.calls();
        calls.service_list += 1;
        let result = if self.not_manager || self.demoted.load(Ordering::SeqCst) {
            Err(not_manager_error())
        } else if calls.service_list <= self.resync_failures {
            // Any error: the reconciler retries every failure alike.
            Err(Error::deadline_exceeded())
        } else {
            calls.resynced = true;
            Ok(one_service())
        };
        drop(calls);
        std::future::ready(result)
    }

    fn node_list(&self) -> impl Future<Output = Result<Vec<Node>, Error>> + Send {
        std::future::ready(if self.demoted.load(Ordering::SeqCst) {
            Err(not_manager_error())
        } else {
            Ok(Vec::new())
        })
    }

    fn service_inspect(&self, _id: &str) -> impl Future<Output = Result<Service, Error>> + Send {
        std::future::ready(Ok(Service::default()))
    }

    fn task_list(&self) -> impl Future<Output = Result<Vec<Task>, Error>> + Send {
        let mut calls = self.calls();
        calls.task_list += 1;
        if !calls.resynced {
            calls.task_list_before_resync = true;
        }
        drop(calls);
        std::future::ready(if self.demoted.load(Ordering::SeqCst) {
            Err(not_manager_error())
        } else {
            Ok(Vec::new())
        })
    }

    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send {
        std::future::ready(Ok(self.containers.clone()))
    }

    fn container_inspect(
        &self,
        _id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send {
        std::future::ready(Ok(ContainerInspect::default()))
    }

    fn events(
        &self,
        _since: Option<Timestamp>,
        _filters: &Filters,
        _cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Quiet, Error>> + Send {
        self.calls().events += 1;
        std::future::ready(Ok(Quiet))
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Go's `configureMetrics`: every family registered the way `run` does, on a private registry.
fn configure_metrics(poll_delay: Duration) -> (Arc<Registry>, Metrics) {
    configure_metrics_with(poll_delay, false)
}

/// [`configure_metrics`], with `--containers` when `containers` is set.
fn configure_metrics_with(poll_delay: Duration, containers: bool) -> (Arc<Registry>, Metrics) {
    let mut registry = Registry::default();
    let metrics = register_metrics(
        &mut registry,
        &MetricsOptions {
            custom_labels: CustomLabels::default(),
            poll_delay,
            containers: ContainersConfig::new(containers, false),
            build: ["test".to_owned(), "none".to_owned(), "unknown".to_owned()],
        },
        clock(),
    );
    (Arc::new(registry), metrics)
}

/// Go's `runWorkers`: the workers as `run` starts them, polling every hour.
fn run_workers(
    cancel: &CancellationToken,
    docker: &Arc<LifecycleDocker>,
    metrics: &Metrics,
) -> Workers {
    start_workers(
        cancel,
        Arc::clone(docker),
        metrics,
        Duration::from_secs(3600),
        &clock(),
    )
}

/// Go's `waitUntil`: polls `condition` every 10ms, failing with `what` after 5s.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Go's `waitWorkers`.
async fn wait_workers(workers: Workers) {
    assert!(
        tokio::time::timeout(WAIT, workers.join()).await.is_ok(),
        "workers did not return after cancellation"
    );
}

/// Serves the endpoints `run` wires for `metrics` on a loopback port until `cancel` fires.
async fn serve_endpoints(
    cancel: &CancellationToken,
    registry: &Arc<Registry>,
    metrics: &Metrics,
) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("bind: {error}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("local address: {error}"));
    let router = app(metrics.health.clone(), Arc::clone(registry));
    let lifecycle = Lifecycle::new();
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let serving = server::serve(listener, router, &lifecycle, Duration::from_secs(1));
        tokio::pin!(serving);
        tokio::select! {
            _ = &mut serving => {}
            () = cancel.cancelled() => {
                lifecycle.stop();
                serving.await;
            }
        }
    });
    addr
}

/// A `GET` of `path` with `Connection: close`: the status code and the body.
async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    tokio::task::spawn_blocking(move || {
        let mut stream =
            TcpStream::connect(addr).unwrap_or_else(|error| panic!("connect: {error}"));
        stream
            .write_all(request.as_bytes())
            .unwrap_or_else(|error| panic!("write: {error}"));
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .unwrap_or_else(|error| panic!("read: {error}"));
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_default();
        (status, body.to_owned())
    })
    .await
    .unwrap_or_else(|error| panic!("request: {error}"))
}

/// Go's `gatheredValue`: the value of the single series of the gauge `name`.
fn gathered_value(registry: &Registry, name: &str) -> f64 {
    let mut body = String::new();
    service_kit::prometheus_client::encoding::text::encode(&mut body, registry)
        .unwrap_or_else(|error| panic!("encode: {error}"));
    let values: Vec<f64> = body
        .lines()
        .filter_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|error| panic!("{name}: {error}"))
        })
        .collect();
    assert_eq!(values.len(), 1, "no single-series gauge {name} gathered");
    values[0]
}

/// Fixed restart-policy labels do not collide with a user-requested custom label.
#[tokio::test(flavor = "multi_thread")]
async fn restart_condition_custom_labels_work_through_running_workers_and_http() {
    use swarm_scheduler_exporter::labels::validate_and_sanitize_label_names;
    for raw in ["restart_condition", "restart.condition"] {
        let names = validate_and_sanitize_label_names(&[raw.to_owned()])
            .unwrap_or_else(|error| panic!("custom label: {error}"));
        let mut registry = Registry::default();
        let metrics = register_metrics(
            &mut registry,
            &MetricsOptions {
                custom_labels: CustomLabels::new(&[raw.to_owned()], &names),
                poll_delay: Duration::from_secs(3600),
                containers: ContainersConfig::new(false, false),
                build: [String::new(), String::new(), String::new()],
            },
            clock(),
        );
        let registry = Arc::new(registry);
        let docker = Arc::new(LifecycleDocker::new(0));
        let cancel = CancellationToken::new();
        let workers = run_workers(&cancel, &docker, &metrics);
        wait_until("the first poll", || docker.calls().task_list > 0).await;
        let address = serve_endpoints(&cancel, &registry, &metrics).await;
        let (status, body) = get(address, "/metrics").await;
        cancel.cancel();
        wait_workers(workers).await;
        assert_eq!(status, 200);
        let restart: Vec<_> = body
            .lines()
            .filter(|line| line.starts_with("swarm_service_restart_policy{"))
            .collect();
        assert_eq!(restart.len(), 3);
        assert!(
            restart
                .iter()
                .all(|line| !line.contains("restart_condition=\"\""))
        );
        assert!(
            restart
                .iter()
                .any(|line| line.contains("restart_condition=\"any\"") && line.ends_with(" 1.0"))
        );
        assert!(
            body.lines()
                .any(|line| line.starts_with("swarm_service_desired_replicas{")
                    && line.contains("restart_condition=\"\""))
        );
    }
}

/// Go: `TestWorkers_FirstResyncRetryThenPollAndListenOnce`
#[tokio::test(flavor = "multi_thread")]
async fn first_resync_retry_then_poll_and_listen_once() {
    let (_registry, metrics) = configure_metrics(Duration::from_secs(3600));
    let docker = Arc::new(LifecycleDocker::new(1));
    let cancel = CancellationToken::new();
    let workers = run_workers(&cancel, &docker, &metrics);
    wait_until("the first poll after the first resync", || {
        docker.calls().task_list > 0
    })
    .await;
    wait_until("the event stream", || docker.calls().events > 0).await;
    cancel.cancel();
    wait_workers(workers).await;
    let calls = docker.calls();
    assert!(
        !calls.task_list_before_resync,
        "the poller listed tasks before the first resync succeeded"
    );
    assert_eq!(
        calls.service_list, 2,
        "service list calls: one failed resync, one retry"
    );
    assert_eq!(calls.events, 1, "event streams opened");
}

/// Go: `TestWorkers_CancelBeforeFirstResyncStopsPoller`
#[tokio::test(flavor = "multi_thread")]
async fn cancel_before_first_resync_stops_poller() {
    let (_registry, metrics) = configure_metrics(Duration::from_secs(3600));
    let docker = Arc::new(LifecycleDocker::new(usize::MAX));
    let cancel = CancellationToken::new();
    let workers = run_workers(&cancel, &docker, &metrics);
    wait_until("the first resync attempt", || {
        docker.calls().service_list > 0
    })
    .await;
    // The reconciler now waits out its backoff and the poller waits for the first resync; the
    // event stream is open, since it must be before the resync lists anything.
    cancel.cancel();
    wait_workers(workers).await;
    let calls = docker.calls();
    assert_eq!(calls.task_list, 0, "the first resync never succeeded");
    assert!(calls.events <= 1, "event streams opened: {}", calls.events);
}

/// Issue #140: the containers are polled without waiting for the first resync, so a daemon whose
/// service list keeps failing (a Swarm worker, or Swarm inactive) still gets its container state.
#[tokio::test(flavor = "multi_thread")]
async fn containers_poll_without_waiting_for_the_first_resync() {
    let (registry, metrics) = configure_metrics_with(Duration::from_secs(3600), true);
    let docker =
        Arc::new(
            LifecycleDocker::new(usize::MAX).with_containers(vec![ContainerSummary {
                id: "c1".to_owned(),
                names: vec!["/plain".to_owned()],
                state: "running".to_owned(),
                ..ContainerSummary::default()
            }]),
        );
    let cancel = CancellationToken::new();
    let workers = run_workers(&cancel, &docker, &metrics);
    let running = "swarm_container_state{container=\"plain\"";
    wait_until("the first container poll", || {
        let mut body = String::new();
        service_kit::prometheus_client::encoding::text::encode(&mut body, &registry)
            .unwrap_or_else(|error| panic!("encode: {error}"));
        body.lines().any(|line| {
            line.starts_with(running)
                && line.contains("state=\"running\"")
                && line.ends_with(" 1.0")
        })
    })
    .await;
    cancel.cancel();
    wait_workers(workers).await;
    let calls = docker.calls();
    assert!(!calls.resynced, "the first resync never succeeded");
    assert_eq!(
        calls.task_list, 0,
        "no replicas poll before the first resync"
    );
}

fn plain_container() -> ContainerSummary {
    ContainerSummary {
        id: "c1".to_owned(),
        names: vec!["/plain".to_owned()],
        state: "running".to_owned(),
        ..ContainerSummary::default()
    }
}

/// Issue #140: on a daemon that is not a Swarm manager, `--containers` publishes the containers
/// and is healthy on them; the replicas poller never polls.
#[tokio::test(flavor = "multi_thread")]
async fn a_non_manager_with_containers_is_healthy_on_its_containers() {
    let (registry, metrics) = configure_metrics_with(Duration::from_secs(1), true);
    let docker = Arc::new(LifecycleDocker::not_manager().with_containers(vec![plain_container()]));
    let cancel = CancellationToken::new();
    let addr = serve_endpoints(&cancel, &registry, &metrics).await;
    let workers = run_workers(&cancel, &docker, &metrics);
    let deadline = Instant::now() + WAIT;
    while get(addr, "/healthz").await != (200, "ok\n".to_owned()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for /healthz: {:?}",
            get(addr, "/healthz").await
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (_, body) = get(addr, "/metrics").await;
    assert!(
        body.lines().any(
            |line| line.starts_with("swarm_container_state{container=\"plain\"")
                && line.contains("state=\"running\"")
                && line.ends_with(" 1.0")
        ),
        "{body}"
    );
    let polls = gathered_value(&registry, "swarm_exporter_polls_total");
    assert!(
        polls.abs() < f64::EPSILON,
        "swarm_exporter_polls_total = {polls}"
    );
    cancel.cancel();
    wait_workers(workers).await;
    assert_eq!(docker.calls().task_list, 0);
}

/// A manager demoted while the replicas poller runs: the poller's task list is refused first,
/// the resync it asks for finds the demotion, the Swarm series go, and from then on the poller
/// skips its ticks: no task list and no poll counted.
#[tokio::test(flavor = "multi_thread")]
async fn a_demoted_manager_stops_polling() {
    let (registry, metrics) = configure_metrics(Duration::from_secs(1));
    let docker = Arc::new(LifecycleDocker::new(0));
    let cancel = CancellationToken::new();
    let workers = start_workers(
        &cancel,
        Arc::clone(&docker),
        &metrics,
        Duration::from_millis(20),
        &clock(),
    );
    wait_until("a few polls", || docker.calls().task_list > 2).await;
    let mut before = String::new();
    service_kit::prometheus_client::encoding::text::encode(&mut before, &registry)
        .unwrap_or_else(|error| panic!("encode: {error}"));
    assert!(
        before
            .lines()
            .any(|line| line.starts_with("swarm_service_desired_replicas{")),
        "{before}"
    );
    docker.demoted.store(true, Ordering::SeqCst);
    wait_until("the demotion", || metrics.health.role() == Role::NotManager).await;
    // A poll already past the role check when the demotion landed may still finish.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let polls = gathered_value(&registry, "swarm_exporter_polls_total");
    let task_lists = docker.calls().task_list;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        docker.calls().task_list,
        task_lists,
        "task lists after the demotion"
    );
    let after = gathered_value(&registry, "swarm_exporter_polls_total");
    assert!(
        (after - polls).abs() < f64::EPSILON,
        "swarm_exporter_polls_total {polls} -> {after}"
    );
    assert_eq!(
        metrics.health.snapshot_now(),
        (false, "not a swarm manager")
    );
    let mut body = String::new();
    service_kit::prometheus_client::encoding::text::encode(&mut body, &registry)
        .unwrap_or_else(|error| panic!("encode: {error}"));
    assert!(
        !body.lines().any(|line| line.starts_with("swarm_service_")),
        "{body}"
    );
    cancel.cancel();
    wait_workers(workers).await;
}

/// Without `--containers`, a daemon that is not a Swarm manager has nothing to export: unhealthy,
/// with its own reason.
#[tokio::test(flavor = "multi_thread")]
async fn a_non_manager_without_containers_is_unhealthy() {
    let (registry, metrics) = configure_metrics(Duration::from_secs(1));
    let docker = Arc::new(LifecycleDocker::not_manager());
    let cancel = CancellationToken::new();
    let addr = serve_endpoints(&cancel, &registry, &metrics).await;
    let workers = run_workers(&cancel, &docker, &metrics);
    let expected = (503, "not a swarm manager\n".to_owned());
    let deadline = Instant::now() + WAIT;
    while get(addr, "/healthz").await != expected {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();
    wait_workers(workers).await;
}

/// Go: `TestWorkers_PermanentResyncFailureIsUnhealthy`
#[tokio::test(flavor = "multi_thread")]
async fn permanent_resync_failure_is_unhealthy() {
    let (registry, metrics) = configure_metrics(Duration::from_secs(1));
    let docker = Arc::new(LifecycleDocker::new(usize::MAX));
    let cancel = CancellationToken::new();
    let addr = serve_endpoints(&cancel, &registry, &metrics).await;
    let workers = run_workers(&cancel, &docker, &metrics);
    // A second attempt comes after the first backoff. A poller that did not wait for the first
    // resync polls as soon as it starts, so by then it would have published a poll.
    wait_until("a second failed resync attempt", || {
        docker.calls().service_list > 1
    })
    .await;
    assert_eq!(
        get(addr, "/healthz").await,
        (503, "initial resync not completed\n".to_owned())
    );
    let health = gathered_value(&registry, "swarm_exporter_health");
    assert!(
        health.abs() < f64::EPSILON,
        "swarm_exporter_health = {health}, want 0"
    );
    cancel.cancel();
    wait_workers(workers).await;
}

/// Go: `TestWorkers_ResyncThenPublishIsHealthy`
#[tokio::test(flavor = "multi_thread")]
async fn resync_then_publish_is_healthy() {
    let (registry, metrics) = configure_metrics(Duration::from_secs(1));
    let docker = Arc::new(LifecycleDocker::new(1));
    let cancel = CancellationToken::new();
    let addr = serve_endpoints(&cancel, &registry, &metrics).await;
    let workers = run_workers(&cancel, &docker, &metrics);
    let deadline = Instant::now() + WAIT;
    while get(addr, "/healthz").await.0 != 200 {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for /healthz to report healthy after the retried resync"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let health = gathered_value(&registry, "swarm_exporter_health");
    assert!(
        (health - 1.0).abs() < f64::EPSILON,
        "swarm_exporter_health = {health}, want 1"
    );
    cancel.cancel();
    wait_workers(workers).await;
}

/// The binary, with an environment that holds only `env`: no Docker socket is reachable unless a
/// test points `DOCKER_HOST` at one.
#[expect(
    clippy::disallowed_methods,
    reason = "the tests run the built exporter as a child"
)]
fn exporter(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_swarm-scheduler-exporter"));
    command
        .args(args)
        .env_clear()
        .env("DOCKER_HOST", "tcp://127.0.0.1:1")
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    command
}

fn run_exporter(args: &[&str], env: &[(&str, &str)]) -> Output {
    exporter(args, env)
        .output()
        .unwrap_or_else(|error| panic!("run the exporter: {error}"))
}

/// One startup case: the arguments and environment, the exit code, and a text the output must
/// contain, on stdout or stderr.
struct ExitCase {
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    code: i32,
    stdout: &'static str,
    stderr: &'static str,
}

/// The startup cases of [`flags_and_startup_exit_codes`].
const EXIT_CASES: [ExitCase; 13] = [
    ExitCase {
        args: &["-containers"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "single-dash long flags are not supported: '-containers'; use '--containers'",
    },
    ExitCase {
        args: &["--containers", "false"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "unexpected argument 'false'",
    },
    ExitCase {
        args: &["--log-level=verbose"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "invalid value 'verbose' for '--log-level",
    },
    ExitCase {
        args: &["--log-format=xml"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "invalid value 'xml' for '--log-format",
    },
    ExitCase {
        args: &["--log-format=plain"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "[possible values: text, json]",
    },
    ExitCase {
        args: &["--log-level=fatal"],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "[possible values: debug, info, warn, error]",
    },
    ExitCase {
        args: &["--label="],
        env: &[],
        code: 2,
        stdout: "",
        stderr: "empty flag value",
    },
    ExitCase {
        args: &["--help"],
        env: &[],
        code: 0,
        stdout: "--poll-delay",
        stderr: "",
    },
    ExitCase {
        args: &["--poll-delay=500ms"],
        env: &[],
        code: 1,
        stdout: "",
        stderr: "poll-delay must be >= 1s\n",
    },
    ExitCase {
        // The probe runs before the poll-delay check, as in Go.
        args: &[
            "--healthcheck",
            "--poll-delay=1ms",
            "--listen-addr=127.0.0.1:1",
        ],
        env: &[],
        code: 1,
        stdout: "healthcheck: Get \"http://127.0.0.1:1/healthz\": ",
        stderr: "",
    },
    ExitCase {
        args: &["--log-format=json", "--label=service"],
        env: &[],
        code: 1,
        stdout: "\"level\":\"ERROR\",\"message\":\"invalid custom labels\",\"error\":\"custom label \
                 collides with a reserved label name: \\\"service\\\"\"",
        stderr: "",
    },
    ExitCase {
        args: &["--log-format=json"],
        env: &[("DOCKER_CERT_PATH", "/nonexistent")],
        code: 1,
        stdout: "\"level\":\"ERROR\",\"message\":\"docker client init failed\"",
        stderr: "",
    },
    ExitCase {
        args: &["--log-format=json"],
        env: &[("DOCKER_API_VERSION", "1.20")],
        code: 1,
        stdout: "\"level\":\"ERROR\",\"message\":\"unsupported Docker API version\"",
        stderr: "",
    },
];

/// The exit codes of the flags and the startup checks: Q3's refusals and help, Q4's refused log
/// flags, Go's `poll-delay must be >= 1s` after `--healthcheck`, and the startup failures that
/// exit 1 after the start log (the custom labels, Q5's TLS refusal, the API version range).
#[test]
fn flags_and_startup_exit_codes() {
    for case in &EXIT_CASES {
        let output = run_exporter(case.args, case.env);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let context = format!(
            "{:?} {:?}:\nstdout: {stdout}\nstderr: {stderr}",
            case.args, case.env
        );
        assert_eq!(output.status.code(), Some(case.code), "{context}");
        assert!(stdout.contains(case.stdout), "{context}");
        assert!(stderr.contains(case.stderr), "{context}");
        if case.stdout.is_empty() {
            assert!(stdout.is_empty(), "{context}");
        }
        if case.stderr.is_empty() {
            assert!(stderr.is_empty(), "{context}");
        }
    }
}

/// A Docker daemon that accepts every connection and answers nothing for 30s.
fn blocking_docker() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|error| panic!("bind: {error}"));
    let addr = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("local address: {error}"));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(30));
                drop(stream);
            });
        }
    });
    addr
}

fn free_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .unwrap_or_else(|error| panic!("free port: {error}"))
}

fn wait_for_exit(child: &mut Child, bound: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < bound {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|error| panic!("wait: {error}"))
        {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Q8: with every Docker request hanging for 30s, SIGTERM still ends the exporter with exit 0
/// within the shutdown budget plus 100ms, and a second signal is swallowed.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test signals the exporter child with kill"
)]
fn shutdown_within_budget_while_docker_blocks() {
    let docker = blocking_docker();
    let listen = free_port();
    let docker_host = format!("tcp://{docker}");
    let listen_arg = format!("--listen-addr={listen}");
    let mut child = exporter(&[&listen_arg], &[("DOCKER_HOST", &docker_host)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn: {error}"));
    let up = Instant::now();
    while TcpStream::connect(listen).is_err() {
        assert!(up.elapsed() < WAIT, "the exporter never listened");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The first Docker request is in flight, and hangs.
    std::thread::sleep(Duration::from_millis(200));
    let signal = |name: &str| {
        Command::new("kill")
            .args([name, &child.id().to_string()])
            .status()
            .unwrap_or_else(|error| panic!("kill: {error}"))
            .success()
    };
    let start = Instant::now();
    assert!(signal("-TERM"), "kill -TERM");
    // Unchecked: the exporter may already be gone. If it is not, the signal must be swallowed,
    // or the exit below is a death by SIGINT, with no code.
    let _ = signal("-INT");
    let status = wait_for_exit(&mut child, SHUTDOWN_BUDGET + Duration::from_secs(5));
    let elapsed = start.elapsed();
    if status.is_none() {
        let _ = child.kill();
    }
    assert_eq!(status.and_then(|status| status.code()), Some(0));
    assert!(
        elapsed <= SHUTDOWN_BUDGET + Duration::from_millis(100),
        "exit after {elapsed:?}"
    );
}
