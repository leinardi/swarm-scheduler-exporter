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

//! Go's server deadlines (5s header, 10s read, 15s write, 60s idle) on a paused clock over a
//! duplex. Each expectation is Go 1.26's wall-clock answer to the same probe, to the
//! millisecond; and two tests show why hyper's own header timer is not used.

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::time::Instant;

use super::*;

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    /// Bytes read at this many milliseconds, with their first line.
    Read(u128, String),
    /// The end of the stream at this many milliseconds.
    End(u128),
}

/// Reads until the end, recording when each read came and its first line.
async fn record(client: &mut DuplexStream, start: Instant) -> Vec<Seen> {
    let mut seen = Vec::new();
    let mut buf = vec![0_u8; 4096];
    loop {
        match client.read(&mut buf).await {
            Ok(0) | Err(_) => {
                seen.push(Seen::End(start.elapsed().as_millis()));
                return seen;
            }
            Ok(read) => {
                let text = String::from_utf8_lossy(&buf[..read]);
                let first = text.split("\r\n").next().unwrap_or("").to_owned();
                seen.push(Seen::Read(start.elapsed().as_millis(), first));
            }
        }
    }
}

const OK: &str = "HTTP/1.1 200 OK";

/// Go: 5.00s, then the end without a response: the head was not complete after 5s.
#[tokio::test(start_paused = true)]
async fn a_slowloris_is_closed_at_5s() {
    let start = Instant::now();
    let (mut reader, mut writer) = tokio::io::split(connect(healthy_app()));
    writer
        .write_all(b"GET /healthz HTTP/1.1\r\n")
        .await
        .unwrap();
    let dripping = tokio::spawn(async move {
        for byte in b"Host: xxxxxxxxxxxxxxxxxxxxxx\r\n" {
            if writer.write_all(&[*byte]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let mut response = Vec::new();
    let _ = reader.read_to_end(&mut response).await;
    assert!(
        response.is_empty(),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(start.elapsed().as_millis(), 5000);
    dripping.abort();
}

/// Go: the answer at once, then the end at 60.03s: an idle keep-alive connection is closed
/// after 60s.
#[tokio::test(start_paused = true)]
async fn an_idle_keep_alive_is_closed_at_60s() {
    let start = Instant::now();
    let mut client = connect(healthy_app());
    client
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(
        record(&mut client, start).await,
        [Seen::Read(0, OK.to_owned()), Seen::End(60_000)]
    );
}

/// Go: the next request's first bytes at 8s, its head still incomplete 5s later: the end at
/// 13s. Go also writes a 400 there, its partial line being malformed; hyper closes without one
/// (Q13).
#[tokio::test(start_paused = true)]
async fn the_keep_alive_header_timer_starts_at_the_first_byte() {
    let start = Instant::now();
    let (mut reader, mut writer) = tokio::io::split(connect(healthy_app()));
    writer
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let dripping = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(8)).await;
        writer.write_all(b"GET /hea").await.unwrap();
        for byte in b"lthz HTTP/1.1\r\nHost: xxxxxxx\r\n\r\n" {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if writer.write_all(&[*byte]).await.is_err() {
                break;
            }
        }
    });
    let mut response = Vec::new();
    let _ = reader.read_to_end(&mut response).await;
    assert!(String::from_utf8_lossy(&response).starts_with(OK));
    assert_eq!(start.elapsed().as_millis(), 13_000);
    dripping.abort();
}

/// Go: a 12s handler is answered at 12s although the 10s read deadline passed, which only
/// cancels the request's context; the connection then idles 60s: the end at 72s.
#[tokio::test(start_paused = true)]
async fn a_handler_past_the_read_deadline_still_answers() {
    let start = Instant::now();
    let mut client = connect(slow_app(Duration::from_secs(12)));
    client
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(
        record(&mut client, start).await,
        [Seen::Read(12_000, OK.to_owned()), Seen::End(72_000)]
    );
}

/// Go: a 16s handler misses the 15s write deadline set when the head was read: the end at 16s,
/// without a response.
#[tokio::test(start_paused = true)]
async fn the_write_deadline_is_15s_from_the_head() {
    let start = Instant::now();
    let mut client = connect(slow_app(Duration::from_secs(16)));
    client
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(record(&mut client, start).await, [Seen::End(16_000)]);
}

/// Go answers a slowly sent body only once draining it fails at 10s (a 200 with `Connection:
/// close`); hyper answers at once without reading it (Q13, recorded rather than matched).
#[tokio::test(start_paused = true)]
async fn a_slow_body_is_answered_at_once() {
    let start = Instant::now();
    let (mut reader, mut writer) = tokio::io::split(connect(healthy_app()));
    writer
        .write_all(b"POST /healthz HTTP/1.1\r\nHost: x\r\nContent-Length: 20\r\n\r\n")
        .await
        .unwrap();
    let dripping = tokio::spawn(async move {
        for _ in 0..20 {
            if writer.write_all(b"a").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let mut buf = vec![0_u8; 4096];
    let read = reader.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..read]).starts_with(OK));
    assert_eq!(start.elapsed().as_millis(), 0);
    dripping.abort();
}

async fn ok(
    _request: Request<Incoming>,
) -> Result<Response<http_body_util::Full<Bytes>>, Infallible> {
    Ok(Response::new(http_body_util::Full::new(
        Bytes::from_static(b"ok\n"),
    )))
}

/// Why hyper's header timer is off: hyper 1.12 arms it as soon as a connection goes idle, so it
/// closes an idle keep-alive connection at 5s, where Go keeps it 60s.
#[tokio::test(start_paused = true)]
async fn hyper_s_header_timeout_also_cuts_an_idle_keep_alive() {
    let (mut client, server_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let _ = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(Duration::from_secs(5))
            .serve_connection(TokioIo::new(server_io), service_fn(ok))
            .await;
    });
    let start = Instant::now();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response).await;
    assert!(String::from_utf8_lossy(&response).starts_with(OK));
    assert_eq!(start.elapsed().as_millis(), 5000);
}

/// And without a hyper timer, a header timeout is accepted by the builder and panics on the
/// connection's first poll.
#[tokio::test]
async fn hyper_s_header_timeout_without_a_timer_panics() {
    let (_client, server_io) = tokio::io::duplex(1 << 16);
    let task = tokio::spawn(async move {
        let _ = http1::Builder::new()
            .header_read_timeout(Duration::from_secs(5))
            .serve_connection(TokioIo::new(server_io), service_fn(ok))
            .await;
    });
    assert!(task.await.unwrap_err().is_panic());
}
