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

//! The exporter's HTTP surface on hyper: `/metrics` and `/healthz`, answered as the Go exporter's
//! `http.ServeMux` behind an `http.Server` answers them, byte for byte where hyper can (Q13).
//!
//! - Exact paths, any method; `OPTIONS *` as Go's global options handler; Go's 404; and a 307 to
//!   the cleaned path for an unclean one, keeping the query ([`gopath`]).
//! - Go's header case and order: the handler's headers sorted by name, then `Date`, then
//!   `Content-Length`, then `Connection`. hyper keeps a header set by hand where it is and only
//!   appends what is missing, so all three are set here; hyper's own `Date` is off, so its
//!   parse-error answers carry none, as Go's do.
//! - Go's framing: a body of at most 2 KiB (Go's `bufferBeforeChunkingSize`) is sent with a
//!   `Content-Length`; a larger one is chunked, in chunks of 2 KiB (Go's chunk boundaries follow
//!   the handler's writes, which no client sees), and a HEAD answer to it has neither header.
//! - Go's four server timeouts, kept by [`timeouts`] instead of hyper's header timer.
//! - The listener is bound as Go's `net.Listen` binds it, and the drain on shutdown stops at a
//!   deadline it is handed (Q8).

pub mod gopath;
pub mod timeouts;

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, DATE,
    HeaderName, HeaderValue, LOCATION,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Version};
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::{GracefulConnection, GracefulShutdown};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::gofmt_compat::duration;
use crate::gofmt_compat::errno::io_error_text;
use crate::gofmt_compat::host_port::split_host_port;
use crate::logger::{Attr, Logger};
use crate::metrics::exposition::Exposition;

use self::gopath::Route;
pub use self::timeouts::{ConnState, TimedIo, Timeouts};

/// The metrics endpoint.
pub const METRICS_PATH: &str = "/metrics";
/// The health endpoint, which the healthcheck requests.
pub const HEALTHZ_PATH: &str = "/healthz";

const TEXT_PLAIN: &str = "text/plain; charset=utf-8";
const TEXT_HTML: &str = "text/html; charset=utf-8";

/// Go's `bufferBeforeChunkingSize`: a handler that writes at most this much gets a
/// `Content-Length`; anything longer is chunked.
const BUFFER_BEFORE_CHUNKING: usize = 2048;

/// The health answer: healthy, or why not.
pub type HealthFn = Box<dyn Fn() -> (bool, String) + Send + Sync>;

/// The `/metrics` answer for a request.
pub type MetricsFn = Box<dyn Fn(&MetricsRequest) -> Exposition + Send + Sync>;

/// What promhttp reads from a scrape: the first `Accept` header, every `Accept-Encoding` one,
/// and the `name[]` query filter, none when the query names no family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsRequest {
    /// The first `Accept` header.
    pub accept: Option<String>,
    /// Every `Accept-Encoding` header.
    pub accept_encoding: Vec<String>,
    /// The `name[]` values; a value that is not UTF-8 names no family.
    pub names: Option<BTreeSet<String>>,
}

/// A route only the tests serve, which waits for the future it makes.
#[cfg(test)]
pub(crate) type SlowRoute = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// What the endpoints answer from, and the clock and logger the server writes times with.
pub struct App {
    health: HealthFn,
    metrics: MetricsFn,
    clock: Arc<dyn Clock>,
    logger: Logger,
    #[cfg(test)]
    pub(crate) slow: Option<SlowRoute>,
}

impl fmt::Debug for App {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("App").finish_non_exhaustive()
    }
}

impl App {
    /// The endpoints answering from `health` and `metrics`, with `Date` read from `clock`, and
    /// the server's own errors written in the zone of `logger` and through it.
    #[must_use]
    pub fn new(
        health: HealthFn,
        metrics: MetricsFn,
        clock: Arc<dyn Clock>,
        logger: Logger,
    ) -> Self {
        Self {
            health,
            metrics,
            clock,
            logger,
            #[cfg(test)]
            slow: None,
        }
    }
}

/// An answer before Go's header order and framing are applied.
struct Reply {
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
    // OPTIONS * sets its Content-Length itself.
    explicit_length: bool,
}

