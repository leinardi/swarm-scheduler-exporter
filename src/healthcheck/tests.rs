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

//! Go's 8 healthcheck tests and the healthcheck spike's, against raw TCP servers that write
//! exact status lines, and against the exporter's own server.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::time::Instant;

use super::*;

/// Serves `response` on every connection once the request head is read, then closes, or with
/// `stall` keeps the connection open without writing more. Counts the connections.
async fn raw_server(response: Vec<u8>, stall: bool) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    let response = Arc::new(response);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let response = Arc::clone(&response);
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 4096];
                let mut head = Vec::new();
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buf[..read]),
                    }
                }
                let _ = stream.write_all(&response).await;
                if stall {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            });
        }
    });
    (address, connections)
}

/// A raw server answering `status_line` with `body`.
async fn answering(status_line: &str, body: &[u8]) -> String {
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    raw_server(response, false).await.0
}

async fn probe(listen_addr: &str) -> (i32, String) {
    let mut out = Vec::new();
    let exit_code = run_healthcheck(listen_addr, &mut out).await;
    (exit_code, String::from_utf8(out).unwrap())
}

/// Go: `TestRunHealthcheck_Healthy`
#[tokio::test]
async fn a_healthy_exporter_exits_zero() {
    let address = answering("200 OK", b"ok\n").await;
    assert_eq!(probe(&address).await, (0, String::new()));
}

/// Go: `TestRunHealthcheck_UnhealthyPrintsReason`
#[tokio::test]
async fn an_unhealthy_exporter_prints_the_reason() {
    let address = answering("503 Service Unavailable", b"initial resync not completed\n").await;
    assert_eq!(
        probe(&address).await,
        (
            1,
            format!(
                "healthcheck: http://{address}/healthz: 503 Service Unavailable: initial resync \
                 not completed\n"
            )
        )
    );
}

/// Go: `TestRunHealthcheck_NothingListening`: exit 1 with the connection error (Q9: the cause is
/// hyper's wording).
#[tokio::test]
async fn nothing_listening_exits_one() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);
    let (exit_code, out) = probe(&address).await;
    assert_eq!(exit_code, 1);
    assert!(
        out.starts_with(&format!(
            "healthcheck: Get \"http://{address}/healthz\": client error (Connect): "
        )),
        "{out}"
    );
    assert!(out.contains("Connection refused"), "{out}");
}

/// Go: `TestRunHealthcheck_RedirectIsUnhealthy`: the 302 is the answer, and its healthy target is
/// never requested.
#[tokio::test]
async fn a_redirect_is_unhealthy_and_not_followed() {
    let (target, target_connections) = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok\n".to_vec(),
        false,
    )
    .await;
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{target}/healthz\r\nContent-Length: 0\r\n\r\n"
    );
    let (address, _) = raw_server(redirect.into_bytes(), false).await;
    assert_eq!(
        probe(&address).await,
        (
            1,
            format!("healthcheck: http://{address}/healthz: 302 Found\n")
        )
    );
    assert_eq!(target_connections.load(Ordering::SeqCst), 0);
}

/// Go: `TestRunHealthcheck_ReasonBodies`: an empty body, one without a newline, the first of
/// several lines, and at most 4096 bytes of an oversized one.
#[tokio::test]
async fn the_reason_is_the_trimmed_first_line_of_at_most_4096_bytes() {
    let oversized = vec![b'x'; 1 << 20];
    for (body, suffix) in [
        (b"".as_slice(), "503 Service Unavailable".to_owned()),
        (
            b"resync outstanding".as_slice(),
            "503 Service Unavailable: resync outstanding".to_owned(),
        ),
        (
            b"last poll too old\nsecond line\n".as_slice(),
            "503 Service Unavailable: last poll too old".to_owned(),
        ),
        (
            oversized.as_slice(),
            format!("503 Service Unavailable: {}", "x".repeat(4096)),
        ),
    ] {
        let address = answering("503 Service Unavailable", body).await;
        let (exit_code, out) = probe(&address).await;
        assert_eq!(exit_code, 1);
        assert_eq!(
            out,
            format!("healthcheck: http://{address}/healthz: {suffix}\n")
        );
    }
}

/// The reason is written as the bytes it was, trimmed as Go trims them, even when the limit cut
/// it inside a character.
#[tokio::test]
async fn the_reason_is_written_as_bytes() {
    let mut body = vec![b'x'; 4095];
    body.extend_from_slice("\u{e9}".as_bytes());
    let address = answering("503 Service Unavailable", &body).await;
    let mut out = Vec::new();
    assert_eq!(run_healthcheck(&address, &mut out).await, 1);
    let mut want =
        format!("healthcheck: http://{address}/healthz: 503 Service Unavailable: ").into_bytes();
    want.extend_from_slice(&body[..4096]);
    want.push(b'\n');
    assert_eq!(out, want);
}

/// The variable a child process of [`the_probe_ignores_the_proxy_variables`] runs with.
const PROXY_CHILD: &str = "SSE_HEALTHCHECK_PROXY_CHILD";

/// An address of this host that is not loopback, which Go's `ProxyFromEnvironment` would not
/// skip; loopback when the host has no route out.
fn non_loopback_ip() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    // TEST-NET-1: a UDP connect sends nothing, it only picks the outgoing address.
    if socket.connect("192.0.2.1:9").is_ok()
        && let Ok(local) = socket.local_addr()
        && !local.ip().is_unspecified()
    {
        return local.ip();
    }
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// The probe [`the_probe_ignores_the_proxy_variables`] runs in a child process with every proxy
/// variable set; run directly, without the variable it is given, it has nothing to check.
#[tokio::test]
async fn proxy_probe_in_child() {
    if std::env::var_os(PROXY_CHILD).is_none() {
        return;
    }
    let target = TcpListener::bind((non_loopback_ip(), 0)).await.unwrap();
    let address = target.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok\n")
                .await;
        }
    });
    assert_eq!(
        probe(&address).await,
        (0, String::new()),
        "probe of {address}"
    );
}

