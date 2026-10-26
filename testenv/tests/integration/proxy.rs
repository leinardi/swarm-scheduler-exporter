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

//! A proxy between the exporter and the manager's Docker API that injects the faults a real
//! daemon cannot be made to produce on demand: a failing service list, and an event stream that
//! drops, stays unavailable, and comes back without the history it missed. Everything else is
//! forwarded unchanged, streamed chunk by chunk as it arrives (the event stream is one long
//! response), as Go's `httputil.ReverseProxy` with `FlushInterval: -1` forwarded it.
//!
//! A stream the proxy cuts ends with an error from its body, so hyper drops the connection with
//! no terminating chunk, as Go's reverse proxy aborts a handler whose request context ends.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use regex::Regex;
use swarm_scheduler_exporter::clock::{Clock as _, SystemClock};
use tokio::net::TcpListener;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use crate::testing::{Fatal, OrFatal as _, TestHandle, cluster};

/// The error a forwarded body ends with: the upstream's, or the proxy's own cut. A concrete type,
/// not a boxed error: a boxed one trips the compiler's higher-ranked lifetime check on the
/// spawned connection future.
#[derive(Debug)]
struct ProxyError(String);

impl std::fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProxyError {}

type ProxyBody = BoxBody<Bytes, ProxyError>;

static SERVICE_LIST_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^(/v[0-9.]+)?/services$"));
static EVENTS_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"^(/v[0-9.]+)?/events$"));

fn pattern(text: &str) -> Regex {
    Regex::new(text).unwrap_or_else(|error| panic!("invalid pattern {text}: {error}"))
}

/// What the faults are set to, and what the proxy saw.
#[derive(Default)]
struct State {
    fail_service_list: bool,
    service_list_failures: usize,
    events_down: bool,
    forget_event_history: bool,
    event_streams: HashMap<u64, CancellationToken>,
    next_event_stream: u64,
    event_connects: usize,
}

/// The running proxy. Its listener and connections end when the test does.
pub struct DockerProxy {
    state: Arc<Mutex<State>>,
    address: SocketAddr,
}

impl DockerProxy {
    /// Starts a proxy to the manager's Docker API, closed when the test ends.
    pub async fn start(test: &TestHandle) -> Result<Arc<Self>, Fatal> {
        let upstream = cluster().manager.docker_host.replacen("tcp://", "", 1);
        let listener = TcpListener::bind("127.0.0.1:0").await.or_fatal(test)?;
        let address = listener.local_addr().or_fatal(test)?;
        let proxy = Arc::new(Self {
            state: Arc::new(Mutex::new(State::default())),
            address,
        });
        let shutdown = CancellationToken::new();
        let client: Client<HttpConnector, Incoming> =
            Client::builder(TokioExecutor::new()).build_http();
        let serving = Arc::clone(&proxy);
        let accept_shutdown = shutdown.clone();
        tokio::spawn(async move {
            while let Some(Ok((stream, _))) =
                accept_shutdown.run_until_cancelled(listener.accept()).await
            {
                let proxy = Arc::clone(&serving);
                let client = client.clone();
                let upstream = upstream.clone();
                let connection_shutdown = accept_shutdown.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let proxy = Arc::clone(&proxy);
                        let client = client.clone();
                        let upstream = upstream.clone();
                        let served: Pin<
                            Box<
                                dyn Future<Output = Result<Response<ProxyBody>, Infallible>> + Send,
                            >,
                        > = Box::pin(
                            async move { Ok(proxy.serve(client, upstream, request).await) },
                        );
                        served
                    });
                    let connection =
                        http1::Builder::new().serve_connection(TokioIo::new(stream), service);
                    // Dropping the connection closes it, mid-response or not.
                    let _ = connection_shutdown.run_until_cancelled(connection).await;
                });
            }
        });

        let closing = Arc::clone(&proxy);
        test.cleanup(move || async move {
            closing.cut_event_streams();
            shutdown.cancel();
        });
        Ok(proxy)
    }

    /// The `DOCKER_HOST` that reaches the manager through the proxy.
    pub fn docker_host(&self) -> String {
        format!("tcp://{}", self.address)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn serve(
        self: Arc<Self>,
        client: Client<HttpConnector, Incoming>,
        upstream: String,
        mut request: Request<Incoming>,
    ) -> Response<ProxyBody> {
        let path = request.uri().path().to_owned();
        if request.method() == Method::GET
            && SERVICE_LIST_PATH.is_match(&path)
            && self.take_service_list_failure()
        {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"message":"injected service list failure"}"#,
            );
        }
        if !EVENTS_PATH.is_match(&path) {
            return forward(&client, &upstream, request).await;
        }

        // An event stream, under a token the proxy can cancel, which drops the exporter's
        // connection the way a daemon restart or a network cut does.
        let (id, token) = {
            let mut state = self.state();
            if state.events_down {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    r#"{"message":"injected event stream outage"}"#,
                );
            }
            if state.forget_event_history {
                // The daemon's history as the exporter asks for it no longer holds what
                // happened while the stream was down, as after a daemon restart: only a resync
                // can catch that up.
                let now = SystemClock.now().as_second().to_string();
                rewrite_since(&mut request, &now);
            }
            let id = state.next_event_stream;
            state.next_event_stream += 1;
            let token = CancellationToken::new();
            state.event_streams.insert(id, token.clone());
            state.event_connects += 1;
            (id, token)
        };
        let response = forward(&client, &upstream, request).await;
        let (parts, body) = response.into_parts();
        let body = CuttableBody {
            inner: body,
            cut: Box::pin(token.cancelled_owned()),
            registration: Some((Arc::clone(&self.state), id)),
        };
        Response::from_parts(parts, body.boxed())
    }

    fn take_service_list_failure(&self) -> bool {
        let mut state = self.state();
        if state.fail_service_list {
            state.service_list_failures += 1;
        }
        state.fail_service_list
    }

    /// Makes every service list fail with a 500, or pass again.
    pub fn set_service_list_failing(&self, failing: bool) {
        self.state().fail_service_list = failing;
    }

    pub fn service_list_failure_count(&self) -> usize {
        self.state().service_list_failures
    }

    pub fn event_connect_count(&self) -> usize {
        self.state().event_connects
    }

    /// Drops every open event stream and answers new ones with a 503 until
    /// [`DockerProxy::restore_event_streams`].
    pub fn cut_event_streams(&self) {
        let mut state = self.state();
        state.events_down = true;
        for token in state.event_streams.values() {
            token.cancel();
        }
    }

    /// Lets event streams connect again, from now on instead of from the `since` the exporter
    /// asks for, so nothing that happened during the outage is replayed.
    pub fn restore_event_streams(&self) {
        let mut state = self.state();
        state.events_down = false;
        state.forget_event_history = true;
    }
}

