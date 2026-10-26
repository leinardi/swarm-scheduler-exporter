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

//! The exporter's wiring, Go's `run`: the flags, the logger, the metrics, the Docker client, the
//! workers (reconciler, event listener, poller) and the HTTP server, and the one shutdown
//! deadline they share (Q8).

use std::future::Future;
use std::io::{self, Write as _};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::cli::{self, Args};
use crate::clock::{Clock, SystemClock};
use crate::collector::containers::{ContainersConfig, ContainersState};
use crate::collector::desired::ReplicaGauges;
use crate::collector::exporter_metrics::ExporterMetrics;
use crate::collector::health::{Health, HealthGauges, Timestamps};
use crate::collector::nodes::NodesByState;
use crate::collector::reconciler::{
    Collectors, Reconciler, ReconcilerHandle, listen_swarm_events, poll_and_publish_replicas_state,
};
use crate::collector::replicas_state::ReplicasState;
use crate::collector::service_update::ServiceUpdateMetrics;
use crate::collector::types::CustomLabels;
use crate::docker::{self, Client, DockerApi, Settings};
use crate::healthcheck::run_healthcheck;
use crate::logger::{self, Attr, Logger};
use crate::metrics::exposition::render;
use crate::metrics::process::ProcessCollector;
use crate::metrics::promhttp::HandlerMetrics;
use crate::metrics::{Collector, RegisterError, Registry};
use crate::server::{self, App, HealthFn, MetricsFn, MetricsRequest};
use crate::version::{COMMIT, DATE, VERSION};

/// The time from the shutdown signal to the exit (Q8): the HTTP drain, the worker join and the
/// runtime's shutdown all come out of it. 10s is Docker's default stop grace period.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// How [`run_with`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The process exit code.
    pub code: i32,
    /// The shutdown deadline, once the server stopped: what is left of it bounds the runtime's
    /// shutdown in `main`.
    pub deadline: Option<Instant>,
}

impl Exit {
    fn code(code: i32) -> Self {
        Self {
            code,
            deadline: None,
        }
    }
}

/// What the families are built with.
#[derive(Debug, Clone)]
pub struct MetricsOptions {
    /// The custom labels of every per-service family.
    pub custom_labels: CustomLabels,
    /// The poll interval, which sets the health window.
    pub poll_delay: Duration,
    /// The container metrics, registered and polled only when enabled.
    pub containers: ContainersConfig,
    /// The `swarm_exporter_build_info` labels: version, commit and date.
    pub build: [String; 3],
}

/// Every family the exporter registers in [`register_metrics`], and what the workers write.
#[derive(Debug, Clone)]
pub struct Metrics {
    /// The families the reconciler writes.
    pub collectors: Collectors,
    /// The health check behind `/healthz` and `swarm_exporter_health`, bound to the reconciler
    /// by [`start_workers`].
    pub health: Health,
    /// The container state metrics, when enabled.
    pub containers: Option<Arc<ContainersState>>,
}

/// The Prometheus Go client's `MustRegister`: the exporter registers each family once, so a refusal
/// is a programming error.
fn must_register(result: Result<(), RegisterError>) {
    if let Err(error) = result {
        panic!("{error}");
    }
}

/// Go's `registerMetrics`: builds every family and registers it on `registry` in Go's order
/// (desired and schedulable replicas, replicas state, health and build info, nodes, the
/// exporter's own families, service update, then the containers when enabled). The health check
/// reads the reconciler only once [`start_workers`] binds it; until then it is unhealthy for the
/// initial resync, as Go's is before the reconciler runs.
#[must_use]
pub fn register_metrics(
    registry: &Registry,
    options: &MetricsOptions,
    clock: Arc<dyn Clock>,
    logger: &Logger,
) -> Metrics {
    let custom = &options.custom_labels;
    let timestamps = Arc::new(Timestamps::default());
    let replica_gauges = Arc::new(ReplicaGauges::new(custom));
    must_register(replica_gauges.register(registry));
    let replicas_state = Arc::new(ReplicasState::new(custom, logger.clone()));
    must_register(replicas_state.register(registry));
    let health = Health::unbound(Arc::clone(&timestamps), options.poll_delay, clock);
    let [version, commit, date] = &options.build;
    must_register(HealthGauges::new(health.clone(), version, commit, date).register(registry));
    let nodes_by_state = Arc::new(NodesByState::new(logger.clone()));
    must_register(nodes_by_state.register(registry));
    let exporter_metrics = Arc::new(ExporterMetrics::new(timestamps));
    must_register(exporter_metrics.register(registry));
    let service_update = Arc::new(ServiceUpdateMetrics::new(custom));
    must_register(service_update.register(registry));
    let containers = options.containers.enabled.then(|| {
        let containers = Arc::new(ContainersState::new(
            options.containers.clone(),
            logger.clone(),
        ));
        must_register(containers.register(registry));
        containers
    });
    Metrics {
        collectors: Collectors {
            custom_labels: custom.clone(),
            replica_gauges,
            service_update,
            nodes_by_state,
            replicas_state,
            metrics: exporter_metrics,
        },
        health,
        containers,
    }
}