impl Reply {
    fn new(
        status: StatusCode,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: impl Into<Bytes>,
    ) -> Self {
        Self {
            status,
            headers,
            body: body.into(),
            explicit_length: false,
        }
    }
}

/// `http.Error(w, "404 page not found", 404)`.
fn not_found() -> Reply {
    Reply::new(
        StatusCode::NOT_FOUND,
        vec![
            (CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN)),
            (
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            ),
        ],
        "404 page not found\n",
    )
}

/// `http.Redirect(w, r, location, 307)`. Go compares the method case-sensitively: only `GET`
/// gets the body, and only `GET` and `HEAD` the `Content-Type`.
fn redirect(method: &Method, location: &str) -> Reply {
    let Ok(location_value) = HeaderValue::from_str(location) else {
        // The location is escaped, so always a valid header value.
        return not_found();
    };
    let mut headers = Vec::with_capacity(2);
    let is_get = method.as_str() == "GET";
    if is_get || method.as_str() == "HEAD" {
        headers.push((CONTENT_TYPE, HeaderValue::from_static(TEXT_HTML)));
    }
    headers.push((LOCATION, location_value));
    let body = if is_get {
        format!(
            "<a href=\"{}\">Temporary Redirect</a>.\n\n",
            gopath::html_escape(location)
        )
    } else {
        String::new()
    };
    Reply::new(StatusCode::TEMPORARY_REDIRECT, headers, body)
}

/// The health handler: `ok`, or the reason with a 503 (`unhealthy` when there is none), each
/// with a newline.
fn healthz(app: &App) -> Reply {
    let headers = vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))];
    let (healthy, reason) = (app.health)();
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

/// promhttp's handler: the exposition, with its `Content-Type` and, when compressed, its
/// `Content-Encoding`.
fn metrics<B>(app: &App, request: &Request<B>) -> Reply {
    let header_text = |value: &HeaderValue| String::from_utf8_lossy(value.as_bytes()).into_owned();
    let names = request.uri().query().and_then(|query| {
        gopath::parse_query(query)
            .remove(b"name[]".as_slice())
            .map(|names| {
                names
                    .into_iter()
                    .filter_map(|name| String::from_utf8(name).ok())
                    .collect()
            })
    });
    let scrape = MetricsRequest {
        accept: request.headers().get(ACCEPT).map(header_text),
        accept_encoding: request
            .headers()
            .get_all(ACCEPT_ENCODING)
            .iter()
            .map(header_text)
            .collect(),
        names,
    };
    let exposition = (app.metrics)(&scrape);
    let mut headers = Vec::with_capacity(2);
    if let Some(encoding) = exposition.content_encoding {
        headers.push((CONTENT_ENCODING, HeaderValue::from_static(encoding)));
    }
    if let Ok(content_type) = HeaderValue::from_str(&exposition.content_type) {
        headers.push((CONTENT_TYPE, content_type));
    }
    Reply::new(StatusCode::OK, headers, exposition.body)
}

/// The patterns the mux serves.
fn patterns(app: &App) -> &'static [&'static str] {
    #[cfg(test)]
    if app.slow.is_some() {
        return &[METRICS_PATH, HEALTHZ_PATH, SLOW_PATH];
    }
    let _ = app;
    &[METRICS_PATH, HEALTHZ_PATH]
}

/// The test-only route that waits.
#[cfg(test)]
const SLOW_PATH: &str = "/slow";

/// Routes one request as the Go exporter's mux does.
fn handle<B>(app: &App, request: &Request<B>) -> Reply {
    let method = request.method();
    // serverHandler.ServeHTTP hands `OPTIONS *` to the global options handler.
    if method == Method::OPTIONS && request.uri().path() == "*" {
        let mut reply = Reply::new(
            StatusCode::OK,
            vec![(CONTENT_LENGTH, HeaderValue::from_static("0"))],
            Bytes::new(),
        );
        reply.explicit_length = true;
        return reply;
    }
    // CONNECT's target is an authority, which matches no path pattern.
    if method == Method::CONNECT {
        return not_found();
    }
    match gopath::route(request.uri().path(), request.uri().query(), patterns(app)) {
        Route::Matched(METRICS_PATH) => metrics(app, request),
        Route::Matched(HEALTHZ_PATH) => healthz(app),
        // Only the tests' slow route, which waited before the request got here.
        Route::Matched(_) => Reply::new(
            StatusCode::OK,
            vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))],
            "slow done\n",
        ),
        Route::Redirect(location) => redirect(method, &location),
        Route::NotFound => not_found(),
        Route::BadRequest => Reply::new(
            StatusCode::BAD_REQUEST,
            vec![
                (CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN)),
                (CONNECTION, HeaderValue::from_static("close")),
            ],
            "400 Bad Request",
        ),
    }
}