/// Sets the `since` query parameter, adding it if missing, as Go's `url.Values.Set` does.
fn rewrite_since(request: &mut Request<Incoming>, now: &str) {
    let path = request.uri().path().to_owned();
    let mut pairs: Vec<String> = request
        .uri()
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("since="))
        .map(str::to_owned)
        .collect();
    pairs.push(format!("since={now}"));
    pairs.sort();
    if let Ok(uri) = format!("{path}?{}", pairs.join("&")).parse::<Uri>() {
        *request.uri_mut() = uri;
    }
}

/// Go's `http.Error`: the message and a newline, as plain text.
fn error_response(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let mut response = Response::new(
        Full::new(Bytes::from(format!("{message}\n")))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The headers that describe one hop, not the message, which a proxy must not pass on.
const HOP_BY_HOP: &[header::HeaderName] = &[
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

/// Sends the request on to the manager and streams its response back; a 502 if it cannot.
async fn forward(
    client: &Client<HttpConnector, Incoming>,
    upstream: &str,
    request: Request<Incoming>,
) -> Response<ProxyBody> {
    let (mut parts, body) = request.into_parts();
    let path_and_query = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    let Ok(uri) = format!("http://{upstream}{path_and_query}").parse::<Uri>() else {
        return error_response(StatusCode::BAD_GATEWAY, "bad upstream URI");
    };
    parts.uri = uri;
    strip_hop_by_hop(&mut parts.headers);
    if let Ok(response) = client.request(Request::from_parts(parts, body)).await {
        let (mut parts, body) = response.into_parts();
        strip_hop_by_hop(&mut parts.headers);
        return Response::from_parts(
            parts,
            body.map_err(|error| ProxyError(error.to_string())).boxed(),
        );
    }
    let mut response = Response::new(
        Full::new(Bytes::new())
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response
}

/// An event stream's body that fails once its token is cancelled, and leaves the proxy's
/// register of open streams when it ends or is dropped.
struct CuttableBody {
    inner: ProxyBody,
    cut: Pin<Box<WaitForCancellationFutureOwned>>,
    registration: Option<(Arc<Mutex<State>>, u64)>,
}

impl Body for CuttableBody {
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ProxyError>>> {
        let this = self.get_mut();
        if this.cut.as_mut().poll(context).is_ready() {
            return Poll::Ready(Some(Err(cut_error())));
        }
        Pin::new(&mut this.inner).poll_frame(context)
    }
}

fn cut_error() -> ProxyError {
    ProxyError("event stream cut by the proxy".to_owned())
}

impl Drop for CuttableBody {
    fn drop(&mut self) {
        if let Some((state, id)) = self.registration.take() {
            state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .event_streams
                .remove(&id);
        }
    }
}
