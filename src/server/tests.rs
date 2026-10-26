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

//! The server's tests: Go's three health handler tests and the framing rules here, the Go wire
//! captures in [`wire`], the deadlines in [`timeouts`], and the listener, the drain and Go's
//! `serveUntilDone` tests in [`shutdown`].

use std::time::Duration;

use bytes::Bytes;
use jiff::Timestamp;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};

use super::*;
use crate::clock::FakeClock;
use crate::logger::Level;
use crate::metrics::exposition;
use crate::test_support::capture_logs;

mod shutdown;
mod timeouts;
mod wire;

/// The time of the Go captures, so `Date` compares exactly.
fn capture_time() -> Timestamp {
    "2026-10-25T08:44:07Z".parse().unwrap()
}

/// An exposition of `body` in the text format, as promhttp answers a scrape with no `Accept`.
fn text_exposition(body: &'static [u8]) -> Exposition {
    Exposition {
        content_type: exposition::content_type(None),
        content_encoding: None,
        body: body.to_vec(),
    }
}

/// An app answering health with `healthy` and `reason`, and every scrape with `metrics`.
fn app_with(healthy: bool, reason: &'static str, metrics: Exposition) -> App {
    App::new(
        Box::new(move || (healthy, reason.to_owned())),
        Box::new(move |_| metrics.clone()),
        Arc::new(FakeClock::new(capture_time())),
        capture_logs(Level::INFO).0,
    )
}

fn healthy_app() -> App {
    app_with(true, "", text_exposition(b"m 1\n"))
}

/// Serves one connection of `app` over a duplex and returns the client end.
fn connect(app: App) -> DuplexStream {
    let (client, server_io) = tokio::io::duplex(1 << 20);
    let connection = connection(server_io, Arc::new(app), Timeouts::EXPORTER);
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Sends `request` on a fresh connection of `app` and returns everything the server wrote
/// before it closed.
async fn exchange(app: App, request: &[u8]) -> Vec<u8> {
    let mut client = connect(app);
    client.write_all(request).await.unwrap();
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response).await;
    response
}

async fn exchange_text(app: App, request: &str) -> String {
    String::from_utf8(exchange(app, request.as_bytes()).await).unwrap()
}

/// Go: `TestHealthHandler_Healthy`
#[tokio::test(start_paused = true)]
async fn health_handler_healthy() {
    let response = exchange_text(
        healthy_app(),
        "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nDate: Sun, 25 Oct 2026 \
         08:44:07 GMT\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n"
    );
}

/// Go: `TestHealthHandler_Unhealthy_WithReason`
#[tokio::test(start_paused = true)]
async fn health_handler_unhealthy_with_reason() {
    let app = app_with(false, "last poll too old", text_exposition(b""));
    let response = exchange_text(app, "GET /healthz HTTP/1.0\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.0 503 Service Unavailable\r\n"),
        "{response}"
    );
    assert!(
        response.ends_with("\r\n\r\nlast poll too old\n"),
        "{response}"
    );
}

/// Go: `TestHealthHandler_Unhealthy_EmptyReason`
#[tokio::test(start_paused = true)]
async fn health_handler_unhealthy_empty_reason() {
    let app = app_with(false, "", text_exposition(b""));
    let response = exchange_text(app, "GET /healthz HTTP/1.0\r\n\r\n").await;
    assert!(
        response.starts_with("HTTP/1.0 503 Service Unavailable\r\n"),
        "{response}"
    );
    assert!(response.ends_with("\r\n\r\nunhealthy\n"), "{response}");
}

