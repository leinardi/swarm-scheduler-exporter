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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use swarm_scheduler_exporter::clock::{Clock, SystemClock};
use swarm_scheduler_exporter::collector::containers::ContainersConfig;
use swarm_scheduler_exporter::collector::types::CustomLabels;
use swarm_scheduler_exporter::docker::types::{
    ContainerInspect, ContainerSummary, Event, Node, Service, Task,
};
use swarm_scheduler_exporter::docker::{DockerApi, Error, EventStream, Filters, StreamError};
use swarm_scheduler_exporter::logger::{Config, Format, Level, Logger};
use swarm_scheduler_exporter::metrics::promhttp::HandlerMetrics;
use swarm_scheduler_exporter::metrics::{Registry, Value};
use swarm_scheduler_exporter::run::{
    Metrics, MetricsOptions, SHUTDOWN_BUDGET, Workers, app, register_metrics, start_workers,
};
use swarm_scheduler_exporter::server::{self, Timeouts};

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
    calls: Mutex<Calls>,
}

impl LifecycleDocker {
    fn new(resync_failures: usize) -> Self {
        Self {
            resync_failures,
            calls: Mutex::new(Calls::default()),
        }
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

fn one_service() -> Vec<Service> {
    serde_json::from_str(
        r#"[{"ID":"svc1","Spec":{"Name":"web","Mode":{"Replicated":{"Replicas":1}}}}]"#,
    )
    .unwrap_or_else(|error| panic!("service fixture: {error}"))
}

impl DockerApi for LifecycleDocker {
    type Events = Quiet;

    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send {
        let mut calls = self.calls();
        calls.service_list += 1;
        let result = if calls.service_list <= self.resync_failures {
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
        std::future::ready(Ok(Vec::new()))
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
        std::future::ready(Ok(Vec::new()))
    }

    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send {
        std::future::ready(Ok(Vec::new()))
    }

    fn container_inspect(
        &self,
        _id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send {
        std::future::ready(Ok(ContainerInspect::default()))
    }

    fn events(
        &self,
        _since: Timestamp,
        _filters: &Filters,
    ) -> impl Future<Output = Result<Quiet, Error>> + Send {
        self.calls().events += 1;
        std::future::ready(Ok(Quiet))
    }
}

fn quiet_logger() -> Logger {
    Logger::new(
        Config {
            format: Format::Text,
            level: Level::INFO,
            include_time: false,
        },
        Box::new(std::io::sink()),
        TimeZone::UTC,
        Arc::new(SystemClock),
    )
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Go's `configureMetrics`: every family registered the way `run` does, on a private registry.
fn configure_metrics(poll_delay: Duration) -> (Arc<Registry>, Metrics) {
    let registry = Arc::new(Registry::new());
    let metrics = register_metrics(
        &registry,
        &MetricsOptions {
            custom_labels: CustomLabels::default(),
            poll_delay,
            containers: ContainersConfig::new(false, false),
            build: ["test".to_owned(), "none".to_owned(), "unknown".to_owned()],
        },
        clock(),
        &quiet_logger(),
    );
    (registry, metrics)
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
        &quiet_logger(),
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
    let app = Arc::new(app(
        metrics.health.clone(),
        Arc::clone(registry),
        HandlerMetrics::new(),
        clock(),
        quiet_logger(),
    ));
    let shutdown = cancel.clone().cancelled_owned();
    tokio::spawn(server::serve(
        listener,
        app,
        Timeouts::EXPORTER,
        shutdown,
        Instant::now,
    ));
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
    let families = registry.gather();
    let family = families
        .iter()
        .find(|family| family.name == name && family.series.len() == 1)
        .unwrap_or_else(|| panic!("no single-series gauge {name} gathered"));
    match family.series[0].value {
        Value::Scalar(value) => value,
        Value::Histogram { .. } => panic!("{name} is a histogram"),
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
const EXIT_CASES: [ExitCase; 11] = [
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
        args: &["--label=service"],
        env: &[],
        code: 1,
        stdout: "level=ERROR msg=\"invalid custom labels\" err=\"custom label collides with a \
                 reserved label name: \\\"service\\\"\"\n",
        stderr: "",
    },
    ExitCase {
        args: &[],
        env: &[("DOCKER_CERT_PATH", "/nonexistent")],
        code: 1,
        stdout: "level=ERROR msg=\"docker client init failed\"",
        stderr: "",
    },
    ExitCase {
        args: &[],
        env: &[("DOCKER_API_VERSION", "1.20")],
        code: 1,
        stdout: "level=ERROR msg=\"unsupported Docker API version\"",
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