/// Whether an HTTP/1.1 request asked for the connection to close, as Go's `wantsClose` reads its
/// `Connection` tokens.
fn wants_close<B>(request: &Request<B>) -> bool {
    request.version() == Version::HTTP_11
        && request.headers().get_all(CONNECTION).iter().any(|value| {
            String::from_utf8_lossy(value.as_bytes())
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        })
}

/// Go's `http.TimeFormat` of `now`.
fn http_date(clock: &dyn Clock) -> HeaderValue {
    let date = clock
        .now()
        .strftime("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    HeaderValue::from_str(&date).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The answer in Go's header order and framing, with a body that reports to `state` when hyper
/// is done with it.
fn finish<B>(
    reply: Reply,
    request: &Request<B>,
    date: HeaderValue,
    state: ConnState,
) -> Response<ResponseBody> {
    let Reply {
        status,
        mut headers,
        body,
        explicit_length,
    } = reply;
    // The handler's headers sorted by name, as Go writes them; a sort on the lowercase names
    // gives the same order as one on Go's canonical ones for header-name characters.
    headers.retain(|(name, _)| explicit_length || *name != CONTENT_LENGTH);
    headers.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    let connection = headers
        .iter()
        .position(|(name, _)| *name == CONNECTION)
        .map(|index| headers.remove(index));
    let chunked = body.len() > BUFFER_BEFORE_CHUNKING;
    let is_head = request.method() == Method::HEAD;
    let mut response = Response::new(ResponseBody {
        data: body.clone(),
        chunked,
        state: Some(state),
    });
    *response.status_mut() = status;
    let map = response.headers_mut();
    for (name, value) in headers {
        map.append(name, value);
    }
    map.insert(DATE, date);
    // Go sets the length of a body it buffered whole, except for a HEAD answer that wrote
    // nothing.
    let head_wrote_nothing = is_head && body.is_empty();
    if !(explicit_length || chunked || head_wrote_nothing) {
        map.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    }
    if let Some((name, value)) = connection {
        map.insert(name, value);
    } else if wants_close(request) {
        map.insert(CONNECTION, HeaderValue::from_static("close"));
    }
    response
}

/// A response body that tells the connection's deadlines when hyper is done with it, and that
/// never reports having ended: hyper 1.12 otherwise treats it as absent, drops a `Content-Length:
/// 0` set by hand and appends its own after every other header.
#[derive(Debug)]
pub struct ResponseBody {
    data: Bytes,
    chunked: bool,
    state: Option<ConnState>,
}

impl Drop for ResponseBody {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state.on_response_done();
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.data.is_empty() {
            return Poll::Ready(None);
        }
        let size = if self.chunked {
            self.data.len().min(BUFFER_BEFORE_CHUNKING)
        } else {
            self.data.len()
        };
        let frame = self.data.split_to(size);
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        if self.chunked {
            SizeHint::default()
        } else {
            SizeHint::with_exact(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
        }
    }
}

/// One HTTP/1 connection over `io`, with Go's deadlines, header case and order.
pub fn connection<I>(
    io: I,
    app: Arc<App>,
    timeouts: Timeouts,
) -> impl GracefulConnection<Error = hyper::Error> + Send + 'static
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let state = ConnState::new(timeouts);
    let io = TokioIo::new(TimedIo::new(io, state.clone()));
    let service = service_fn(move |request: Request<Incoming>| {
        let app = Arc::clone(&app);
        let state = state.clone();
        state.on_request_head();
        async move {
            #[cfg(test)]
            if let Some(slow) = app.slow.as_ref().filter(|_| {
                gopath::route(request.uri().path(), None, patterns(&app))
                    == Route::Matched(SLOW_PATH)
            }) {
                slow().await;
            }
            let reply = handle(&app, &request);
            let date = http_date(&*app.clock);
            Ok::<_, Infallible>(finish(reply, &request, date, state))
        }
    });
    http1::Builder::new()
        .title_case_headers(true)
        .auto_date_header(false)
        // No hyper timer: the deadlines are the I/O wrapper's.
        .header_read_timeout(None)
        .serve_connection(io, service)
}

/// How the drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drained {
    /// Every connection finished before the deadline.
    Clean,
    /// Connections were still open at the deadline, Go's `Shutdown` returning the context's
    /// error.
    DeadlineExceeded,
}

