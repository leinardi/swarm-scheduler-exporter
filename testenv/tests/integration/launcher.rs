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

//! Runs the exporter binary under test against the Swarm, as a child process.
//!
//! The child gets `--listen-addr` on a probed free port, `--poll-delay 1s` and
//! `--log-format json`, then the test's flags, all spelled with two dashes as the Rust binary
//! takes them. Its environment is the suite's minus every `DOCKER_*` variable, plus `DOCKER_HOST`.
//! The port can be taken between the probe and the child's bind; the exporter then exits
//! non-zero, and that exit (like any other) is detected and retried on a new port, up to
//! [`EXPORTER_ATTEMPTS`] times. The exporter is stopped with SIGTERM when the test ends (killed
//! if it has not exited [`EXPORTER_STOP_TIMEOUT`] later), and its output is logged if the test
//! failed.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};
use testenv::context::Context;
use testenv::metrics_text::format_value;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

use crate::http::{get, scrape_metrics};
use crate::swarm::{ServiceKey, teardown_ctx};
use crate::testing::{Fatal, OrFatal as _, TestHandle, cluster, suite};
use crate::wait::{
    METRIC_DESIRED, METRIC_NODES_BY_STATE, METRIC_RUNNING, POLL_INTERVAL, go_duration, no_labels,
};

const EXPORTER_ATTEMPTS: usize = 3;
/// Bounds one attempt: healthy, then metrics seeded.
const EXPORTER_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an exporter sent SIGTERM gets before it is killed.
const EXPORTER_STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// When a launch counts as up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// Healthy and seeded, see [`readiness`].
    Seeded,
    /// Its HTTP server answers `/healthz`, healthy or not.
    Serving,
}

/// What to launch, and how.
pub struct Launch<'args> {
    docker_host: String,
    services: Vec<ServiceKey>,
    args: &'args [&'args str],
    ready: Ready,
    occupy_first_probed_port: bool,
}

impl<'args> Launch<'args> {
    /// Go's `startExporter`: against the manager, up once healthy and seeded with `services`.
    pub fn seeded(services: &[ServiceKey], args: &'args [&'args str]) -> Self {
        Self {
            docker_host: cluster().manager.docker_host.clone(),
            services: services.to_vec(),
            args,
            ready: Ready::Seeded,
            occupy_first_probed_port: false,
        }
    }

    /// Go's `startExporterServing`: up as soon as `/healthz` answers, for a test that looks at
    /// the exporter before it is ready.
    pub fn serving(docker_host: &str, args: &'args [&'args str]) -> Self {
        Self {
            docker_host: docker_host.to_owned(),
            services: Vec::new(),
            args,
            ready: Ready::Serving,
            occupy_first_probed_port: false,
        }
    }

    /// Go's `startExporterVia`: with `DOCKER_HOST` set to `docker_host`, such as a proxy in front
    /// of the manager.
    #[must_use]
    pub fn via(mut self, docker_host: &str) -> Self {
        docker_host.clone_into(&mut self.docker_host);
        self
    }

    /// Go's `occupyFirstProbedPort`: holds the first probed port with a listener of its own, so
    /// the first attempt loses the port race and the retry path runs.
    #[must_use]
    pub fn occupying_first_probed_port(mut self) -> Self {
        self.occupy_first_probed_port = true;
        self
    }

    /// Runs the exporter until it is up, retrying a launch that exits, and returns its base URL.
    pub async fn start(self, test: &TestHandle) -> Result<String, Fatal> {
        let ctx = test.ctx().clone();
        let current: Rc<RefCell<Option<ExporterProc>>> = Rc::new(RefCell::new(None));
        let transcript = Rc::new(RefCell::new(String::new()));

        {
            let (test, current, transcript) =
                (test.clone(), Rc::clone(&current), Rc::clone(&transcript));
            test.clone().cleanup(move || async move {
                let running = current.borrow_mut().take();
                if let Some(proc) = running {
                    if let Err(error) = proc.stop(&teardown_ctx()).await {
                        test.error(format!("stop exporter: {error}"));
                    }
                    transcript.borrow_mut().push_str(&proc.output());
                }
                if test.failed() {
                    test.log(format!("exporter output:\n{}", transcript.borrow()));
                }
            });
        }

        for attempt in 1..=EXPORTER_ATTEMPTS {
            let port = free_port().or_fatal(test)?;
            if attempt == 1 && self.occupy_first_probed_port {
                let blocker = TcpListener::bind(local_addr(port))
                    .map_err(|error| test.fatal(format!("occupy probed port {port}: {error}")))?;
                test.cleanup(move || async move { drop(blocker) });
            }

            let proc = launch_exporter(port, &self.docker_host, self.args).or_fatal(test)?;
            let _ = writeln!(
                transcript.borrow_mut(),
                "--- attempt {attempt}, port {port} ---"
            );
            let base_url = format!("http://{}", local_addr(port));
            let outcome = proc.wait_ready(&ctx, &base_url, &self).await;
            *current.borrow_mut() = Some(proc);
            match outcome {
                Ok(()) => return Ok(base_url),
                Err(WaitError::Exited) => {}
                Err(WaitError::Other(error)) => {
                    return Err(test.fatal(format!("exporter not ready: {error}")));
                }
            }

            test.log(format!(
                "exporter attempt {attempt} on port {port} failed, retrying: exporter exited"
            ));
            let failed = current.borrow_mut().take();
            if let Some(proc) = failed {
                proc.stop(&ctx).await.map_err(|error| {
                    test.fatal(format!("stop failed exporter attempt: {error}"))
                })?;
                transcript.borrow_mut().push_str(&proc.output());
            }
        }
        Err(test.fatal(format!(
            "exporter did not start in {EXPORTER_ATTEMPTS} attempts"
        )))
    }
}

