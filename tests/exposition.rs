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

//! The golden `/metrics` diff: the Rust binary, stamped as the Go capture's was, against the Rust
//! fake Engine serving the api-1.56 fixtures, scraped under the protocol the Go v1.3.2 capture in
//! `tests/testdata/exposition/` was taken with, so that every remaining difference is a Q row or
//! a defect.
//!
//! The protocol (the throwaway Go capture program and its commands are in the body of the commit
//! that added the captures):
//! - both run with `--containers` and a poll delay of 1h, so exactly one replicas poll and one
//!   containers poll happen, against an `/events` stream that sends headers, no events, and
//!   stays open;
//! - settled before the first scrape without scraping: `/healthz` 200, then the last container
//!   inspect in the engine's request log (`GET /containers/c-plain/json`: c-paused needs none,
//!   c-swarm-api is a Swarm task's), then 500ms, then `/healthz` 200 again;
//! - scrapes over a raw TCP connection with exactly the listed headers and `Connection: close`
//!   (Go's `http.Client` would add `Accept-Encoding: gzip` by itself), in the capture's order, so
//!   scrape n shows `promhttp_metric_handler_requests_total{code="200"}` n−1;
//! - masked, each with an assertion instead: the `process_*` values (the schema is compared), the
//!   poll-duration `_sum` and bucket values (monotonic, `+Inf` = `_count` = 1, the `le` labels
//!   compared), and the two success timestamps (between the process start and the scrape).
//!   Nothing else: every other series is compared byte for byte, `go_*` removed from Go's (Q1).

#[expect(
    dead_code,
    reason = "the fake engine's request-pinning helpers serve the engine-wire test; this one only \
              replays the fixtures and reads the request log"
)]
mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use flate2::read::GzDecoder;
use jiff::{SignedDuration, Timestamp};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::Instant;

use swarm_scheduler_exporter::clock::{Clock as _, SystemClock};
use swarm_scheduler_exporter::version;

use support::OrPanic as _;
use support::fake_engine::{EventsConnection, FIXTURE_SETS, FakeEngine, StreamEnd};

/// The stamps the Go capture's binary was built with
/// (`-ldflags "-X main.version=golden -X main.commit=0000000 -X main.date=1970-01-01T00:00:00Z"`).
const GOLDEN_STAMPS: [(&str, &str); 3] = [
    ("SWARM_SCHEDULER_EXPORTER_VERSION", "golden"),
    ("SWARM_SCHEDULER_EXPORTER_COMMIT", "0000000"),
    ("SWARM_SCHEDULER_EXPORTER_DATE", "1970-01-01T00:00:00Z"),
];

/// The capture's `Accept` values.
const ACCEPT_TEXT: &str = "text/plain;version=0.0.4";
/// Prometheus 3.15.1's default scrape `Accept`, pinned: it changes between minor releases.
const ACCEPT_PROMETHEUS_3_15: &str = "application/openmetrics-text;version=1.0.0;escaping=allow-utf-8;q=0.7,application/openmetrics-text;version=0.0.1;q=0.6,text/plain;version=1.0.0;escaping=allow-utf-8;q=0.5,text/plain;version=0.0.4;q=0.4,*/*;q=0.3";
/// Prometheus 3.0's, which the capture scraped fifth; its capture is not kept, as it answered
/// what 3.15.1's did.
const ACCEPT_PROMETHEUS_3_0: &str = "application/openmetrics-text;version=1.0.0;escaping=allow-utf-8;q=0.6,application/openmetrics-text;version=0.0.1;escaping=allow-utf-8;q=0.5,text/plain;version=1.0.0;escaping=allow-utf-8;q=0.4,text/plain;version=0.0.4;escaping=allow-utf-8;q=0.3,*/*;q=0.2";
const ACCEPT_PROTOBUF: &str =
    "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited";
const ACCEPT_ESCAPING_UTF8: &str = "text/plain;version=0.0.4;escaping=allow-utf-8";

/// How long the exporter gets to start and settle.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

