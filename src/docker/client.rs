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

//! The client: settings from the environment, the request builder, the ping and the lazy API
//! version negotiation, and moby's error wording.

use std::io::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use flate2::write::GzDecoder;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::body::{Frame, SizeHint};
use hyper::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::header::{HOST, HeaderValue, USER_AGENT};
use hyper::{Method, Request, Response, StatusCode};
use rustls::ClientConfig;

use super::connect::{ConnectError, Connector};
use super::error::{ConfigError, Error};
use super::host::{Endpoint, Host, escape_host, escape_path, join_path, remove_zone};
use super::pool::{Pool, PooledBody, SendError};
use super::version::{self, MAX_API_VERSION, parse_api_version};
use super::{DEFAULT_DOCKER_HOST, DUMMY_HOST, tls};
use crate::go_compat::errno::io_error_text;
use crate::go_compat::http_status::status_text;
use crate::go_compat::quote::quote;

/// moby's `checkResponseErr` reads at most this much of an error body.
const ERROR_BODY_LIMIT: usize = 1024 * 1024;

/// The client's settings, read from the environment.
#[derive(Debug, Clone)]
pub struct Settings {
    host: String,
    parsed: Host,
    endpoint: Endpoint,
    tls: Option<Arc<ClientConfig>>,
    version: String,
    pinned: bool,
}

impl Settings {
    /// The Go exporter's `newDockerClient`, with `getenv` for `os.Getenv` (unset and empty are
    /// the same): TLS from `DOCKER_CERT_PATH` and `DOCKER_TLS_VERIFY` (Q5: an empty
    /// `DOCKER_TLS_VERIFY` is refused), then the host, [`DEFAULT_DOCKER_HOST`] unless
    /// `DOCKER_HOST` sets one, then `DOCKER_API_VERSION`, trimmed and without a `v` prefix, which
    /// pins the version and skips negotiation. The range of a pinned version is checked by
    /// [`version::validate_client_api_version`], as the Go exporter checks it after this.
    ///
    /// # Errors
    ///
    /// [`ConfigError`]: `configure TLS from DOCKER_CERT_PATH=<path>: …` for the TLS material, and
    /// `docker client: …` for a host or API version moby refuses.
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| getenv(name).filter(|value| !value.is_empty());
        let tls = match get("DOCKER_CERT_PATH") {
            Some(cert_path) => Some(Arc::new(tls::client_config(
                &cert_path,
                get("DOCKER_TLS_VERIFY").is_some(),
            )?)),
            None => None,
        };
        let host = get("DOCKER_HOST").unwrap_or_else(|| DEFAULT_DOCKER_HOST.to_owned());
        let docker_client = |message: String| ConfigError::new(format!("docker client: {message}"));
        let parsed = Host::parse(&host).map_err(docker_client)?;
        let endpoint = parsed.endpoint().map_err(docker_client)?;
        let raw_version = getenv("DOCKER_API_VERSION").unwrap_or_default();
        let raw_version = raw_version.trim();
        let pin = raw_version.strip_prefix('v').unwrap_or(raw_version);
        let (version, pinned) = if pin.is_empty() {
            (MAX_API_VERSION.to_owned(), false)
        } else {
            let version = parse_api_version(pin).map_err(|reason| {
                docker_client(format!("invalid API version ({raw_version}): {reason}"))
            })?;
            (version, true)
        };
        Ok(Self {
            host,
            parsed,
            endpoint,
            tls,
            version,
            pinned,
        })
    }

    /// moby's `DaemonHost`: `DOCKER_HOST`, or the default.
    #[must_use]
    pub fn daemon_host(&self) -> &str {
        &self.host
    }

    /// moby's `ClientVersion` before any request: the pinned version, or [`MAX_API_VERSION`].
    #[must_use]
    pub fn client_version(&self) -> &str {
        &self.version
    }
}

/// The negotiated API version, shared by every request.
#[derive(Debug)]
struct Negotiation {
    state: tokio::sync::Mutex<NegotiationState>,
    // Bumped after every ping, so a request that waited for one shares its outcome.
    pings: AtomicU64,
}