/// Go's framing: up to 2 KiB a `Content-Length`, above it chunks of 2 KiB.
#[tokio::test(start_paused = true)]
async fn bodies_above_2_kib_are_chunked() {
    let exact = vec![b'x'; 2048].leak();
    let response = exchange(
        app_with(true, "", text_exposition(exact)),
        b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("\r\nContent-Length: 2048\r\n"), "{text}");
    assert!(!text.contains("Transfer-Encoding"), "{text}");

    let over = vec![b'x'; 4097].leak();
    let response = exchange(
        app_with(true, "", text_exposition(over)),
        b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(
        head.ends_with("\r\nConnection: close\r\nTransfer-Encoding: chunked"),
        "{head}"
    );
    assert!(!head.contains("Content-Length"), "{head}");
    let x2048 = "x".repeat(2048);
    assert_eq!(
        body,
        format!("800\r\n{x2048}\r\n800\r\n{x2048}\r\n1\r\nx\r\n0\r\n\r\n")
    );

    // HEAD: neither header, as Go writes it.
    let response = exchange(
        app_with(true, "", text_exposition(over)),
        b"HEAD /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let text = String::from_utf8_lossy(&response);
    assert!(!text.contains("Content-Length"), "{text}");
    assert!(!text.contains("Transfer-Encoding"), "{text}");
    assert!(text.ends_with("\r\n\r\n"), "{text}");
}

/// promhttp reads the first `Accept`, every `Accept-Encoding` and the `name[]` filter; a gzip
/// answer carries its `Content-Encoding` ahead of the `Content-Type`.
#[tokio::test(start_paused = true)]
async fn the_scrape_request_reaches_the_metrics_function() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let app = App::new(
        Box::new(|| (true, String::new())),
        Box::new(move |scrape: &MetricsRequest| {
            recorded.lock().unwrap().push(scrape.clone());
            Exposition {
                content_type: exposition::content_type(None),
                content_encoding: Some("gzip"),
                body: b"z".to_vec(),
            }
        }),
        Arc::new(FakeClock::new(capture_time())),
        capture_logs(Level::INFO).0,
    );
    let response = exchange_text(
        app,
        "GET /metrics?name[]=up&name%5B%5D=x&other=1 HTTP/1.1\r\nHost: x\r\nAccept: text/plain\r\n\
         Accept: application/json\r\nAccept-Encoding: gzip\r\nAccept-Encoding: br\r\n\
         Connection: close\r\n\r\n",
    )
    .await;
    assert!(
        response.starts_with(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Type: text/plain; \
             version=0.0.4; charset=utf-8; escaping=underscores\r\nDate: "
        ),
        "{response}"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        [MetricsRequest {
            accept: Some("text/plain".to_owned()),
            accept_encoding: vec!["gzip".to_owned(), "br".to_owned()],
            names: Some(BTreeSet::from(["up".to_owned(), "x".to_owned()])),
        }]
    );
}

/// Without `name[]`, no filter.
#[tokio::test(start_paused = true)]
async fn a_scrape_without_names_has_no_filter() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let app = App::new(
        Box::new(|| (true, String::new())),
        Box::new(move |scrape: &MetricsRequest| {
            recorded.lock().unwrap().push(scrape.clone());
            text_exposition(b"")
        }),
        Arc::new(FakeClock::new(capture_time())),
        capture_logs(Level::INFO).0,
    );
    exchange(
        app,
        b"GET /metrics?a=b HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [MetricsRequest::default()]);
}

/// Go's `LookupPort` answers, from a Go program.
#[test]
fn ports_parse_as_go_s() {
    assert_eq!(parse_port("8888"), Ok(8888));
    assert_eq!(parse_port("+80"), Ok(80));
    assert_eq!(
        parse_port("70000"),
        Err("address 70000: invalid port".to_owned())
    );
    assert_eq!(parse_port("-1"), Err("address -1: invalid port".to_owned()));
    assert_eq!(
        parse_port("http"),
        Err("lookup tcp/http: unknown port".to_owned())
    );
    assert_eq!(parse_port(""), Ok(0));
    assert_eq!(parse_port("+"), Ok(0));
    assert_eq!(parse_port("-0"), Ok(0));
    assert_eq!(parse_port("08"), Ok(8));
    assert_eq!(
        parse_port("0x10"),
        Err("lookup tcp/0x10: unknown port".to_owned())
    );
}

/// A slow route for the deadline and drain tests: it waits `duration`.
fn sleeping(duration: Duration) -> SlowRoute {
    Arc::new(move || Box::pin(tokio::time::sleep(duration)))
}

fn slow_app(duration: Duration) -> App {
    let mut app = healthy_app();
    app.slow = Some(sleeping(duration));
    app
}