/// How [`serve`] ended: the drain, the deadline it ran to, and the error that stopped the
/// server on its own, if one did.
#[derive(Debug)]
pub struct Served {
    /// How the drain ended.
    pub drained: Drained,
    /// The drain's deadline.
    pub deadline: Instant,
    /// Go's `Serve` error: an accept error that is not temporary.
    pub error: Option<io::Error>,
}

/// Whether Go's `Serve` retries an accept error: `syscall.Errno.Temporary` (`EINTR`, `EMFILE`,
/// `ENFILE`, and the timeouts `EAGAIN` and `ETIMEDOUT`), and `ECONNABORTED`, which Go's poller
/// retries by itself.
fn temporary(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(4 | 11 | 23 | 24 | 103 | 110))
}

/// Go's `log.Printf` line: `2006/01/02 15:04:05 <message>` in the local zone, on stderr.
fn go_log(app: &App, message: &str) {
    let now = app.clock.now();
    let offset = app.logger.zone().to_offset(now);
    let civil = offset.to_datetime(now);
    let _ = writeln!(
        io::stderr(),
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02} {message}",
        civil.year(),
        civil.month(),
        civil.day(),
        civil.hour(),
        civil.minute(),
        civil.second()
    );
}

/// Serves `app` on `listener` until `shutdown` resolves or accepting fails, then closes the
/// listener at once, so later connects are refused, and waits for the open connections until
/// `drain_deadline()`, computed when the drain starts.
pub async fn serve(
    listener: TcpListener,
    app: Arc<App>,
    timeouts: Timeouts,
    shutdown: impl Future<Output = ()>,
    drain_deadline: impl FnOnce() -> Instant,
) -> Served {
    let graceful = GracefulShutdown::new();
    let local = listener
        .local_addr()
        .map_or_else(|_| String::new(), |address| address.to_string());
    let mut shutdown = std::pin::pin!(shutdown);
    let mut retry = std::time::Duration::ZERO;
    let mut error = None;
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    retry = std::time::Duration::ZERO;
                    let watched = graceful.watch(connection(stream, Arc::clone(&app), timeouts));
                    tokio::spawn(async move {
                        let _ = watched.await;
                    });
                }
                Err(accept_error) if temporary(&accept_error) => {
                    // Go's Serve: 5ms, doubling, at most 1s.
                    retry = if retry.is_zero() {
                        std::time::Duration::from_millis(5)
                    } else {
                        (retry * 2).min(std::time::Duration::from_secs(1))
                    };
                    go_log(
                        &app,
                        &format!(
                            "http: Accept error: accept tcp {local}: accept4: {}; retrying in {}",
                            io_error_text(&accept_error),
                            duration::format(i64::try_from(retry.as_nanos()).unwrap_or(i64::MAX))
                        ),
                    );
                    tokio::time::sleep(retry).await;
                }
                Err(accept_error) => {
                    error = Some(io::Error::new(
                        accept_error.kind(),
                        format!("accept tcp {local}: accept4: {}", io_error_text(&accept_error)),
                    ));
                    break;
                }
            },
        }
    }
    let deadline = drain_deadline();
    drop(listener);
    let drained = match tokio::time::timeout_at(deadline, graceful.shutdown()).await {
        Ok(()) => Drained::Clean,
        Err(_) => Drained::DeadlineExceeded,
    };
    Served {
        drained,
        deadline,
        error,
    }
}

/// A listen failure: `http listen: ` and Go's `net.OpError` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenError(String);

impl fmt::Display for ListenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "http listen: {}", self.0)
    }
}

impl std::error::Error for ListenError {}

