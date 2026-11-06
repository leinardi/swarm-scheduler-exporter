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

//! The server's tests, over real sockets: the health answers, the metrics answer, axum's 404, the
//! header-read timeout, the listen errors, and the stop and drain sequence in [`shutdown`].

use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::*;

mod shutdown;

fn exposition(body: &'static str) -> Exposition {
    Exposition {
        content_type: "application/openmetrics-text; version=1.0.0; charset=utf-8".to_owned(),
        body: body.as_bytes().to_vec(),
    }
}

/// The exporter's routes, answering health with `healthy` and `reason`.
fn app_with(healthy: bool, reason: &'static str, metrics: Exposition) -> Router {
    router(
        Box::new(move || (healthy, reason.to_owned())),
        Box::new(move || metrics.clone()),
    )
}

fn healthy_app() -> Router {
    app_with(true, "", exposition("# EOF\n"))
}

/// A server on a free loopback port, and what stops it.
struct Running {
    address: SocketAddr,
    lifecycle: Lifecycle,
    outcome: tokio::task::JoinHandle<Outcome>,
}

impl Running {
    async fn start(router: Router, drain: Duration) -> Self {
        let listener = bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let lifecycle = Lifecycle::new();
        let outcome = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move { serve(listener, router, &lifecycle, drain).await })
        };
        Self {
            address,
            lifecycle,
            outcome,
        }
    }

    async fn stop(self) -> Outcome {
        self.lifecycle.stop();
        self.outcome.await.unwrap()
    }
}

/// Sends `request` on a fresh connection and returns everything the server wrote before it
/// closed.
async fn exchange(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.unwrap();
    String::from_utf8(answer).unwrap()
}

/// A `GET` of `path` with `Connection: close`: the head and the body.
async fn get_path(address: SocketAddr, path: &str) -> (String, String) {
    let request = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    let response = exchange(address, &request).await;
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (head.to_owned(), body.to_owned())
}

/// Go: `TestHealthHandler_Healthy`
#[tokio::test]
async fn health_handler_healthy() {
    let server = Running::start(healthy_app(), Duration::from_secs(1)).await;
    let (head, body) = get_path(server.address, "/healthz").await;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(
        head.to_lowercase()
            .contains("content-type: text/plain; charset=utf-8"),
        "{head}"
    );
    assert_eq!(body, "ok\n");
    assert!(matches!(server.stop().await, Outcome::Drained));
}

/// Go: `TestHealthHandler_Unhealthy_WithReason`
#[tokio::test]
async fn health_handler_unhealthy_with_reason() {
    let app = app_with(false, "last poll too old", exposition("# EOF\n"));
    let server = Running::start(app, Duration::from_secs(1)).await;
    let (head, body) = get_path(server.address, "/healthz").await;
    assert!(
        head.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "{head}"
    );
    assert!(
        head.to_lowercase()
            .contains("content-type: text/plain; charset=utf-8"),
        "{head}"
    );
    assert_eq!(body, "last poll too old\n");
    server.stop().await;
}

/// Go: `TestHealthHandler_Unhealthy_EmptyReason`
#[tokio::test]
async fn health_handler_unhealthy_empty_reason() {
    let app = app_with(false, "", exposition("# EOF\n"));
    let server = Running::start(app, Duration::from_secs(1)).await;
    let (head, body) = get_path(server.address, "/healthz").await;
    assert!(
        head.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "{head}"
    );
    assert_eq!(body, "unhealthy\n");
    server.stop().await;
}

/// `/metrics` answers the exposition with its own `Content-Type`.
#[tokio::test]
async fn metrics_answers_the_exposition() {
    let app = app_with(true, "", exposition("m 1\n# EOF\n"));
    let server = Running::start(app, Duration::from_secs(1)).await;
    let (head, body) = get_path(server.address, "/metrics").await;
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(
        head.to_lowercase()
            .contains("content-type: application/openmetrics-text; version=1.0.0; charset=utf-8"),
        "{head}"
    );
    assert!(!head.to_lowercase().contains("content-encoding"), "{head}");
    assert_eq!(body, "m 1\n# EOF\n");
    server.stop().await;
}