fn testdata(name: &str) -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests/testdata/exposition",
        name,
    ]
    .iter()
    .collect()
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(testdata(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn read_text(name: &str) -> String {
    String::from_utf8(read(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// `unwrap` for an `Option` in the helpers of this test.
trait OrPanicNone<T> {
    fn or_panic_none(self) -> T;
}

impl<T> OrPanicNone<T> for Option<T> {
    #[track_caller]
    fn or_panic_none(self) -> T {
        self.unwrap_or_else(|| panic!("unexpected None"))
    }
}

/// The binary with the golden stamps. When this test was not built with them (`make rust-test`
/// stamps nothing), a stamped copy is built once, into its own target directory, so the stamped
/// and unstamped builds do not keep invalidating each other.
fn golden_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let stamped = [version::VERSION, version::COMMIT, version::DATE]
            .iter()
            .zip(GOLDEN_STAMPS)
            .all(|(have, (_, want))| *have == want);
        if stamped {
            return PathBuf::from(env!("CARGO_BIN_EXE_swarm-scheduler-exporter"));
        }
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden-stamps");
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "--bin",
                "swarm-scheduler-exporter",
                "--target-dir",
            ])
            .arg(&target)
            .envs(GOLDEN_STAMPS)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .unwrap_or_else(|error| panic!("cargo build: {error}"));
        assert!(status.success(), "cargo build of the stamped binary failed");
        target.join("debug").join("swarm-scheduler-exporter")
    })
}

/// One HTTP answer, read off the wire.
struct Response {
    /// The status line and headers as sent, `Date` masked.
    head: String,
    /// The header values by lowercase name.
    headers: BTreeMap<String, String>,
    /// The body, de-chunked, still encoded.
    body: Vec<u8>,
}

impl Response {
    fn status(&self) -> u16 {
        self.head
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_default()
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }

    fn text(&self) -> String {
        let plain = if self.header("content-encoding") == "gzip" {
            gunzip(&self.body)
        } else {
            self.body.clone()
        };
        String::from_utf8(plain).or_panic()
    }
}

fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    GzDecoder::new(data).read_to_end(&mut out).or_panic();
    out
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|window| window == b"\r\n")
            .unwrap_or_else(|| panic!("chunk size line"));
        let size =
            usize::from_str_radix(std::str::from_utf8(&data[..line_end]).or_panic(), 16).or_panic();
        data = &data[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// The capture's scrape: `GET path` with `Host`, the capture's `User-Agent`, `headers` and
/// `Connection: close`, nothing else.
async fn scrape(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut request =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: sse-golden-capture\r\n");
    for (name, value) in headers {
        let _ = write!(request, "{name}: {value}\r\n");
    }
    request.push_str("Connection: close\r\n\r\n");
    let mut stream = TcpStream::connect(addr).await.or_panic();
    stream.write_all(request.as_bytes()).await.or_panic();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.or_panic();
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no end of headers"))
        + 4;
    let head = String::from_utf8(raw[..split].to_vec()).or_panic();
    let mut headers = BTreeMap::new();
    let mut masked = String::new();
    for line in head.split_inclusive("\r\n") {
        if let Some((name, value)) = line.trim_end().split_once(": ") {
            headers.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        if line.starts_with("Date: ") {
            masked.push_str("Date: <masked>\r\n");
        } else {
            masked.push_str(line);
        }
    }
    let rest = &raw[split..];
    let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    Response {
        head: masked,
        headers,
        body,
    }
}

/// The running binary, killed when dropped.
struct Exporter {
    child: Child,
    addr: SocketAddr,
    log: PathBuf,
}

impl Exporter {
    fn start(binary: &Path, engine: &FakeEngine) -> Self {
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .or_panic();
        let log = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exposition-exporter.log");
        let log_file = std::fs::File::create(&log).or_panic();
        let child = Command::new(binary)
            .args([
                "--containers",
                "--poll-delay",
                "1h",
                "--listen-addr",
                &addr.to_string(),
            ])
            .env_clear()
            .env("DOCKER_HOST", format!("tcp://{}", engine.addr()))
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(log_file.try_clone().or_panic())
            .stderr(log_file)
            .spawn()
            .or_panic();
        Self { child, addr, log }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn healthz(addr: SocketAddr) -> u16 {
    if TcpStream::connect(addr).await.is_err() {
        return 0;
    }
    scrape(addr, "/healthz", &[]).await.status()
}

async fn wait_for(exporter: &Exporter, what: &str, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    while !condition().await {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; exporter log:\n{}",
            exporter.log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The family name of a sample line.
fn sample_name(line: &str) -> &str {
    let end = line.find(['{', ' ']).unwrap_or(line.len());
    &line[..end]
}

/// Go's `go_*` families removed (Q1).
fn without_go(body: &str) -> String {
    body.split_inclusive('\n')
        .filter(|line| {
            let line = line
                .strip_prefix("# HELP ")
                .or_else(|| line.strip_prefix("# TYPE "))
                .unwrap_or(line);
            !line.starts_with("go_")
        })
        .collect()
}

/// The protocol's masks: the value of every `process_*` sample, of the poll-duration `_sum` and
/// buckets, and of the two success timestamps; when `scrape_counter` is set, also the value of
/// `promhttp_metric_handler_requests_total{code="200"}`, for comparing scrapes with each other.
fn mask(body: &str, scrape_counter: bool) -> String {
    body.split_inclusive('\n')
        .map(|line| {
            let name = sample_name(line);
            let masked = !line.starts_with('#')
                && (name.starts_with("process_")
                    || name == "swarm_exporter_poll_duration_seconds_bucket"
                    || name == "swarm_exporter_poll_duration_seconds_sum"
                    || name == "swarm_exporter_last_poll_success_timestamp_seconds"
                    || name == "swarm_exporter_last_resync_success_timestamp_seconds"
                    || (scrape_counter
                        && line
                            .starts_with("promhttp_metric_handler_requests_total{code=\"200\"} ")));
            match line.rfind(' ') {
                Some(space) if masked => format!("{} <masked>\n", &line[..space]),
                _ => line.to_owned(),
            }
        })
        .collect()
}

/// The value of the sample line that starts with `series` and a space.
fn sample(body: &str, series: &str) -> f64 {
    body.lines()
        .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("no sample {series}"))
        .parse()
        .or_panic()
}

/// The assertions that stand in for the masks, on scrape `n` of the Rust binary, which started
/// at `start` and was scraped by `now`: the scrape counters, the poll-duration histogram, and
/// the two success timestamps.
fn assert_counters(body: &str, n: u32, start: Timestamp, now: Timestamp) {
    let context = format!("scrape {n}");
    for (code, want) in [("200", f64::from(n - 1)), ("500", 0.0), ("503", 0.0)] {
        let got = sample(
            body,
            &format!("promhttp_metric_handler_requests_total{{code=\"{code}\"}}"),
        );
        assert!(
            (got - want).abs() < f64::EPSILON,
            "{context}: code {code} = {got}"
        );
    }
    let codes = body
        .lines()
        .filter(|line| line.starts_with("promhttp_metric_handler_requests_total{"))
        .count();
    assert_eq!(codes, 3, "{context}: only the 200, 500 and 503 counters");
    let in_flight = sample(body, "promhttp_metric_handler_requests_in_flight");
    assert!(
        (in_flight - 1.0).abs() < f64::EPSILON,
        "{context}: in flight"
    );
    let buckets: Vec<f64> = body
        .lines()
        .filter(|line| line.starts_with("swarm_exporter_poll_duration_seconds_bucket{"))
        .map(|line| {
            line.rsplit(' ')
                .next()
                .unwrap_or_default()
                .parse()
                .or_panic()
        })
        .collect();
    assert!(
        buckets.windows(2).all(|pair| pair[0] <= pair[1]),
        "{context}: buckets not monotonic: {buckets:?}"
    );
    let infinite = sample(
        body,
        "swarm_exporter_poll_duration_seconds_bucket{le=\"+Inf\"}",
    );
    let count = sample(body, "swarm_exporter_poll_duration_seconds_count");
    assert!(
        (infinite - 1.0).abs() < f64::EPSILON && (count - 1.0).abs() < f64::EPSILON,
        "{context}: +Inf bucket {infinite}, count {count}"
    );
    for name in [
        "swarm_exporter_last_poll_success_timestamp_seconds",
        "swarm_exporter_last_resync_success_timestamp_seconds",
    ] {
        let value = sample(body, name);
        let at = SignedDuration::try_from_secs_f64(value)
            .ok()
            .and_then(|since_epoch| Timestamp::from_duration(since_epoch).ok())
            .unwrap_or_else(|| panic!("{context}: {name} = {value} is not a time"));
        assert!(
            start <= at && at <= now,
            "{context}: {name} = {at} not between {start} and {now}"
        );
    }
}

/// The body a text scrape of Go answered, with `go_*` removed: the reference Rust's must equal
/// after the masks.
fn go_reference(name: &str) -> String {
    mask(&without_go(&read_text(name)), false)
}

/// The text `Content-Type` Rust answers where Go answered `go`, protobuf included (Q2): the same
/// `escaping` parameter on text 0.0.4.
fn rust_content_type(go: &str) -> String {
    let escaping = go
        .rsplit("; ")
        .find_map(|parameter| parameter.strip_prefix("escaping="))
        .unwrap_or_else(|| panic!("no escaping in {go}"));
    format!("text/plain; version=0.0.4; charset=utf-8; escaping={escaping}")
}

/// The testdata is the capture it claims to be: the raw gzip body decompresses to the kept
/// prometheus3 body, the engine saw what the protocol says, and the header-only negotiation rows
/// are the ones the exposition unit tests pin.
fn assert_testdata_consistent() {
    assert_eq!(
        gunzip(&read("go-v1.3.2-prometheus3.body.gz")),
        read("go-v1.3.2-prometheus3.body")
    );
    assert!(!read("go-v1.3.2-protobuf.body.bin").is_empty());
    let unit_rows = read_text("go-negotiation.tsv");
    for row in read_text("go-v1.3.2-negotiation.tsv").lines().skip(1) {
        let (accept, rest) = row.split_once('\t').or_panic_none();
        let (encoding, rest) = rest.split_once("\t=> ").or_panic_none();
        let dash = |value: &str| {
            if value.is_empty() {
                "-".to_owned()
            } else {
                value.to_owned()
            }
        };
        let want = format!("{}\t{}\t{rest}", dash(accept), dash(encoding));
        assert!(
            unit_rows.lines().any(|line| line == want),
            "negotiation row {row:?} is not in go-negotiation.tsv"
        );
    }
}

/// The engine-wire requests of the run against Go's: the same multiset, the pings aside, which
/// Q11 shares between concurrent first requests (Go sent one per concurrent first request).
fn assert_engine_requests(engine: &FakeEngine) {
    let go_log = read_text("go-v1.3.2-engine-requests.log");
    let (mut go, go_pings): (Vec<&str>, Vec<&str>) =
        go_log.lines().partition(|line| *line != "HEAD /_ping");
    let requests = engine.all_requests();
    let mut rust: Vec<String> = requests
        .iter()
        .filter(|request| request.path != "/_ping")
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    go.sort_unstable();
    rust.sort_unstable();
    assert_eq!(rust, go, "the engine requests, pings aside");
    let pings = requests
        .iter()
        .filter(|request| request.path == "/_ping")
        .count();
    assert_eq!((go_pings.len(), pings), (2, 1), "pings: Go, Rust");
}

/// Go: the S2 capture of v1.3.2's `/metrics` (no Go counterpart). Cases 1 and 2 (text, and Prometheus
/// 3.15.1's `Accept` with gzip) are byte-exact after the masks with `go_*` removed, headers
/// included; case 3 (protobuf) differs only in Go's protobuf `Content-Type` (Q2), the body equal
/// to Go's protobuf decoded to text; and the `escaping` echo and `*/*` answer Go's.
#[tokio::test(flavor = "multi_thread")]
async fn metrics_match_the_go_capture() {
    assert_testdata_consistent();
    let binary = tokio::task::spawn_blocking(golden_binary).await.or_panic();
    let engine = FakeEngine::start(FIXTURE_SETS[1], std::collections::HashMap::new()).await;
    engine.script_events(vec![EventsConnection {
        chunks: Vec::new(),
        end: StreamEnd::Hold,
    }]);
    let start = SystemClock.now();
    let exporter = Exporter::start(binary, &engine);
    let addr = exporter.addr;

    wait_for(&exporter, "healthz 200", async || {
        healthz(addr).await == 200
    })
    .await;
    wait_for(&exporter, "the last container inspect", async || {
        engine
            .all_requests()
            .iter()
            .any(|request| request.method == "GET" && request.path == "/containers/c-plain/json")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(healthz(addr).await, 200, "healthz after settling");

    let mut n = 0;
    let mut next = async |headers: &[(&str, &str)]| {
        n += 1;
        let response = scrape(addr, "/metrics", headers).await;
        let text = response.text();
        assert_counters(&text, n, start, SystemClock.now());
        (response, text)
    };

    // 1: text.
    let (text, text_body) = next(&[("Accept", ACCEPT_TEXT)]).await;
    assert_eq!(text.head, read_text("go-v1.3.2-text.headers"));
    assert_eq!(mask(&text_body, false), go_reference("go-v1.3.2-text.body"));
    // 2 and 3: stable.
    for _ in 2..=3 {
        let (again, again_body) = next(&[("Accept", ACCEPT_TEXT)]).await;
        assert_eq!(again.head, text.head);
        assert_eq!(mask(&again_body, true), mask(&text_body, true));
    }
    // 4: Prometheus 3.15.1, gzip.
    let (prometheus3, prometheus3_body) = next(&[
        ("Accept", ACCEPT_PROMETHEUS_3_15),
        ("Accept-Encoding", "gzip"),
    ])
    .await;
    assert_eq!(prometheus3.head, read_text("go-v1.3.2-prometheus3.headers"));
    assert_eq!(
        mask(&prometheus3_body, false),
        go_reference("go-v1.3.2-prometheus3.body")
    );
    // 5: Prometheus 3.0, gzip: as 4.
    let (legacy, legacy_body) = next(&[
        ("Accept", ACCEPT_PROMETHEUS_3_0),
        ("Accept-Encoding", "gzip"),
    ])
    .await;
    assert_eq!(legacy.head, prometheus3.head);
    assert_eq!(mask(&legacy_body, true), mask(&text_body, true));
    // 6: protobuf: Go's Content-Type is protobuf, Rust's text (Q2).
    let (protobuf, protobuf_body) = next(&[("Accept", ACCEPT_PROTOBUF)]).await;
    let go_head = read_text("go-v1.3.2-protobuf.headers");
    let go_type = "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; \
                   encoding=delimited; escaping=underscores";
    assert!(go_head.contains(&format!("\r\nContent-Type: {go_type}\r\n")));
    assert_eq!(
        protobuf.head,
        go_head.replace(go_type, &rust_content_type(go_type))
    );
    assert_eq!(
        mask(&protobuf_body, false),
        go_reference("go-v1.3.2-protobuf.body")
    );
    // 7: the escaping echo; 8: */*.
    for (accept, case) in [(ACCEPT_ESCAPING_UTF8, "escaping"), ("*/*", "star")] {
        let (response, body) = next(&[("Accept", accept)]).await;
        assert_eq!(
            response.head,
            read_text(&format!("go-v1.3.2-{case}.headers"))
        );
        assert_eq!(
            mask(&body, false),
            go_reference(&format!("go-v1.3.2-{case}.body"))
        );
    }

    // 9 to 32: the header-only negotiation rows, Go's protobuf answers as Rust's text (Q2).
    for row in read_text("go-v1.3.2-negotiation.tsv").lines().skip(1) {
        let (accept, rest) = row.split_once('\t').or_panic_none();
        let (encoding, rest) = rest.split_once("\t=> ").or_panic_none();
        let (go_type, go_encoding) = rest.split_once('\t').or_panic_none();
        let mut headers = Vec::new();
        if !accept.is_empty() {
            headers.push(("Accept", accept));
        }
        if !encoding.is_empty() {
            headers.push(("Accept-Encoding", encoding));
        }
        let response = scrape(addr, "/metrics", &headers).await;
        assert_eq!(
            (
                response.header("content-type"),
                response.header("content-encoding")
            ),
            (rust_content_type(go_type).as_str(), go_encoding),
            "negotiation row {row:?}"
        );
    }

    assert_engine_requests(&engine);
}