#[derive(Debug)]
struct NegotiationState {
    version: String,
    negotiated: bool,
}

/// The read-only Docker client.
#[derive(Debug)]
pub struct Client {
    settings: Settings,
    pool: Arc<Pool>,
    scheme: &'static str,
    host_header: HeaderValue,
    user_agent: HeaderValue,
    gzip: bool,
    negotiation: Negotiation,
}

/// A response body: read it to the end and the connection is reused. On TCP a gzip body is
/// decompressed as Go's transport does when it asked for gzip itself.
#[derive(Debug)]
pub struct Body {
    inner: PooledBody,
    gzip: Option<GzDecoder<Vec<u8>>>,
}

impl hyper::body::Body for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        loop {
            let frame = match Pin::new(&mut self.inner).poll_frame(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(frame) => frame,
            };
            let Some(decoder) = self.gzip.as_mut() else {
                return Poll::Ready(frame.map(|frame| frame.map_err(body_error)));
            };
            match frame {
                None => {
                    let finished = decoder
                        .try_finish()
                        .map(|()| std::mem::take(decoder.get_mut()));
                    self.gzip = None;
                    return Poll::Ready(match finished {
                        Ok(rest) if rest.is_empty() => None,
                        Ok(rest) => Some(Ok(Frame::data(Bytes::from(rest)))),
                        Err(error) => {
                            Some(Err(Error::body(format!("gzip: {error}")).with_source(error)))
                        }
                    });
                }
                Some(Err(error)) => return Poll::Ready(Some(Err(body_error(error)))),
                Some(Ok(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    if let Err(error) = decoder.write_all(&data) {
                        return Poll::Ready(Some(Err(
                            Error::body(format!("gzip: {error}")).with_source(error)
                        )));
                    }
                    let output = std::mem::take(decoder.get_mut());
                    if !output.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(Bytes::from(output)))));
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.gzip.is_none() && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        if self.gzip.is_some() {
            SizeHint::default()
        } else {
            self.inner.size_hint()
        }
    }
}

fn body_error(error: hyper::Error) -> Error {
    Error::body(http_error_text(&error)).with_source(error)
}

