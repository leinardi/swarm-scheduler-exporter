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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

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

/// A capture's request, and its response with whether the capture was cut before the end.
struct Capture {
    port: u16,
    request: Vec<u8>,
    response: Vec<u8>,
    truncated: bool,
}

fn parse(text: &[u8]) -> Capture {
    let mut lines = text.splitn(3, |byte| *byte == b'\n');
    let title = String::from_utf8(lines.next().unwrap().to_vec()).unwrap();
    let port = title
        .rsplit_once("(port ")
        .and_then(|(_, port)| port.strip_suffix(')'))
        .unwrap()
        .parse()
        .unwrap();
    let request_line = String::from_utf8(lines.next().unwrap().to_vec()).unwrap();
    let request = request_line
        .strip_prefix("### request: ")
        .unwrap()
        .replace("\\r\\n", "\r\n")
        .into_bytes();
    let rest = lines.next().unwrap();
    let end = b"[end: <nil>]\n\n";
    let (response, truncated) = if rest.ends_with(end) {
        (rest[..rest.len() - end.len()].to_vec(), false)
    } else {
        (rest.strip_suffix(b"\n").unwrap_or(rest).to_vec(), true)
    };
    Capture {
        port,
        request,
        response,
        truncated,
    }
}

/// The time of the capture's `Date`, so it compares exactly; the captures were taken over a
/// minute.
fn date_of(response: &[u8]) -> Timestamp {
    let text = String::from_utf8_lossy(response);
    text.split("\r\n")
        .find_map(|line| line.strip_prefix("Date: "))
        .map_or_else(capture_time, |date| {
            jiff::fmt::strtime::parse("%a, %d %b %Y %H:%M:%S GMT", date)
                .and_then(|parsed| parsed.to_datetime())
                .and_then(|civil| civil.to_zoned(jiff::tz::TimeZone::UTC))
                .unwrap()
                .timestamp()
        })
}

/// The Go server a capture was taken from, at the capture's time; `/metrics` answers 3300 bytes,
/// more than Go buffers before chunking.
fn app_for(capture: &Capture) -> App {
    let metrics = text_exposition("# HELP x y\n".repeat(300).leak().as_bytes());
    let mut app = match capture.port {
        18080 => app_with(true, "", metrics),
        18081 => app_with(false, "events stream down", metrics),
        18082 => app_with(false, "", metrics),
        other => panic!("no Go server on port {other}"),
    };
    app.clock = Arc::new(FakeClock::new(date_of(&capture.response)));
    app
}

/// 32 of the 37 captures are identical byte for byte, `Date` included; the other 5 are the Q13
/// and Q1 cases listed in [`DIFFERENT`].
#[tokio::test(start_paused = true)]
async fn the_go_wire_captures_are_reproduced() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/http");
    let mut names: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    names.sort();
    assert_eq!(names.len(), 37);
    let mut outcomes = BTreeMap::new();
    let mut report = String::new();
    for path in names {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let capture = parse(&std::fs::read(&path).unwrap());
        let response = exchange(app_for(&capture), &capture.request).await;
        let identical = if capture.truncated {
            response.starts_with(&capture.response)
        } else {
            response == capture.response
        };
        if !identical {
            let _ = writeln!(
                report,
                "{name} differs:\n--- Go\n{}\n--- Rust\n{}",
                String::from_utf8_lossy(&capture.response),
                String::from_utf8_lossy(&response)
            );
        }
        outcomes.insert(name, identical);
    }
    let different: Vec<&str> = outcomes
        .iter()
        .filter(|(_, identical)| !**identical)
        .map(|(name, _)| name.as_str())
        .collect();
    let mut expected: Vec<&str> = DIFFERENT.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(different, expected, "{report}");
    let identical = outcomes.values().filter(|identical| **identical).count();
    assert!(identical >= 31, "{identical} identical");
    assert_eq!(identical, 32);
}

/// The `/metrics` capture's head is Go's, chunked framing included.
#[tokio::test(start_paused = true)]
async fn the_metrics_head_is_go_s() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/http");
    let capture = parse(&std::fs::read(directory.join("go-get-metrics.txt")).unwrap());
    let response = exchange(app_for(&capture), &capture.request).await;
    let head_end = |bytes: &[u8]| {
        bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
    };
    let go_head = &capture.response[..head_end(&capture.response)];
    assert_eq!(&response[..head_end(&response)], go_head);
    assert!(response[head_end(&response)..].starts_with(b"\r\n\r\n800\r\n"));
}
