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

//! The exporter's HTTP surface: `/metrics` and `/healthz` as an axum router, served by
//! service-kit's HTTP/1 server (hyper) with a 5 s header-read timeout, a 15 s write-stall timeout
//! and a drain bounded by the shutdown budget (Q8, Q13).
//!
//! What is the exporter's stays here: the two routes and their answers (anything else is axum's
//! 404), the `http listen: ` prefix of a listen error, and `serveUntilDone`: a server that fails
//! stops at once, the drain and the worker wait share one deadline from the stop, and the workers
//! are stopped after the drain.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use go_compat::host_port::split_host_port;
use service_kit::http::{Lifecycle, Outcome, ServerConfig};
use tokio::net::{TcpListener, TcpSocket};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

/// The metrics endpoint.
pub const METRICS_PATH: &str = "/metrics";
/// The health endpoint, which the healthcheck requests.
pub const HEALTHZ_PATH: &str = "/healthz";

/// How long a client may take to send a request's head, as the Go exporter's `ReadHeaderTimeout`.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a write may wait without a byte leaving before the connection is closed, as the Go
/// exporter's `WriteTimeout`.
pub const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(15);

/// The `Content-Type` of the health answers.
const TEXT_PLAIN: &str = "text/plain; charset=utf-8";

/// What the Go exporter wrapped a listen error in.
const LISTEN_ERROR_PREFIX: &str = "http listen: ";

/// The backlog of the listening socket.
const BACKLOG: u32 = 1024;

/// The health answer: healthy, or why not.
pub type HealthFn = Box<dyn Fn() -> (bool, String) + Send + Sync>;

/// What `/metrics` answers: the body and its `Content-Type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposition {
    /// The `Content-Type` header.
    pub content_type: String,
    /// The body.
    pub body: Vec<u8>,
}

/// The `/metrics` answer.
pub type MetricsFn = Box<dyn Fn() -> Exposition + Send + Sync>;

/// The exporter's routes: `/metrics` from `metrics` and `/healthz` from `health`; any other path
/// is axum's 404.
pub fn router(health: HealthFn, metrics: MetricsFn) -> Router {
    let health = Arc::new(health);
    let metrics = Arc::new(metrics);
    Router::new()
        .route(
            METRICS_PATH,
            get(move || {
                let exposition = metrics();
                async move {
                    (
                        [(header::CONTENT_TYPE, exposition.content_type)],
                        exposition.body,
                    )
                        .into_response()
                }
            }),
        )
        .route(
            HEALTHZ_PATH,
            get(move || {
                let answer = healthz(&health);
                async move { answer }
            }),
        )
}

/// The health handler: `ok`, or the reason with a 503 (`unhealthy` when there is none), each
/// with a newline.
fn healthz(health: &HealthFn) -> Response {
    let (healthy, reason) = health();
    let (status, body) = if healthy {
        (StatusCode::OK, "ok\n".to_owned())
    } else if reason.is_empty() {
        (StatusCode::SERVICE_UNAVAILABLE, "unhealthy\n".to_owned())
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, format!("{reason}\n"))
    };
    (status, [(header::CONTENT_TYPE, TEXT_PLAIN)], body).into_response()
}

/// A listen address that could not be bound, with the `http listen: ` prefix.
#[derive(Debug)]
pub struct ListenError(String);

impl fmt::Display for ListenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{LISTEN_ERROR_PREFIX}{}", self.0)
    }
}

impl std::error::Error for ListenError {}

/// Binds `address` (`host:port`, the host empty for every interface). An empty or unspecified
/// host listens on `[::]`, dual-stack, and on `0.0.0.0` where IPv6 is missing; a host name listens
/// on its first IPv4 address; `SO_REUSEADDR` is on.
///
/// # Errors
///
/// A [`ListenError`]: a malformed address, a host that does not resolve, or the system call that
/// failed.
pub async fn bind(address: &str) -> Result<TcpListener, ListenError> {
    let fail = |text: String| ListenError(text);
    let (host, port) =
        split_host_port(address).map_err(|error| fail(format!("listen tcp: {error}")))?;
    let port: u16 = port
        .parse()
        .map_err(|_| fail(format!("listen tcp: address {port}: invalid port")))?;
    let host = host.split('%').next().unwrap_or(host);
    let wildcard = host.is_empty()
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_unspecified());
    let candidates: Vec<SocketAddr> = if wildcard {
        vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
        ]
    } else {
        let mut found: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| fail(format!("listen tcp: lookup {host}: {error}")))?
            .collect();
        found.sort_by_key(SocketAddr::is_ipv6);
        found.truncate(1);
        found
    };
    let mut last = format!("listen tcp {address}: no suitable address");
    for candidate in candidates {
        match listen_on(candidate) {
            Ok(listener) => return Ok(listener),
            Err(error) => {
                last = format!("listen tcp {address}: {error}");
                // Only a host without IPv6 (EAFNOSUPPORT) falls back to 0.0.0.0: an address in
                // use on [::] must not quietly bind IPv4 only.
                if error.raw_os_error() != Some(97) {
                    break;
                }
            }
        }
    }
    Err(fail(last))
}