/// Go: `TestRunHealthcheck_NoProxy`: with every proxy variable pointing at a recording proxy,
/// the probe still goes straight to the exporter. The variables are set on a child process,
/// since a test cannot set its own environment safely.
#[tokio::test]
async fn the_probe_ignores_the_proxy_variables() {
    let (proxy, proxy_connections) = raw_server(
        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n".to_vec(),
        false,
    )
    .await;
    let proxy_url = format!("http://{proxy}");
    let executable = std::env::current_exe().unwrap();
    let output = tokio::task::spawn_blocking(move || {
        let mut child = std::process::Command::new(executable);
        child
            .args([
                "--exact",
                "healthcheck::tests::proxy_probe_in_child",
                "--test-threads=1",
            ])
            .env(PROXY_CHILD, "1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            child.env(name, &proxy_url);
        }
        child.output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("1 passed"), "{stdout}");
    assert_eq!(
        proxy_connections.load(Ordering::SeqCst),
        0,
        "the proxy was contacted"
    );
}

/// Go: `TestHealthcheckURL`
#[test]
fn the_url_maps_wildcards_to_loopback() {
    for (listen_addr, want) in [
        ("0.0.0.0:8888", "http://127.0.0.1:8888/healthz"),
        (":8888", "http://127.0.0.1:8888/healthz"),
        ("[::]:8888", "http://[::1]:8888/healthz"),
        ("10.0.0.5:9000", "http://10.0.0.5:9000/healthz"),
        ("[fd00::5]:9000", "http://[fd00::5]:9000/healthz"),
    ] {
        assert_eq!(healthcheck_url(listen_addr).unwrap(), want, "{listen_addr}");
    }
}

/// Go: `TestRunHealthcheck_MalformedListenAddr`, with Go's `SplitHostPort` messages.
#[tokio::test]
async fn a_malformed_listen_address_is_reported() {
    assert_eq!(
        probe("8888").await,
        (
            1,
            "healthcheck: parse -listen-addr: address 8888: missing port in address\n".to_owned()
        )
    );
    assert_eq!(
        probe("::1:8888").await,
        (
            1,
            "healthcheck: parse -listen-addr: address ::1:8888: too many colons in address\n"
                .to_owned()
        )
    );
}

/// Go's `resp.Status` is the wire text: a custom reason survives, and a status line without one
/// leaves the bare code. hyper keeps the wire reason only when it is not the canonical one.
#[tokio::test]
async fn the_status_is_the_wire_text() {
    for (status_line, status) in [
        ("503 Custom Reason", "503 Custom Reason"),
        ("599 Weird", "599 Weird"),
        ("503", "503"),
        ("418 I'm a teapot", "418 I'm a teapot"),
    ] {
        let address = answering(status_line, b"").await;
        assert_eq!(
            probe(&address).await,
            (
                1,
                format!("healthcheck: http://{address}/healthz: {status}\n")
            ),
            "{status_line}"
        );
    }
}

/// One 3s deadline covers the body too, and what was read before it is still used.
#[tokio::test]
async fn a_stalled_body_uses_what_was_read_within_3s() {
    let (address, _) = raw_server(
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 100\r\n\r\n  partial reason  "
            .to_vec(),
        true,
    )
    .await;
    let start = Instant::now();
    let result = probe(&address).await;
    let took = start.elapsed();
    assert_eq!(
        result,
        (
            1,
            format!(
                "healthcheck: http://{address}/healthz: 503 Service Unavailable: partial reason\n"
            )
        )
    );
    assert!(
        took >= Duration::from_millis(2990) && took < Duration::from_millis(3300),
        "{took:?}"
    );
}

/// Headers that never come meet the same deadline, worded as Go's.
#[tokio::test]
async fn stalled_headers_time_out_at_3s() {
    let (address, _) = raw_server(Vec::new(), true).await;
    let start = Instant::now();
    let result = probe(&address).await;
    let took = start.elapsed();
    assert_eq!(
        result,
        (
            1,
            format!("healthcheck: Get \"http://{address}/healthz\": context deadline exceeded\n")
        )
    );
    assert!(
        took >= Duration::from_millis(2990) && took < Duration::from_millis(3300),
        "{took:?}"
    );
}

/// The exporter's own server, unhealthy, answers the probe as Go's did.
#[tokio::test]
async fn the_probe_reads_the_exporter_s_answer() {
    let listener = crate::server::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let app = crate::server::App::new(
        Box::new(|| (false, "last poll too old".to_owned())),
        Box::new(|_| crate::metrics::exposition::Exposition {
            content_type: String::new(),
            content_encoding: None,
            body: Vec::new(),
        }),
        Arc::new(crate::clock::SystemClock),
        crate::test_support::capture_logs(crate::logger::Level::INFO).0,
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let shutdown = cancel.clone();
    let served = tokio::spawn(crate::server::serve(
        listener,
        Arc::new(app),
        crate::server::Timeouts::EXPORTER,
        shutdown.cancelled_owned(),
        || Instant::now() + Duration::from_secs(1),
    ));
    assert_eq!(
        probe(&address).await,
        (
            1,
            format!(
                "healthcheck: http://{address}/healthz: 503 Service Unavailable: last poll too old\n"
            )
        )
    );
    cancel.cancel();
    served.await.unwrap();
}
