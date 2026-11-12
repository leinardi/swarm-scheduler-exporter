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

//! The exporter's wiring, Go's `run`: the flags, the log, the metrics, the Docker client, the
//! workers (reconciler, event listener, poller) and the HTTP server, and the one shutdown
//! deadline they share (Q8).

use std::future::Future;
use std::io::{self, Write as _};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::cli::{self, Args};
use crate::collector::containers::{ContainersConfig, ContainersState};
use crate::collector::desired::ReplicaGauges;
use crate::collector::exporter_metrics::ExporterMetrics;
use crate::collector::health::{Health, HealthGauges, Timestamps};
use crate::collector::nodes::{NodeMetrics, NodesByState};
use crate::collector::reconciler::{
    Collectors, Reconciler, ReconcilerHandle, listen_swarm_events, poll_and_publish_replicas_state,
};
use crate::collector::replicas_state::ReplicasState;
use crate::collector::service_restart::ServiceRestartPolicy;
use crate::collector::service_update::ServiceUpdateMetrics;
use crate::collector::types::CustomLabels;
use crate::docker::{self, Client, DockerApi};
use crate::healthcheck::run_healthcheck;
use crate::metrics;
use crate::server::{self, Exposition, HealthFn, MetricsFn};
use crate::version::{COMMIT, DATE, VERSION};
use clock::{Clock, SystemClock};
use prometheus_client::registry::Registry;
use service_kit::process::ProcessCollector;
use service_kit::runtime::Exit;

/// The time from the shutdown signal to the exit (Q8): the HTTP drain, the worker join and the
/// runtime's shutdown all come out of it. 10s is Docker's default stop grace period.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// What the families are built with.
#[derive(Debug, Clone)]
pub struct MetricsOptions {
    /// The custom labels, excluding restart-policy metadata.
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

/// Go's `registerMetrics`: builds every family and registers it on `registry` in Go's order
/// (desired and schedulable replicas, replicas state, health and build info, nodes, the
/// exporter's own families, service update, then the containers when enabled). The health check
/// reads the reconciler only once [`start_workers`] binds it; until then it is unhealthy for the
/// initial resync, as Go's is before the reconciler runs.
#[must_use]
pub fn register_metrics(
    registry: &mut Registry,
    options: &MetricsOptions,
    clock: Arc<dyn Clock>,
) -> Metrics {
    let custom = &options.custom_labels;
    let timestamps = Arc::new(Timestamps::default());
    let replica_gauges = Arc::new(ReplicaGauges::new(custom));
    replica_gauges.register(registry);
    let replicas_state = Arc::new(ReplicasState::new(custom));
    replicas_state.register(registry);
    let health = Health::unbound(Arc::clone(&timestamps), options.poll_delay, clock);
    let [version, commit, date] = &options.build;
    HealthGauges::new(health.clone(), version, commit, date).register(registry);
    let nodes_by_state = Arc::new(NodesByState::new());
    nodes_by_state.register(registry);
    let node_metrics = Arc::new(NodeMetrics::new());
    node_metrics.register(registry);
    let restart_policy = Arc::new(ServiceRestartPolicy::new());
    restart_policy.register(registry);
    let exporter_metrics = Arc::new(ExporterMetrics::new(timestamps));
    exporter_metrics.register(registry);
    let service_update = Arc::new(ServiceUpdateMetrics::new(custom));
    service_update.register(registry);
    let containers = options.containers.enabled.then(|| {
        let containers = Arc::new(ContainersState::new(options.containers.clone()));
        containers.register(registry);
        containers
    });
    Metrics {
        collectors: Collectors {
            custom_labels: custom.clone(),
            replica_gauges,
            service_update,
            nodes_by_state,
            node_metrics,
            restart_policy,
            replicas_state,
            metrics: exporter_metrics,
        },
        health,
        containers,
    }
}

/// What Go's default registry added besides the exporter's families: the process collector (Q1:
/// no `go_*`, and no `promhttp_metric_handler_*`).
pub fn register_runtime_metrics(registry: &mut Registry) {
    registry.register_collector(Box::new(ProcessCollector::new()));
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
) -> Workers {
    let reconciler = Reconciler::new(
        Arc::clone(&docker),
        metrics.collectors.clone(),
        Arc::clone(clock),
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
                debug!("polling loop: context canceled before the first resync completed");
                return;
            }
            () = self.handle.ready() => {}
        }
        debug!(every = ?delay, "start polling replicas state");
        let mut ticker = tokio::time::interval(delay);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;
        self.poll_once().await;
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    debug!("polling loop: context canceled");
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
            error!(error = %error, "poll replicas state failed");
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
            Some(Err(error)) => warn!(error = %error, "poll containers state failed"),
            // Go's list fails at once on the cancelled context.
            None => warn!(
                error = "container list: context canceled",
                "poll containers state failed"
            ),
        }
    }
}