/// What Go's default registry and `promhttp.Handler` add besides the exporter's families: the
/// process collector, and the scrape handler's own counters (Q1: no `go_*`).
#[must_use]
pub fn register_runtime_metrics(registry: &Registry) -> HandlerMetrics {
    must_register(registry.register(Arc::new(ProcessCollector::new()) as Arc<dyn Collector>));
    let handler = HandlerMetrics::new();
    must_register(handler.register(registry));
    handler
}

/// The running workers.
#[derive(Debug)]
pub struct Workers {
    tasks: Vec<JoinHandle<()>>,
}

impl Workers {
    /// Waits for every worker to return, Go's `WaitGroup.Wait`.
    pub async fn join(self) {
        for task in self.tasks {
            // A worker that panicked has returned too.
            let _ = task.await;
        }
    }
}

/// Go's `startWorkers`: the reconciler, the event listener and the poller, until `root` is
/// cancelled. The reconciler is created first and bound to the health check (Go installs it when
/// `Run` starts; a reconciler that has not completed its first resync reads the same either way),
/// then the event-stream anchor is captured, before the reconciler's first resync lists anything,
/// so every change made while that resync runs reaches the reconciler as an event. The poller
/// waits for that first resync.
#[must_use]
pub fn start_workers<D: DockerApi + 'static>(
    root: &CancellationToken,
    docker: Arc<D>,
    metrics: &Metrics,
    poll_delay: Duration,
    clock: &Arc<dyn Clock>,
    logger: &Logger,
) -> Workers {
    let reconciler = Reconciler::new(
        Arc::clone(&docker),
        metrics.collectors.clone(),
        Arc::clone(clock),
        logger.clone(),
    );
    let handle = reconciler.handle();
    metrics.health.bind(handle.clone());
    let anchor = clock.now();

    let reconciler_cancel = root.clone();
    let reconciler_task = tokio::spawn(async move {
        let _ = reconciler.run(reconciler_cancel).await;
    });

    let listener_cancel = root.clone();
    let listener_docker = Arc::clone(&docker);
    let listener_handle = handle.clone();
    // The listener only returns once cancelled, so Go's "event listener exited with error",
    // logged for any other return, cannot happen.
    let listener_task = tokio::spawn(async move {
        let _ = listen_swarm_events(
            &*listener_docker,
            &listener_handle,
            anchor,
            &listener_cancel,
        )
        .await;
    });

    let poller = Poller {
        docker,
        handle,
        exporter_metrics: Arc::clone(&metrics.collectors.metrics),
        containers: metrics.containers.clone(),
        logger: logger.clone(),
        cancel: root.clone(),
    };
    let poller_task = tokio::spawn(poller.run(poll_delay));

    Workers {
        tasks: vec![reconciler_task, listener_task, poller_task],
    }
}

/// The poller: tasks, and containers when enabled, every poll delay, against the reconciler's
/// polling snapshots.
struct Poller<D> {
    docker: Arc<D>,
    handle: ReconcilerHandle,
    exporter_metrics: Arc<ExporterMetrics>,
    containers: Option<Arc<ContainersState>>,
    logger: Logger,
    cancel: CancellationToken,
}