impl Client {
    /// A client for `settings`, with no connection open yet.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self::with_pool(settings, Pool::new)
    }

    pub(super) fn with_pool(settings: Settings, pool: impl FnOnce(Connector) -> Arc<Pool>) -> Self {
        let connector = Connector::new(settings.endpoint.clone(), settings.tls.clone());
        let host_header = match &settings.endpoint {
            Endpoint::Unix(_) => DUMMY_HOST.to_owned(),
            Endpoint::Tcp(addr) => remove_zone(addr),
        };
        let negotiation = Negotiation {
            state: tokio::sync::Mutex::new(NegotiationState {
                version: settings.version.clone(),
                negotiated: settings.pinned,
            }),
            pings: AtomicU64::new(0),
        };
        Self {
            pool: pool(connector),
            scheme: if settings.tls.is_some() {
                "https"
            } else {
                "http"
            },
            host_header: HeaderValue::from_str(&host_header)
                .unwrap_or_else(|_| HeaderValue::from_static(DUMMY_HOST)),
            user_agent: HeaderValue::from_str(&format!(
                "swarm-scheduler-exporter/{}",
                crate::version::VERSION
            ))
            .unwrap_or_else(|_| HeaderValue::from_static("swarm-scheduler-exporter")),
            // go-connections leaves compression on for every protocol but unix sockets.
            gzip: matches!(settings.endpoint, Endpoint::Tcp(_)),
            negotiation,
            settings,
        }
    }

    /// The settings the client was built from.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The API version requests use now: the pinned or negotiated one, else [`MAX_API_VERSION`].
    pub async fn client_version(&self) -> String {
        self.negotiation.state.lock().await.version.clone()
    }

    /// The one request builder: `GET` with Go's headers in Go's order (`Host`, `User-Agent`,
    /// `Accept`, then on TCP the transport's `Accept-Encoding: gzip`). Only [`Client::ping`]
    /// changes the method, to `HEAD`.
    fn request(&self, target: &str, accept: &[&'static str]) -> Request<Empty<Bytes>> {
        let mut request = Request::new(Empty::new());
        *request.method_mut() = Method::GET;
        *request.uri_mut() = target
            .parse()
            .unwrap_or_else(|_| hyper::Uri::from_static("/"));
        let headers = request.headers_mut();
        headers.insert(HOST, self.host_header.clone());
        headers.insert(USER_AGENT, self.user_agent.clone());
        for value in accept {
            headers.append(ACCEPT, HeaderValue::from_static(value));
        }
        if self.gzip {
            headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        }
        request
    }

    fn url(&self, target: &str) -> String {
        format!(
            "{}://{}{target}",
            self.scheme,
            escape_host(&self.settings.parsed.addr)
        )
    }

    /// Sends the request `build` makes (again, if a reused connection fails first), mapping a
    /// failure to moby's wording, and sets up the body's decompression when Go's transport
    /// asked for gzip itself.
    async fn send(
        &self,
        target: &str,
        build: impl Fn() -> Request<Empty<Bytes>>,
    ) -> Result<Response<Body>, Error> {
        let probe = build();
        // net/http's url.Error names the method this way: "Get", "Head".
        let method = probe.method().as_str();
        let operation = format!("{}{}", &method[..1], method[1..].to_ascii_lowercase());
        let asked_gzip = probe.headers().contains_key(ACCEPT_ENCODING);
        drop(probe);
        let response = self.pool.send(build).await.map_err(|failure| {
            self.connection_error(&operation, &self.url(target), &failure)
                .with_source(failure)
        })?;
        let (mut parts, inner) = response.into_parts();
        let gzipped = asked_gzip
            && parts
                .headers
                .get(CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("gzip"));
        if gzipped {
            parts.headers.remove(CONTENT_ENCODING);
            parts.headers.remove(CONTENT_LENGTH);
        }
        let body = Body {
            inner,
            gzip: gzipped.then(|| GzDecoder::new(Vec::new())),
        };
        Ok(Response::from_parts(parts, body))
    }

    /// moby's `ping`: `HEAD /_ping`, used as is on a 200; on a failure or any other status,
    /// `GET /_ping`, whose failure fails the ping and whose status of 400 or above is moby's
    /// response error. Returns the `Api-Version` header, empty when absent.
    async fn ping(&self) -> Result<String, Error> {
        let target = escape_path(&join_path(&[&self.settings.parsed.base_path, "/_ping"]));
        // The only HEAD the client sends; Go's transport asks no gzip for a HEAD.
        let head = || {
            let mut request = self.request(&target, &[]);
            *request.method_mut() = Method::HEAD;
            request.headers_mut().remove(ACCEPT_ENCODING);
            request
        };
        if let Ok(response) = self.send(&target, head).await {
            let status = response.status();
            let api_version = api_version_header(&response);
            // Read to the end, so the connection is reused, as moby's ensureReaderClosed does.
            let _ = response.into_body().collect().await;
            if status == StatusCode::OK {
                return Ok(api_version);
            }
        }
        let response = self.send(&target, || self.request(&target, &[])).await?;
        let api_version = api_version_header(&response);
        self.check_response(response, "GET", &self.url(&target))
            .await?;
        Ok(api_version)
    }

    /// moby's `checkVersion`: negotiates once, and again on the next request after a failure,
    /// which moby discards (`_ = cli.checkVersion(ctx)`): the request then goes out at the
    /// current version, [`MAX_API_VERSION`] until a negotiation succeeds. Concurrent requests
    /// share one ping and its outcome (Q11; moby sends one each).
    async fn api_version(&self) -> String {
        let pings = self.negotiation.pings.load(Ordering::Acquire);
        let mut state = self.negotiation.state.lock().await;
        if state.negotiated || self.negotiation.pings.load(Ordering::Acquire) != pings {
            return state.version.clone();
        }
        if let Ok(ping_version) = self.ping().await
            && let Ok(negotiated) = version::negotiate(&ping_version)
        {
            state.version = negotiated;
            state.negotiated = true;
        }
        self.negotiation.pings.fetch_add(1, Ordering::Release);
        state.version.clone()
    }

    /// A versioned `GET` of `path` (with `query`, already encoded, and `accept` header lines):
    /// the response once its headers are in, or moby's error for a status of 400 or above.
    ///
    /// # Errors
    ///
    /// [`Error`] for no response, as moby words it, or a response error.
    pub async fn get(
        &self,
        path: &str,
        query: &str,
        accept: &[&'static str],
    ) -> Result<Response<Body>, Error> {
        let version = self.api_version().await;
        let api_path = join_path(&[
            &self.settings.parsed.base_path,
            &format!("/v{}", version.strip_prefix('v').unwrap_or(&version)),
            path,
        ]);
        let mut target = escape_path(&api_path);
        if !query.is_empty() {
            target.push('?');
            target.push_str(query);
        }
        let response = self.send(&target, || self.request(&target, accept)).await?;
        if response.status().as_u16() < 400 {
            return Ok(response);
        }
        let url = self.url(&target);
        Err(self
            .check_response(response, "GET", &url)
            .await
            .err()
            .unwrap_or_else(|| Error::response(StatusCode::INTERNAL_SERVER_ERROR, String::new())))
    }

    /// moby's `checkResponseErr`: a status from 200 to 399 is fine; otherwise the error carries
    /// the daemon's message (the JSON `message` for `application/json`, else the body), or says
    /// it is missing or too large.
    async fn check_response(
        &self,
        response: Response<Body>,
        method: &str,
        url: &str,
    ) -> Result<(), Error> {
        let status = response.status();
        if (200..400).contains(&status.as_u16()) {
            return Ok(());
        }
        let status_message = go_status(&response);
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = BytesMut::new();
        if method != "HEAD" {
            let mut limited = Limited::new(response.into_body(), ERROR_BODY_LIMIT);
            while let Some(frame) = limited.frame().await {
                match frame {
                    Ok(frame) => {
                        if let Ok(data) = frame.into_data() {
                            body.extend_from_slice(&data);
                        }
                    }
                    Err(error) if error.is::<http_body_util::LengthLimitError>() => {
                        return Err(Error::response(
                            status,
                            format!(
                                "request returned {status_message} with a message (> \
                                 {ERROR_BODY_LIMIT} bytes) for API route and version {url}, \
                                 check if the server supports the requested API version"
                            ),
                        )
                        .with_source(error));
                    }
                    Err(error) => {
                        return Err(Error::response(status, error.to_string()).with_source(error));
                    }
                }
            }
        }
        if body.len() >= ERROR_BODY_LIMIT {
            return Err(Error::response(
                status,
                format!(
                    "request returned {status_message} with a message (> {ERROR_BODY_LIMIT} \
                     bytes) for API route and version {url}, check if the server supports the \
                     requested API version"
                ),
            ));
        }
        if body.is_empty() {
            return Err(Error::response(
                status,
                format!(
                    "request returned {status_message} for API route and version {url}, check if \
                     the server supports the requested API version"
                ),
            ));
        }
        let daemon_message = if content_type.as_deref() == Some("application/json") {
            match serde_json::from_slice::<ErrorResponse>(&body) {
                Err(error) => {
                    return Err(
                        Error::response(status, format!("error reading JSON: {error}"))
                            .with_source(error),
                    );
                }
                Ok(ErrorResponse { message })
                    if message.as_deref().unwrap_or_default().is_empty() =>
                {
                    format!(
                        "API returned a {} ({}) but provided no error-message",
                        status.as_u16(),
                        status_text(status.as_u16())
                    )
                }
                Ok(ErrorResponse { message }) => message.unwrap_or_default().trim().to_owned(),
            }
        } else {
            String::from_utf8_lossy(&body).trim().to_owned()
        };
        Err(Error::response(
            status,
            format!("Error response from daemon: {daemon_message}"),
        ))
    }

    /// moby's `doRequest` wording of a request that got no response.
    fn connection_error(&self, operation: &str, url: &str, failure: &SendError) -> Error {
        let host = self.settings.daemon_host();
        let inner = match failure {
            SendError::Connect(error) => error.to_string(),
            SendError::HeaderTimeout => "net/http: timeout awaiting response headers".to_owned(),
            SendError::Http(error) => http_error_text(error),
        };
        let url_error = format!("{operation} {}: {inner}", quote(url));
        if self.scheme != "https" && matches!(failure, SendError::Http(error) if error.is_parse()) {
            return Error::connection_failed(format!(
                "{url_error}.\n* Are you trying to connect to a TLS-enabled daemon without TLS?"
            ));
        }
        if let SendError::Connect(error) = failure {
            if self.scheme == "https"
                && matches!(
                    error.tls_error(),
                    Some(rustls::Error::AlertReceived(
                        rustls::AlertDescription::HandshakeFailure
                            | rustls::AlertDescription::BadCertificate
                    ))
                )
            {
                return Error::connection_failed(format!(
                    "the server probably has client authentication (--tlsverify) enabled; check \
                     your TLS client certification settings: {url_error}"
                ));
            }
            if let Some(dial) = error.dial_error() {
                match dial.raw_os_error() {
                    Some(1 | 13) => {
                        return Error::connection_failed(format!(
                            "permission denied while trying to connect to the docker API at {host}"
                        ));
                    }
                    Some(2) => {
                        return Error::connection_failed(format!(
                            "failed to connect to the docker API at {host}; check if the path is \
                             correct and if the daemon is running: {inner}"
                        ));
                    }
                    _ => {}
                }
            }
            if let ConnectError::Lookup { .. } = error {
                let lookup = inner.strip_prefix("dial tcp: ").unwrap_or(&inner);
                return Error::connection_failed(format!(
                    "failed to connect to the docker API at {host}: {lookup}"
                ));
            }
        }
        let timeout = match failure {
            SendError::Connect(error) => error.is_timeout(),
            SendError::HeaderTimeout => true,
            SendError::Http(_) => false,
        };
        if timeout || inner.contains("connection refused") || inner.contains("dial unix") {
            return Error::connection_failed(format!(
                "Cannot connect to the Docker daemon at {host}. Is the docker daemon running?"
            ));
        }
        Error::connection_failed(format!("error during connect: {url_error}"))
    }
}

/// The text of a failed exchange, as close to Go's as hyper allows: a connection closed before a
/// response is Go's `EOF`, a malformed response is net/http's, and an I/O error is its errno text.
fn http_error_text(error: &hyper::Error) -> String {
    if error.is_parse() {
        return "net/http: HTTP/1.x transport connection broken: malformed HTTP response"
            .to_owned();
    }
    if error.is_incomplete_message() || error.is_closed() || error.is_canceled() {
        return "EOF".to_owned();
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_error_text(io);
        }
        source = cause.source();
    }
    error.to_string()
}

/// The `Api-Version` header, as Go's `Header.Get` reads it: the first value, empty when absent.
fn api_version_header<B>(response: &Response<B>) -> String {
    response
        .headers()
        .get("Api-Version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Go's `resp.Status`: the code and the reason from the wire, which hyper keeps only when it is
/// not the canonical one.
fn go_status<B>(response: &Response<B>) -> String {
    let code = response.status().as_u16();
    let reason = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
        .or_else(|| response.status().canonical_reason().map(str::to_owned))
        .unwrap_or_default();
    if reason.is_empty() {
        code.to_string()
    } else {
        format!("{code} {reason}")
    }
}

/// moby's `common.ErrorResponse`.
#[derive(Debug, serde::Deserialize)]
struct ErrorResponse {
    // A missing or null message is Go's empty string.
    #[serde(default)]
    message: Option<String>,
}