/// The endpoints Go's `server.NewMuxWithHealth` serves: `/healthz` from `health`, and
/// `/metrics` from `registry` as OpenMetrics text; anything else is a 404.
pub fn app(health: Health, registry: Arc<Registry>) -> axum::Router {
    let health: HealthFn = Box::new(move || {
        let (healthy, reason) = health.snapshot_now();
        (healthy, reason.to_owned())
    });
    let metrics: MetricsFn = Box::new(move || Exposition {
        content_type: metrics::CONTENT_TYPE.to_owned(),
        body: metrics::render(&registry).into_bytes(),
    });
    server::router(health, metrics)
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
/// `--healthcheck` probe, the poll-delay check, the log and the start log, the custom labels,
/// the metrics, the shutdown watch, the Docker client and its API version, the workers, and the
/// HTTP server until the shutdown or its failure. `shutdown` is polled only from there, so a
/// probe or a failed startup is not held up by signal handling.
pub async fn run_with<I, T>(
    args: I,
    getenv: impl Fn(&str) -> Option<String>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Exit<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let args: Args = match cli::parse(args) {
        Ok(args) => args,
        Err(exit) => {
            print(exit.stdout, &exit.message);
            return Exit::early(exit.code);
        }
    };
    // A probe of the running exporter, e.g. the image HEALTHCHECK: it touches no log, no
    // metric and no Docker client.
    if args.healthcheck {
        return Exit::early(run_healthcheck(&args.listen_addr, &mut io::stdout()).await);
    }
    if let Err(error) = cli::check_poll_delay(args.poll_delay) {
        print(false, &format!("{error}\n"));
        return Exit::early(1);
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    // Installed once per process: a second run_with in the same process (the tests) logs through
    // the first one's subscriber, or the scoped default of the test.
    let _ = tracing_setup::install_global(tracing_setup::build(
        io::stdout,
        args.log_format.into(),
        args.log_level.filter(),
        tracing_setup::Options::new().time(args.log_time),
    ));
    info!(
        version = VERSION,
        commit = COMMIT,
        date = DATE,
        "swarm-scheduler-exporter starting"
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
            error!(error = %error, "invalid custom labels");
            return Exit::early(1);
        }
    };
    // Checked by check_poll_delay: at least one second.
    let poll_delay = args.poll_delay;
    let mut registry = Registry::default();
    let metrics = register_metrics(
        &mut registry,
        &MetricsOptions {
            custom_labels,
            poll_delay,
            containers: ContainersConfig::new(args.containers, args.containers_include_swarm),
            build: [VERSION.to_owned(), COMMIT.to_owned(), DATE.to_owned()],
        },
        Arc::clone(&clock),
    );

    let (root, signalled) = watch_shutdown(shutdown);

    // Configured from DOCKER_HOST, DOCKER_API_VERSION, DOCKER_CERT_PATH and DOCKER_TLS_VERIFY.
    // The API version is negotiated lazily, on the first request, unless DOCKER_API_VERSION pins
    // it.
    let config = match docker::settings_from_env(&getenv) {
        Ok(config) => config,
        Err(error @ docker::SettingsError::Version(_)) => {
            error!(error = %error, "unsupported Docker API version");
            return Exit::early(1);
        }
        Err(error) => {
            error!(error = %error, "docker client init failed");
            return Exit::early(1);
        }
    };
    let docker = match Client::new(config) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            error!(error = %error, "docker client init failed");
            return Exit::early(1);
        }
    };

    let workers = start_workers(&root, docker, &metrics, poll_delay, &clock);

    register_runtime_metrics(&mut registry);
    let app = app(metrics.health.clone(), Arc::new(registry));
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