impl<D: DockerApi> Poller<D> {
    /// Go's `startPoller`: waits for the reconciler's first resync (before it the caches are
    /// empty, and there is nothing to poll), polls at once, then on every tick. The interval is
    /// created before the first poll and its immediate first tick consumed, so the next poll is
    /// due one delay after the first started, as with Go's `Ticker`; a tick missed while a poll
    /// ran long is skipped, not made up in a burst, which is what the `Ticker`'s one-slot channel
    /// does.
    async fn run(self, delay: Duration) {
        tokio::select! {
            () = self.cancel.cancelled() => {
                self.logger.debug(
                    "polling loop: context canceled before the first resync completed",
                    &[],
                );
                return;
            }
            () = self.handle.ready() => {}
        }
        self.logger.debug(
            "start polling replicas state",
            &[Attr::duration("every", nanos(delay))],
        );
        let mut ticker = tokio::time::interval(delay);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;
        self.poll_once().await;
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    self.logger.debug("polling loop: context canceled", &[]);
                    return;
                }
                _ = ticker.tick() => self.poll_once().await,
            }
        }
    }

    /// Go's `pollOnce`: the replicas state, its duration observed and the poll counted whatever
    /// the outcome, a failure counted and logged; then the containers when enabled, a failure
    /// logged with the previous set kept.
    async fn poll_once(&self) {
        let start = Instant::now();
        let result =
            poll_and_publish_replicas_state(&*self.docker, &self.handle, &self.cancel).await;
        self.exporter_metrics.observe_poll_duration(start.elapsed());
        self.exporter_metrics.inc_polls();
        if let Err(error) = result {
            self.exporter_metrics.inc_poll_errors();
            self.logger
                .error("poll replicas state failed", &[Attr::error("err", &error)]);
        }
        let Some(containers) = &self.containers else {
            return;
        };
        match self
            .cancel
            .run_until_cancelled(containers.poll(&*self.docker))
            .await
        {
            Some(Ok(rows)) => containers.update(&rows),
            Some(Err(error)) => self.logger.warn(
                "poll containers state failed",
                &[Attr::error("err", &error)],
            ),
            // Go's list fails at once on the cancelled context.
            None => self.logger.warn(
                "poll containers state failed",
                &[Attr::string("err", "container list: context canceled")],
            ),
        }
    }
}