/// The child's environment: the suite's own, minus every `DOCKER_*` variable, plus `DOCKER_HOST`.
/// The exporter reads its client settings from the environment, so an inherited
/// `DOCKER_TLS_VERIFY`, `DOCKER_CERT_PATH` or `DOCKER_API_VERSION` from the developer's shell would
/// otherwise leak into it.
fn exporter_env(docker_host: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("DOCKER_"))
        .collect();
    env.push(("DOCKER_HOST".to_owned(), docker_host.to_owned()));
    env
}

/// A port nothing listens on right now.
pub fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("probe free port: {error}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("probe free port: {error}"))
}

/// `127.0.0.1:<port>`.
pub fn local_addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Why a launch is not up.
enum WaitError {
    /// It exited, which the caller retries: a lost port race ends that way too.
    Exited,
    Other(String),
}

/// One running exporter child.
struct ExporterProc {
    pid: Option<Pid>,
    output: Arc<Mutex<Vec<u8>>>,
    exited: watch::Receiver<bool>,
}

fn launch_exporter(port: u16, docker_host: &str, args: &[&str]) -> Result<ExporterProc, String> {
    let listen = local_addr(port).to_string();
    let mut command = Command::new(&suite().exporter_binary);
    command
        .args([
            "--listen-addr",
            &listen,
            "--poll-delay",
            "1s",
            "--log-format",
            "json",
        ])
        .args(args)
        .env_clear()
        .envs(exporter_env(docker_host))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The exporter's logger writes to stdout and a crash lands on stderr: both go to one buffer.
    let mut child = command
        .spawn()
        .map_err(|error| format!("start exporter: {error}"))?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let readers = [
        child
            .stdout
            .take()
            .map(|pipe| copy_into(pipe, Arc::clone(&output))),
        child
            .stderr
            .take()
            .map(|pipe| copy_into(pipe, Arc::clone(&output))),
    ];
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw);
    let (exited_tx, exited) = watch::channel(false);
    let exit_output = Arc::clone(&output);
    tokio::spawn(async move {
        let status = child.wait().await;
        // As Go's cmd.Wait, the output is complete once the process has exited and the pipes
        // are drained.
        for reader in readers.into_iter().flatten() {
            let _ = reader.await;
        }
        let line = match status {
            Ok(status) => format!("--- exited: {status} ---\n"),
            Err(error) => format!("--- exited: {error} ---\n"),
        };
        exit_output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(line.as_bytes());
        let _ = exited_tx.send(true);
    });
    Ok(ExporterProc {
        pid,
        output,
        exited,
    })
}