/// Any other path is a 404, whatever its shape.
#[tokio::test]
async fn an_unknown_path_is_a_404() {
    let server = Running::start(healthy_app(), Duration::from_secs(1)).await;
    for path in [
        "/",
        "/metrics/",
        "/healthz/x",
        "/nothing",
        "/%2e%2e/metrics",
    ] {
        let (head, _) = get_path(server.address, path).await;
        assert!(
            head.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{path}: {head}"
        );
    }
    server.stop().await;
}

/// A client that never finishes its request head is cut at the header-read timeout, the 5 s the
/// Go exporter had.
#[tokio::test(start_paused = true)]
async fn a_slow_header_is_cut_at_five_seconds() {
    let server = Running::start(healthy_app(), Duration::from_secs(1)).await;
    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let start = tokio::time::Instant::now();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.unwrap();
    let waited = start.elapsed();
    assert!(
        waited >= HEADER_READ_TIMEOUT && waited < HEADER_READ_TIMEOUT + Duration::from_secs(1),
        "{waited:?}"
    );
    assert!(
        !String::from_utf8_lossy(&answer).contains("200 OK"),
        "{answer:?}"
    );
    server.stop().await;
}

/// A busy port, a missing port and a bad port fail wrapped `http listen: `.
#[tokio::test]
async fn a_bind_failure_is_reported_with_the_prefix() {
    let busy = bind("127.0.0.1:0").await.unwrap();
    let busy_address = busy.local_addr().unwrap();
    let error = bind(&busy_address.to_string())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with(&format!("http listen: listen tcp {busy_address}: "))
            && error.contains("Address already in use"),
        "{error}"
    );
    assert_eq!(
        bind("127.0.0.1").await.unwrap_err().to_string(),
        "http listen: listen tcp: address 127.0.0.1: missing port in address"
    );
    assert!(
        bind("127.0.0.1:70000")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("http listen: listen tcp: address 70000")
    );
}

/// An empty host listens on every interface, as Go's `:port` does.
#[tokio::test]
async fn an_empty_host_is_the_wildcard() {
    let listener = bind(":0").await.unwrap();
    assert!(listener.local_addr().unwrap().ip().is_unspecified());
    let loopback = SocketAddr::new(
        Ipv4Addr::LOCALHOST.into(),
        listener.local_addr().unwrap().port(),
    );
    assert!(
        TcpStream::connect(loopback).await.is_ok(),
        "IPv4 reaches the wildcard"
    );
}

/// The wildcard socket is dual-stack on its own: `IPV6_V6ONLY` is off, so a host with
/// `net.ipv6.bindv6only=1` still takes IPv4 clients. Skipped where the wildcard fell back to IPv4.
#[tokio::test]
async fn the_wildcard_socket_is_not_v6_only() {
    let listener = bind("[::]:0").await.unwrap();
    if listener.local_addr().unwrap().is_ipv4() {
        return;
    }
    assert!(!socket2::SockRef::from(&listener).only_v6().unwrap());
}

/// The zone of a scoped listen address reaches the socket address as its scope id: the interface
/// name and the numeric index name the same one, and the loopback's own index is read from the
/// kernel, so the test needs no particular host.
#[tokio::test]
async fn a_zone_becomes_the_scope_id() {
    let Ok(index) = std::fs::read_to_string("/sys/class/net/lo/ifindex") else {
        return;
    };
    let index: u32 = index.trim().parse().unwrap();
    let link_local: Ipv6Addr = "fe80::1".parse().unwrap();
    for host in ["fe80::1%lo".to_owned(), format!("fe80::1%{index}")] {
        let found = resolve(&host, 8888).await.unwrap();
        let [SocketAddr::V6(address)] = found.as_slice() else {
            panic!("{host}: {found:?}");
        };
        assert_eq!(address.scope_id(), index, "{host}");
        assert_eq!((address.ip(), address.port()), (&link_local, 8888));
    }
}

/// An interface that does not exist is a lookup error naming the address, not a bind of the
/// unscoped one.
#[tokio::test]
async fn an_unknown_zone_is_a_clear_error() {
    let error = bind("[fe80::1%nosuchif0]:0").await.unwrap_err().to_string();
    assert!(
        error.starts_with("http listen: listen tcp: lookup fe80::1%nosuchif0: "),
        "{error}"
    );
}