/// Go's `LookupPort` for TCP: decimal digits with an optional sign, from 0 to 65535. Go looks a
/// service name up in `/etc/services`; the exporter's distroless image has none, so a name is an
/// unknown port here.
fn parse_port(port: &str) -> Result<u16, String> {
    let (negative, digits) = match port.as_bytes().first() {
        Some(b'+') => (false, &port[1..]),
        Some(b'-') => (true, &port[1..]),
        _ => (false, port),
    };
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("lookup tcp/{port}: unknown port"));
    }
    let value = digits.bytes().fold(0_u32, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(u32::from(digit - b'0'))
    });
    match u16::try_from(value) {
        Ok(0) => Ok(0),
        Ok(value) if !negative => Ok(value),
        Ok(_) | Err(_) => Err(format!("address {port}: invalid port")),
    }
}

/// `net.Listen("tcp", address)`, which `TcpListener::bind` is not: an empty host is the
/// dual-stack wildcard `[::]` (`0.0.0.0` only where IPv6 is missing), a host name listens on
/// its first IPv4 address, `SO_REUSEADDR` is on, and the backlog is the kernel's limit, as Go
/// reads `somaxconn`.
///
/// # Errors
///
/// A [`ListenError`] with Go's wording for the address errors and the failing system call.
pub async fn bind(address: &str) -> Result<TcpListener, ListenError> {
    let (host, port) =
        split_host_port(address).map_err(|error| ListenError(format!("listen tcp: {error}")))?;
    let port = parse_port(port).map_err(|error| ListenError(format!("listen tcp: {error}")))?;
    let candidates: Vec<SocketAddr> = if host.is_empty() {
        vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
        ]
    } else if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| ListenError(format!("listen tcp: lookup {host}: {error}")))?
            .collect();
        resolved
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| resolved.first())
            .copied()
            .into_iter()
            .collect()
    };
    let shown = if host.is_empty() {
        format!(":{port}")
    } else {
        candidates
            .first()
            .map_or_else(|| address.to_owned(), ToString::to_string)
    };
    let mut last = ListenError(format!("listen tcp {shown}: no suitable address"));
    for candidate in candidates {
        match listen_on(candidate, host.is_empty()) {
            Ok(listener) => return Ok(listener),
            Err((call, error)) => {
                last = ListenError(format!(
                    "listen tcp {shown}: {call}: {}",
                    io_error_text(&error)
                ));
                // Only a host without IPv6 (EAFNOSUPPORT) falls back to 0.0.0.0: an address in
                // use on [::] must not quietly bind IPv4 only.
                if error.raw_os_error() != Some(97) {
                    return Err(last);
                }
            }
        }
    }
    Err(last)
}

/// One listening socket on `address`, with the system call that failed.
fn listen_on(
    address: SocketAddr,
    dual_stack: bool,
) -> Result<TcpListener, (&'static str, io::Error)> {
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )
    .map_err(|error| ("socket", error))?;
    socket
        .set_reuse_address(true)
        .map_err(|error| ("setsockopt", error))?;
    if dual_stack && address.is_ipv6() {
        socket
            .set_only_v6(false)
            .map_err(|error| ("setsockopt", error))?;
    }
    socket
        .set_nonblocking(true)
        .map_err(|error| ("setsockopt", error))?;
    socket
        .bind(&address.into())
        .map_err(|error| ("bind", error))?;
    // The kernel caps it at somaxconn, which is what Go asks for.
    socket
        .listen(i32::from(u16::MAX))
        .map_err(|error| ("listen", error))?;
    TcpListener::from_std(socket.into()).map_err(|error| ("listen", error))
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
    let logger = app.logger.clone();
    let (deadline, failure) = match bind(address).await {
        Err(error) => (deadline(), Some(error.to_string())),
        Ok(listener) => {
            let served = serve(
                listener,
                app,
                Timeouts::EXPORTER,
                root.cancelled(),
                deadline,
            )
            .await;
            if served.drained == Drained::DeadlineExceeded {
                logger.warn(
                    "HTTP server shutdown",
                    &[Attr::string("err", "context deadline exceeded")],
                );
            }
            (served.deadline, served.error.map(|error| error.to_string()))
        }
    };
    let exit_code = match failure {
        Some(error) => {
            logger.error("http server error", &[Attr::string("err", error)]);
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