fn listen_on(address: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = if address.is_ipv6() {
        TcpSocket::new_v6()?
    } else {
        TcpSocket::new_v4()?
    };
    socket.set_reuseaddr(true)?;
    if address.is_ipv6() {
        // Dual-stack whatever net.ipv6.bindv6only says, so IPv4 clients reach `[::]` too.
        socket2::SockRef::from(&socket).set_only_v6(false)?;
    }
    socket.bind(address)?;
    socket.listen(BACKLOG)
}

/// Serves `router` on `listener` until `lifecycle` is stopped or accepting fails, then drains for
/// at most `drain`.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    lifecycle: &Lifecycle,
    drain: Duration,
) -> Outcome {
    let config = ServerConfig::new(HEADER_READ_TIMEOUT, WRITE_STALL_TIMEOUT, drain);
    service_kit::http::serve(listener, router, config, lifecycle).await
}

/// Go's `serveUntilDone`: serves `router` on `address` until `root` is cancelled or the server
/// fails, then cancels `root` so the workers return, and waits for them. The drain and the wait
/// share one deadline, `deadline()`, computed when the server stops (Q8): the drain is cut at it,
/// and so is the wait. It returns the exit code: 0 after a requested shutdown, 1 when the server
/// failed on its own (the address in use, a failed accept).
pub async fn serve_until_done(
    root: &CancellationToken,
    workers: impl Future<Output = ()>,
    address: &str,
    router: Router,
    deadline: impl FnOnce() -> Instant,
) -> i32 {
    let (deadline, failure) = match bind(address).await {
        Err(error) => (deadline(), Some(error.to_string())),
        Ok(listener) => serve_listener(root, listener, router, deadline).await,
    };
    let exit_code = match failure {
        Some(error) => {
            error!(error = %error, "http server error");
            1
        }
        None => 0,
    };
    // Stop the workers: on a server failure nothing else would end them.
    root.cancel();
    let _ = tokio::time::timeout_at(deadline, workers).await;
    exit_code
}

/// Serves on `listener` until `root` is cancelled, and returns the shared deadline and the
/// server's own failure, if any. The server's drain is long (the whole budget) and the cut comes
/// from here, at the deadline computed when the stop began.
async fn serve_listener(
    root: &CancellationToken,
    listener: TcpListener,
    router: Router,
    deadline: impl FnOnce() -> Instant,
) -> (Instant, Option<String>) {
    let mut deadline = Some(deadline);
    let lifecycle = Lifecycle::new();
    let serving = serve(listener, router, &lifecycle, crate::run::SHUTDOWN_BUDGET);
    tokio::pin!(serving);
    let mut stopped_at = None;
    let mut cut = false;
    // The stop is the caller's (`root`), or the server's own: a failed accept makes the server
    // stop its lifecycle and start draining, which is just as much the moment the shared deadline
    // is computed from.
    let finished = tokio::select! {
        outcome = &mut serving => Some(outcome),
        () = root.cancelled() => {
            lifecycle.stop();
            None
        }
        () = lifecycle.stop_token().cancelled() => None,
    };
    let outcome = if let Some(outcome) = finished {
        outcome
    } else {
        let at = deadline
            .take()
            .map_or_else(Instant::now, |compute| compute());
        stopped_at = Some(at);
        tokio::select! {
            outcome = &mut serving => outcome,
            () = tokio::time::sleep_until(at) => {
                cut = true;
                lifecycle.cut_token().cancel();
                serving.await
            }
        }
    };
    let failure = match outcome {
        Outcome::ListenerFailed(error) => Some(error.to_string()),
        Outcome::DeadlineExceeded => {
            cut = true;
            None
        }
        Outcome::Drained => None,
    };
    if cut {
        warn!(error = "context deadline exceeded", "HTTP server shutdown");
    }
    let at = stopped_at
        .or_else(|| deadline.take().map(|compute| compute()))
        .unwrap_or_else(Instant::now);
    (at, failure)
}

#[cfg(test)]
mod tests;