fn copy_into(
    mut pipe: impl AsyncRead + Unpin + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = [0_u8; 8192];
        while let Ok(read) = pipe.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            output
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(&chunk[..read]);
        }
    })
}

impl ExporterProc {
    fn has_exited(&self) -> bool {
        *self.exited.borrow()
    }

    async fn wait_exit(&self) {
        let mut exited = self.exited.clone();
        let _ = exited.wait_for(|done| *done).await;
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }

    /// Waits for the launch to be up, or [`WaitError::Exited`] as soon as it exits.
    async fn wait_ready(
        &self,
        ctx: &Context,
        base_url: &str,
        launch: &Launch<'_>,
    ) -> Result<(), WaitError> {
        let deadline = Instant::now() + EXPORTER_READY_TIMEOUT;
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            let last = match launch.ready {
                Ready::Seeded => readiness(ctx, base_url, &launch.services).await,
                Ready::Serving => get(ctx, &format!("{base_url}/healthz")).await.map(|_| ()),
            };
            // A foreign process that grabbed the port could answer, but it cannot have served
            // seeded exporter metrics before our exporter, failing to bind, exited: checking the
            // exit after a successful probe settles whose metrics those were.
            if self.has_exited() {
                return Err(WaitError::Exited);
            }
            let Err(last) = last else {
                return Ok(());
            };
            let next = async {
                tokio::select! {
                    () = self.wait_exit() => Some(WaitError::Exited),
                    () = tokio::time::sleep_until(deadline) => Some(WaitError::Other(format!(
                        "exporter not ready within {}: {last}",
                        go_duration(EXPORTER_READY_TIMEOUT)
                    ))),
                    _ = ticker.tick() => None,
                }
            };
            match ctx.run(next).await {
                Err(ended) => {
                    return Err(WaitError::Other(format!(
                        "wait for exporter: {ended}: {last}"
                    )));
                }
                Ok(Some(error)) => return Err(error),
                Ok(None) => {}
            }
        }
    }

    /// Sends SIGTERM and waits for the exporter to exit, killing it if it does not in time.
    async fn stop(&self, ctx: &Context) -> Result<(), String> {
        if self.has_exited() {
            return Ok(());
        }
        if let Some(pid) = self.pid {
            let _ = kill_process(pid, Signal::TERM);
        }
        let waited = ctx
            .run(tokio::time::timeout(
                EXPORTER_STOP_TIMEOUT,
                self.wait_exit(),
            ))
            .await;
        if matches!(waited, Ok(Ok(()))) {
            return Ok(());
        }
        if let Some(pid) = self.pid {
            let _ = kill_process(pid, Signal::KILL);
        }
        self.wait_exit().await;
        Err(format!(
            "exporter did not exit within {} of SIGTERM; killed",
            go_duration(EXPORTER_STOP_TIMEOUT)
        ))
    }
}

/// Healthy and seeded: `/healthz` answers 200, `nodes_by_state` accounts for every cluster node,
/// and every service in `services` has desired and running series. Health alone is not enough:
/// it can turn 200 before the event listener has seeded the metadata cache.
pub async fn readiness(
    ctx: &Context,
    base_url: &str,
    services: &[ServiceKey],
) -> Result<(), String> {
    let (status, _) = get(ctx, &format!("{base_url}/healthz")).await?;
    if status != 200 {
        return Err(format!("/healthz answered {status}: not yet"));
    }
    let scraped = scrape_metrics(ctx, base_url).await?;
    let nodes = scraped.sum(METRIC_NODES_BY_STATE, &no_labels());
    let want = cluster().nodes.len();
    if nodes
        .total_cmp(&f64::from(u32::try_from(want).unwrap_or(u32::MAX)))
        .is_ne()
    {
        return Err(format!(
            "{METRIC_NODES_BY_STATE} sums to {}, want {want}: not yet",
            format_value(nodes)
        ));
    }
    for service in services {
        if !scraped.has(METRIC_DESIRED, &service.labels())
            || !scraped.has(METRIC_RUNNING, &service.labels())
        {
            return Err(format!(
                "no desired/running series yet for {}/{}: not yet",
                service.stack, service.service
            ));
        }
    }
    Ok(())
}