fn nanos(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

/// The endpoints Go's `server.NewMuxWithHealth` serves: `/healthz` from `health`, and
/// `/metrics` from `registry`, counted by `handler` as promhttp's `InstrumentMetricHandler`
/// counts it (in flight while the families are gathered and encoded, then one more request under
/// its status code), with `Date` from `clock` and the server's errors through `logger`.
#[must_use]
pub fn app(
    health: Health,
    registry: Arc<Registry>,
    handler: HandlerMetrics,
    clock: Arc<dyn Clock>,
    logger: Logger,
) -> App {
    let health: HealthFn = Box::new(move || {
        let (healthy, reason) = health.snapshot_now();
        (healthy, reason.to_owned())
    });
    let metrics: MetricsFn = Box::new(move |request: &MetricsRequest| {
        let in_flight = handler.begin();
        let families = registry.gather();
        let accept_encoding: Vec<&str> =
            request.accept_encoding.iter().map(String::as_str).collect();
        let exposition = render(
            &families,
            request.accept.as_deref(),
            &accept_encoding,
            request.names.as_ref(),
        );
        in_flight.finish(200);
        exposition
    });
    App::new(health, metrics, clock, logger)
}

/// Writes `text` to stdout or stderr, as Go's `fmt.Fprint` does, ignoring a write error.
fn print(stdout: bool, text: &str) {
    let _ = if stdout {
        io::stdout().write_all(text.as_bytes())
    } else {
        io::stderr().write_all(text.as_bytes())
    };
}

/// Go's `signal.NotifyContext`: a root token cancelled once `shutdown` resolves, and the time it
/// did, which starts the shutdown budget.
fn watch_shutdown(
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> (CancellationToken, Arc<OnceLock<Instant>>) {
    let root = CancellationToken::new();
    let signalled = Arc::new(OnceLock::new());
    tokio::spawn({
        let root = root.clone();
        let signalled = Arc::clone(&signalled);
        async move {
            shutdown.await;
            let _ = signalled.set(Instant::now());
            root.cancel();
        }
    });
    (root, signalled)
}

/// Go's `run`, on `args` (the program name first) and the environment `getenv`, shutting down
/// when `shutdown` resolves. In Go's order: the flags (the help, a parse error), the
/// `--healthcheck` probe, the poll-delay check, the logger and the start log, the custom labels,
/// the metrics, the shutdown watch, the Docker client and its API version, the workers, and the
/// HTTP server until the shutdown or its failure. `shutdown` is polled only from there, so a
/// probe or a failed startup is not held up by signal handling.
pub async fn run_with<I, T>(
    args: I,
    getenv: impl Fn(&str) -> Option<String>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let args: Args = match cli::parse(args) {
        Ok(args) => args,
        Err(exit) => {
            print(exit.stdout, &exit.message);
            return Exit::code(exit.code);
        }
    };
    // A probe of the running exporter, e.g. the image HEALTHCHECK: it touches no logger, no
    // metric and no Docker client.
    if args.healthcheck {
        return Exit::code(run_healthcheck(&args.listen_addr, &mut io::stdout()).await);
    }
    if let Err(error) = cli::check_poll_delay(args.poll_delay) {
        print(false, &format!("{error}\n"));
        return Exit::code(1);
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let logger = logger::configure(
        logger::Config {
            format: args.log_format,
            level: args.log_level,
            include_time: args.log_time,
        },
        Arc::clone(&clock),
        &getenv,
    );
    logger.info(
        "swarm-scheduler-exporter starting",
        &[
            Attr::string("version", VERSION),
            Attr::string("commit", COMMIT),
            Attr::string("date", DATE),
        ],
    );
    let custom_labels = match cli::validate_custom_labels(&args.labels) {
        Ok(labels) => {
            let (raw, sanitized): (Vec<String>, Vec<String>) = labels
                .into_iter()
                .map(|label| (label.raw, label.sanitized))
                .unzip();
            CustomLabels::new(&raw, &sanitized)
        }
        Err(error) => {
            logger.error("invalid custom labels", &[Attr::error("err", &error)]);
            return Exit::code(1);
        }
    };
    // Checked by check_poll_delay: at least one second.
    let poll_delay = Duration::from_nanos(u64::try_from(args.poll_delay).unwrap_or_default());
    let registry = Arc::new(Registry::new());
    let metrics = register_metrics(
        &registry,
        &MetricsOptions {
            custom_labels,
            poll_delay,
            containers: ContainersConfig::new(args.containers, args.containers_include_swarm),
            build: [VERSION.to_owned(), COMMIT.to_owned(), DATE.to_owned()],
        },
        Arc::clone(&clock),
        &logger,
    );

    let (root, signalled) = watch_shutdown(shutdown);

    // Configured from DOCKER_HOST, DOCKER_API_VERSION, DOCKER_CERT_PATH and DOCKER_TLS_VERIFY.
    // The API version is negotiated lazily, on the first request.
    let settings = match Settings::from_env(&getenv) {
        Ok(settings) => settings,
        Err(error) => {
            logger.error("docker client init failed", &[Attr::error("err", &error)]);
            return Exit::code(1);
        }
    };
    if let Err(error) = docker::version::validate_client_api_version(settings.client_version()) {
        logger.error(
            "unsupported Docker API version",
            &[Attr::error("err", &error)],
        );
        return Exit::code(1);
    }
    let docker = Arc::new(Client::new(settings));

    let workers = start_workers(&root, docker, &metrics, poll_delay, &clock, &logger);

    let handler = register_runtime_metrics(&registry);
    let app = Arc::new(app(
        metrics.health.clone(),
        registry,
        handler,
        clock,
        logger,
    ));
    let deadline: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let code = server::serve_until_done(&root, workers.join(), &args.listen_addr, app, {
        let deadline = Arc::clone(&deadline);
        move || {
            let start = signalled.get().copied().unwrap_or_else(Instant::now);
            *deadline.get_or_init(|| start + SHUTDOWN_BUDGET)
        }
    })
    .await;
    Exit {
        code,
        deadline: deadline.get().copied(),
    }
}

/// Resolves on the first SIGINT or SIGTERM. The handlers stay installed for the life of the
/// process, so later signals are swallowed, as with Go's `signal.NotifyContext` until `run`
/// returns. Signals cannot be watched when the runtime has no signal driver; then it never
/// resolves.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut interrupt), Ok(mut terminate)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
    }
}
