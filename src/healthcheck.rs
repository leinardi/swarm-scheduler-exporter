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

//! `--healthcheck`: probes the `/healthz` of an exporter serving on the listen address and
//! returns the exit code, 0 only for a 200. The image is distroless: there is no shell or curl a
//! `HEALTHCHECK` could run instead.
//!
//! The client is hyper-util's legacy client on its `HttpConnector`: it never reads the proxy
//! variables (a non-loopback listen address must not be probed through a proxy, and Go's
//! transport has no proxy either), it never follows a redirect (the 3xx is the answer, and not a
//! 200), and it keeps no connection. One 3s deadline covers the connect, the request and the
//! body.

use std::error::Error as _;
use std::io::Write;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::ext::ReasonPhrase;
use hyper::{Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::time::Instant;

use crate::gofmt_compat::host_port::{AddrError, join_host_port, split_host_port};
use crate::gofmt_compat::strings::trim_space_bytes;
use crate::server::HEALTHZ_PATH;
use crate::server::gopath::escape_host;

/// The deadline of a whole probe.
pub const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// How much of an unhealthy answer's body is read for its reason.
pub const HEALTHCHECK_REASON_LIMIT: usize = 4096;

/// The `/healthz` URL of an exporter serving on `listen_addr`. A wildcard host is not an address
/// to connect to, so it becomes the loopback of the same family; any other host is kept, since
/// the exporter listens there only.
///
/// # Errors
///
/// Go's `SplitHostPort` error, as `parse -listen-addr: …`.
pub fn healthcheck_url(listen_addr: &str) -> Result<String, ListenAddrError> {
    let (host, port) = split_host_port(listen_addr).map_err(ListenAddrError)?;
    let host = match host {
        "" | "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    Ok(format!(
        "http://{}{HEALTHZ_PATH}",
        escape_host(&join_host_port(host, port))
    ))
}

/// A listen address the healthcheck cannot parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenAddrError(AddrError);

impl std::fmt::Display for ListenAddrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "parse -listen-addr: {}", self.0)
    }
}

impl std::error::Error for ListenAddrError {}

/// Go's `resp.Status`: the code and the reason as they were on the wire. hyper keeps the wire
/// reason only when it differs from the canonical one, so the canonical one stands in otherwise,
/// and an empty reason leaves the bare code.
#[must_use]
pub fn go_status(status: StatusCode, reason: Option<&ReasonPhrase>) -> Vec<u8> {
    let reason = reason.map_or_else(
        || status.canonical_reason().unwrap_or("").as_bytes(),
        ReasonPhrase::as_bytes,
    );
    let mut out = status.as_str().as_bytes().to_vec();
    if !reason.is_empty() {
        out.push(b' ');
        out.extend_from_slice(reason);
    }
    out
}

/// The error and every cause under it: hyper-util's `Display` shows one level only.
fn error_chain(error: &hyper_util::client::legacy::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Probes the exporter on `listen_addr` and returns the exit code: 0 when it answers exactly
/// 200, 1 otherwise, after writing why to `out`: `healthcheck: <url>: <status>[: <reason>]`,
/// the reason being the first line of at most 4096 bytes of the body, trimmed; or
/// `healthcheck: <error>`.
pub async fn run_healthcheck(listen_addr: &str, out: &mut impl Write) -> i32 {
    let url = match healthcheck_url(listen_addr) {
        Ok(url) => url,
        Err(error) => {
            let _ = writeln!(out, "healthcheck: {error}");
            return 1;
        }
    };
    let deadline = Instant::now() + HEALTHCHECK_TIMEOUT;
    let mut connector = HttpConnector::new();
    connector.enforce_http(true);
    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build(connector);
    let request = match Request::get(url.as_str()).body(Empty::new()) {
        Ok(request) => request,
        Err(error) => {
            let _ = writeln!(out, "healthcheck: parse \"{url}\": {error}");
            return 1;
        }
    };
    let response = match tokio::time::timeout_at(deadline, client.request(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            let _ = writeln!(out, "healthcheck: Get \"{url}\": {}", error_chain(&error));
            return 1;
        }
        Err(_) => {
            let _ = writeln!(out, "healthcheck: Get \"{url}\": context deadline exceeded");
            return 1;
        }
    };
    if response.status() == StatusCode::OK {
        return 0;
    }
    let status = go_status(
        response.status(),
        response.extensions().get::<ReasonPhrase>(),
    );
    // io.ReadAll(io.LimitReader(body, 4096)) under the same deadline: what could be read before
    // an error is still used.
    let mut body = response.into_body();
    let mut read = Vec::new();
    while read.len() < HEALTHCHECK_REASON_LIMIT {
        let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, body.frame()).await else {
            break;
        };
        if let Some(data) = frame.data_ref() {
            let room = HEALTHCHECK_REASON_LIMIT - read.len();
            read.extend_from_slice(&data[..data.len().min(room)]);
        }
    }
    let first_line = read.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let reason = trim_space_bytes(first_line);
    let mut line = format!("healthcheck: {url}: ").into_bytes();
    line.extend_from_slice(&status);
    if !reason.is_empty() {
        line.extend_from_slice(b": ");
        line.extend_from_slice(reason);
    }
    line.push(b'\n');
    let _ = out.write_all(&line);
    1
}

#[cfg(test)]
mod tests;
