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

//! The exporter's HTTP surface: `/metrics` and `/healthz` on rust-common's `go-http`, which
//! answers them as the Go exporter's `http.ServeMux` behind an `http.Server` does (Q13), with
//! Go's four server timeouts, `net.Listen`'s bind and the drain to a deadline it is handed (Q8).
//!
//! What is the exporter's stays here: the two routes and their answers, the server's timeouts,
//! the `http listen: ` prefix of a listen error, where `net/http`'s own messages go, and Go's
//! `serveUntilDone`: a server that fails stops at once, and the workers are stopped after the
//! drain.

use std::future::Future;
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clock::Clock;
use go_http::{Drained, ListenError, Mux, Reply, Served, Server, TEXT_PLAIN, Timeouts};
use hyper::StatusCode;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

pub use go_http::MetricsRequest;

/// The metrics endpoint.
pub const METRICS_PATH: &str = "/metrics";
/// The health endpoint, which the healthcheck requests.
pub const HEALTHZ_PATH: &str = "/healthz";

/// The Go exporter's `http.Server` timeouts: `ReadHeaderTimeout` 5 s, `ReadTimeout` 10 s,
/// `WriteTimeout` 15 s and `IdleTimeout` 60 s.
pub const TIMEOUTS: Timeouts = Timeouts {
    read_header: Duration::from_secs(5),
    read: Duration::from_secs(10),
    write: Duration::from_secs(15),
    idle: Duration::from_secs(60),
};

/// What the Go exporter wrapped a listen error in.
const LISTEN_ERROR_PREFIX: &str = "http listen: ";

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

/// The `/metrics` answer for a request.
pub type MetricsFn = Box<dyn Fn(&MetricsRequest) -> Exposition + Send + Sync>;

/// The endpoints; the server's errors go through the log.
#[derive(Debug)]
pub struct App {
    server: Arc<Server>,
}

impl App {
    /// The endpoints answering from `health` and `metrics`, with `Date` read from `clock`, and
    /// the server's own errors logged.
    #[must_use]
    pub fn new(health: HealthFn, metrics: MetricsFn, clock: Arc<dyn Clock>) -> Self {
        Self::with_mux(mux(health, metrics), clock)
    }

    /// `mux` served with `Date` from `clock`, and `net/http`'s messages logged.
    fn with_mux(mux: Mux, clock: Arc<dyn Clock>) -> Self {
        Self {
            server: Arc::new(server(mux, clock)),
        }
    }
}

/// The Go exporter's mux: `/metrics` from `metrics` and `/healthz` from `health`.
fn mux(health: HealthFn, metrics: MetricsFn) -> Mux {
    Mux::new()
        .route(METRICS_PATH, move |request| {
            let exposition = metrics(&MetricsRequest::of(request));
            Reply::metrics(&exposition.content_type, None, exposition.body)
        })
        .route(HEALTHZ_PATH, move |_| healthz(&health))
}

/// `mux` behind a server whose own messages (Go's `log.Printf` lines) are logged at error level.
fn server(mux: Mux, clock: Arc<dyn Clock>) -> Server {
    Server::new(mux, clock, move |message| {
        error!(target: "http", "{}", message.trim_end());
    })
}

/// The health handler: `ok`, or the reason with a 503 (`unhealthy` when there is none), each
/// with a newline.
fn healthz(health: &HealthFn) -> Reply {
    let headers = vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))];
    let (healthy, reason) = health();
    if healthy {
        return Reply::new(StatusCode::OK, headers, "ok\n");
    }
    let reason = if reason.is_empty() {
        "unhealthy".to_owned()
    } else {
        reason
    };
    Reply::new(
        StatusCode::SERVICE_UNAVAILABLE,
        headers,
        format!("{reason}\n"),
    )
}

/// `net.Listen("tcp", address)` as Go binds it, a failure wrapped `http listen: `.
///
/// # Errors
///
/// A [`ListenError`] with Go's wording.
pub async fn bind(address: &str) -> Result<TcpListener, ListenError> {
    go_http::bind(address, LISTEN_ERROR_PREFIX).await
}

/// Serves `app` on `listener` until `shutdown` resolves or accepting fails, then closes the
/// listener at once and waits for the open connections until `drain_deadline()`, computed when
/// the drain starts. Returns how the drain ended, and Go's `Serve` error (an accept error that
/// is not temporary) when that is what stopped it.
pub async fn serve(
    listener: TcpListener,
    app: Arc<App>,
    timeouts: Timeouts,
    shutdown: impl Future<Output = ()>,
    drain_deadline: impl FnOnce() -> Instant,
) -> (Served, Option<io::Error>) {
    // A failed accept stops the server at once, as Go's `Serve` returning does, through a token
    // of its own: the caller's stop is not cancelled here.
    let failed = CancellationToken::new();
    let error = OnceLock::new();
    let served = go_http::serve(
        listener,
        Arc::clone(&app.server),
        timeouts,
        async {
            tokio::select! {
                () = shutdown => {}
                () = failed.cancelled() => {}
            }
        },
        drain_deadline,
        |cause| {
            let _ = error.set(cause);
            failed.cancel();
        },
    )
    .await;
    (served, error.into_inner())
}

/// Go's `serveUntilDone`: serves `app` on `address` until `root` is cancelled or the server
/// fails, then cancels `root` so the workers return, and waits for them. The drain and the wait
/// share one deadline, `deadline()`, computed when the server stops (Q8). It returns the exit
/// code: 0 after a requested shutdown, 1 when the server failed on its own (the address in use,
/// a failed accept).
pub async fn serve_until_done(
    root: &CancellationToken,
    workers: impl Future<Output = ()>,
    address: &str,
    app: Arc<App>,
    deadline: impl FnOnce() -> Instant,
) -> i32 {
    let (deadline, failure) = match bind(address).await {
        Err(error) => (deadline(), Some(error.to_string())),
        Ok(listener) => {
            let (served, error) = serve(listener, app, TIMEOUTS, root.cancelled(), deadline).await;
            if served.drained == Drained::DeadlineExceeded {
                warn!(error = "context deadline exceeded", "HTTP server shutdown");
            }
            (served.deadline, error.map(|error| error.to_string()))
        }
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

#[cfg(test)]
mod tests;
