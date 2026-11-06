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

//! The server's tests: Go's three health handler tests and the scrape the `/metrics` route
//! reads, the Go wire captures in [`wire`], Go's `serveUntilDone` tests and the listen errors in
//! [`shutdown`], and the regression tests of what go-http changed for the exporter: Go's service
//! names, `net.Listen`'s literals, zones and resolver, and a read deadline brought forward while a
//! read waits. The generic framing, routing, deadline and drain tests are go-http's own.

use std::collections::BTreeSet;
use std::time::Duration;

use jiff::Timestamp;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::test_support::capture_logs;
use clock::FakeClock;
use go_compat::errno::io_error_text;
use go_http::{ConnState, TimedIo};
use go_prom::exposition;
use tracing::level_filters::LevelFilter;

mod parity;
mod shutdown;
mod wire;

/// The time of the Go captures, so `Date` compares exactly.
fn capture_time() -> Timestamp {
    "2026-10-25T08:44:07Z".parse().unwrap()
}

/// An exposition of `body` in the text format, as promhttp answers a scrape with no `Accept`.
fn text_exposition(body: &'static [u8]) -> Exposition {
    Exposition {
        content_type: exposition::content_type(go_prom::ExpfmtVersion::V0_71_0, None),
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
    )
}

fn healthy_app() -> App {
    app_with(true, "", text_exposition(b"m 1\n"))
}

/// The exporter's routes plus a `/slow` one, which waits `duration` before it answers, for the
/// drain tests.
fn slow_app(duration: Duration) -> App {
    let mux = mux(
        Box::new(|| (true, String::new())),
        Box::new(|_| text_exposition(b"m 1\n")),
    )
    .route_async("/slow", move |_| async move {
        tokio::time::sleep(duration).await;
        Reply::new(
            StatusCode::OK,
            vec![(CONTENT_TYPE, HeaderValue::from_static(TEXT_PLAIN))],
            "slow done\n",
        )
    });
    App::with_mux(mux, Arc::new(FakeClock::new(capture_time())))
}

/// Sends `request` on a fresh connection of `app` and returns everything the server wrote
/// before it closed.
async fn exchange(app: App, request: &[u8]) -> Vec<u8> {
    go_http::testing::exchange(app.server, TIMEOUTS, request).await
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
                content_type: exposition::content_type(go_prom::ExpfmtVersion::V0_71_0, None),
                content_encoding: Some("gzip"),
                body: b"z".to_vec(),
            }
        }),
        Arc::new(FakeClock::new(capture_time())),
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
    );
    exchange(
        app,
        b"GET /metrics?a=b HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [MetricsRequest::default()]);
}

/// An I/O error message (listen, accept, the Docker client) quotes Go's `syscall.Errno` text for every errno Go names, not only
/// the ones the exporter's own table listed, and Go's `errno <n>` for one it does not (Linux's
/// internal 524): a full disk reads `no space left on device`, as Go printed it, not Rust's
/// `No space left on device (os error 28)`.
#[test]
fn io_error_text_quotes_go_s_whole_errno_table() {
    for (errno, want) in [
        (28, "no space left on device"),
        (3, "no such process"),
        (71, "protocol error"),
        (524, "errno 524"),
        (98, "address already in use"),
    ] {
        assert_eq!(
            io_error_text(&io::Error::from_raw_os_error(errno)),
            want,
            "errno {errno}"
        );
    }
}
