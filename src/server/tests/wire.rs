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

//! The 37 Go 1.26 wire captures of the exporter's mux behind its `http.Server`, replayed against
//! the server, byte for byte. Each capture holds a header line naming the Go server's port
//! (18080 healthy, 18081 unhealthy with a reason, 18082 unhealthy without one), the request with
//! `\r\n` written out, then the response as read and `[end: <nil>]` once the server closed; the
//! `/metrics` capture was cut at 1200 bytes, before its end.

use std::path::Path;

use go_http::testing::{self, Capture};

use super::*;

/// The captures that differ, each with its Q13 row; every other one must be identical.
const DIFFERENT: [(&str, &str); 5] = [
    // Go's body is its runtime and process families (Q1), and its chunks follow the encoder's
    // writes; the head, with `Transfer-Encoding: chunked`, is the same.
    ("go-get-metrics", "Q1 body; Q13 chunk boundaries"),
    // hyper answers a malformed request with its own 400: no reason in the status line, no body.
    ("go-malformed", "Q13 malformed request"),
    ("go-bad-header", "Q13 malformed request"),
    // hyper answers an unknown version with a 400 where Go writes a 505.
    ("go-bad-version", "Q13 malformed request"),
    // hyper serves an HTTP/1.1 request without `Host`; Go refuses it with a 400.
    ("go-get-nohost", "Q13 malformed request"),
];

/// The Go server a capture was taken from, at the capture's time (the captures were taken over
/// a minute); `/metrics` answers 3300 bytes, more than Go buffers before chunking.
fn server_for(capture: &Capture) -> Server {
    let metrics = text_exposition("# HELP x y\n".repeat(300).leak().as_bytes());
    let (healthy, reason) = match capture.port {
        18080 => (true, ""),
        18081 => (false, "events stream down"),
        18082 => (false, ""),
        other => panic!("no Go server on port {other}"),
    };
    let clock = Arc::new(FakeClock::new(capture.date().unwrap_or_else(capture_time)));
    server(
        mux(
            Box::new(move || (healthy, reason.to_owned())),
            Box::new(move |_| metrics.clone()),
        ),
        clock,
    )
}

/// The captures, from `tests/testdata/http`.
fn captures() -> Vec<Capture> {
    testing::read_captures(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/http"))
        .unwrap()
}

/// 32 of the 37 captures are identical byte for byte, `Date` included; the other 5 are the Q13
/// and Q1 cases listed in [`DIFFERENT`].
#[tokio::test(start_paused = true)]
async fn the_go_wire_captures_are_reproduced() {
    let captures = captures();
    assert_eq!(captures.len(), 37);
    let replay = testing::replay(&captures, TIMEOUTS, server_for).await;
    let mut expected: Vec<&str> = DIFFERENT.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    let mut different = replay.different.clone();
    different.sort_unstable();
    assert_eq!(different, expected, "{}", replay.report);
    assert_eq!(replay.identical.len(), 32);
}

/// The `/metrics` capture's head is Go's, chunked framing included.
#[tokio::test(start_paused = true)]
async fn the_metrics_head_is_go_s() {
    let captures = captures();
    let capture = captures
        .iter()
        .find(|capture| capture.name == "go-get-metrics")
        .unwrap();
    let response =
        testing::exchange(Arc::new(server_for(capture)), TIMEOUTS, &capture.request).await;
    let head = testing::head(&response).unwrap();
    assert_eq!(head, testing::head(&capture.response).unwrap());
    assert!(response[head.len()..].starts_with(b"\r\n\r\n800\r\n"));
}
