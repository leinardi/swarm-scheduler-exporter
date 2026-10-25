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

//! A fake Docker Engine serving the engine-wire captures over TCP, and the recorder of every
//! request it gets: the port of the Go engine-wire test's `fakeEngine` and `requestRecorder`.
//!
//! It speaks just enough HTTP/1.1 for the client: keep-alive, `Content-Length` answers, and the
//! event stream as a chunked body that ends with the terminating chunk, stays open until the
//! client goes away, or is cut without the terminating chunk.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use swarm_scheduler_exporter::docker::{Client, Settings};

/// What the fake advertises on `/_ping`: the API version of a Docker 29 engine.
pub const DAEMON_API_VERSION: &str = "1.56";

/// One set of captures, with the `Content-Type` a daemon of that API version streams events with.
#[derive(Debug, Clone, Copy)]
pub struct FixtureSet {
    pub dir: &'static str,
    pub events_content_type: &'static str,
}

/// The two fixture sets: the same cluster in the response shapes of API 1.51 (Docker 28) and
/// 1.56 (Docker 29).
pub const FIXTURE_SETS: [FixtureSet; 2] = [
    FixtureSet {
        dir: "api-1.51",
        events_content_type: "application/json",
    },
    FixtureSet {
        dir: "api-1.56",
        events_content_type: "application/jsonl",
    },
];

/// How a scripted events connection ends after its chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The terminating chunk: a clean end.
    Close,
    /// Nothing more until the client goes away.
    Hold,
    /// The connection closed without the terminating chunk, as a proxy timeout or a killed
    /// daemon ends it (S1).
    Cut,
}

/// One scripted `GET /events` connection: the chunks of its body, then its end.
#[derive(Debug, Clone)]
pub struct EventsConnection {
    pub chunks: Vec<Vec<u8>>,
    pub end: StreamEnd,
}

/// One request, as the recorder saw it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub raw_path: String,
    pub raw_query: String,
    /// `/v1.56`, or empty for an unversioned request such as `/_ping`.
    pub version_prefix: String,
    /// The path without the version prefix.
    pub path: String,
    /// The query pairs, decoded, in their order.
    pub query: Vec<(String, String)>,
    pub normalized: String,
    /// Every `Accept` line, joined with `, ` as Go's test joins them.
    pub accept: String,
    pub user_agent: String,
    pub host: String,
}

impl RecordedRequest {
    /// The first value of `key`, as Go's `url.Values.Get`.
    #[must_use]
    pub fn query_value(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Every request of a run, and those since the last [`Recorder::reset`].
#[derive(Debug, Default)]
pub struct Recorder {
    all: Vec<RecordedRequest>,
    phase: Vec<RecordedRequest>,
    unexpected: Vec<String>,
}

#[derive(Debug, Default)]
struct Script {
    connections: Vec<EventsConnection>,
    served: usize,
}

#[derive(Debug)]
struct Shared {
    fixtures: PathBuf,
    events_content_type: String,
    body_overrides: HashMap<String, Vec<u8>>,
    recorder: Mutex<Recorder>,
    script: Mutex<Script>,
}

/// The fake engine, serving until it is dropped.
#[derive(Debug)]
pub struct FakeEngine {
    shared: Arc<Shared>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeEngine {
    /// Serves `set`, answering a path (without the version prefix) in `body_overrides` with its
    /// body instead.
    pub async fn start(set: FixtureSet, body_overrides: HashMap<String, Vec<u8>>) -> Self {
        Self::start_with_events_type(set, set.events_content_type, body_overrides).await
    }

    /// As [`FakeEngine::start`], streaming events with `events_content_type`.
    pub async fn start_with_events_type(
        set: FixtureSet,
        events_content_type: &str,
        body_overrides: HashMap<String, Vec<u8>>,
    ) -> Self {
        let fixtures: PathBuf = [
            env!("CARGO_MANIFEST_DIR"),
            "internal/collector/testdata/engine_wire",
            set.dir,
        ]
        .iter()
        .collect();
        let shared = Arc::new(Shared {
            fixtures,
            events_content_type: events_content_type.to_owned(),
            body_overrides,
            recorder: Mutex::new(Recorder::default()),
            script: Mutex::new(Script::default()),
        });
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("fake engine: bind: {error}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("fake engine: local address: {error}"));
        let server_shared = Arc::clone(&shared);
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_connection(Arc::clone(&server_shared), stream));
            }
        });
        Self {
            shared,
            addr,
            server,
        }
    }

    /// A Docker client pointed at the fake over TCP, built from the environment as main builds
    /// it: it negotiates its API version lazily, with a ping before its first request.
    #[must_use]
    pub fn client(&self) -> Client {
        let host = format!("tcp://{}", self.addr);
        let settings = Settings::from_env(|name| (name == "DOCKER_HOST").then(|| host.clone()))
            .unwrap_or_else(|error| panic!("docker client: {error}"));
        Client::new(settings)
    }

    /// The address the fake listens on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The fixture file `name`.
    #[must_use]
    pub fn fixture(&self, name: &str) -> Vec<u8> {
        read_fixture(&self.shared, name).unwrap_or_else(|| panic!("fixture {name}: missing"))
    }

    /// Scripts the next `GET /events` connections; one past the script gets a 500.
    pub fn script_events(&self, connections: Vec<EventsConnection>) {
        let mut script = lock(&self.shared.script);
        script.connections = connections;
        script.served = 0;
    }

    /// Starts a new phase of the recording.
    pub fn reset(&self) {
        lock(&self.shared.recorder).phase.clear();
    }

    /// The requests of this phase, without the ping, which W2 lets move between phases.
    #[must_use]
    pub fn phase_requests(&self) -> Vec<RecordedRequest> {
        lock(&self.shared.recorder)
            .phase
            .iter()
            .filter(|entry| entry.path != "/_ping")
            .cloned()
            .collect()
    }

    /// Every request of the run.
    #[must_use]
    pub fn all_requests(&self) -> Vec<RecordedRequest> {
        lock(&self.shared.recorder).all.clone()
    }

    /// The requests the fake had no answer for.
    #[must_use]
    pub fn unexpected_requests(&self) -> Vec<String> {
        lock(&self.shared.recorder).unexpected.clone()
    }
}

fn read_fixture(shared: &Shared, name: &str) -> Option<Vec<u8>> {
    // Only the file name, as the Go fake's filepath.Base: a path in a request stays inside.
    let base = name.rsplit('/').next().unwrap_or(name);
    std::fs::read(shared.fixtures.join(base)).ok()
}

/// One request line and its headers.
struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

async fn read_head(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Head> {
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let text = String::from_utf8_lossy(&buffer[..end]).into_owned();
            buffer.drain(..end + 4);
            let mut lines = text.split("\r\n");
            let mut request_line = lines.next()?.split(' ');
            let method = request_line.next()?.to_owned();
            let target = request_line.next()?.to_owned();
            let headers = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            return Some(Head {
                method,
                target,
                headers,
            });
        }
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

async fn serve_connection(shared: Arc<Shared>, mut stream: TcpStream) {
    let mut buffer = Vec::new();
    while let Some(head) = read_head(&mut stream, &mut buffer).await {
        let entry = record(&shared, &head);
        let keep_open = if let Some(body) = shared.body_overrides.get(&entry.path) {
            respond(&mut stream, 200, "application/json", body, &head.method).await
        } else {
            route(&shared, &mut stream, &head, &entry).await
        };
        if !keep_open {
            return;
        }
    }
}

/// Answers one request; false when the connection is over.
async fn route(
    shared: &Shared,
    stream: &mut TcpStream,
    head: &Head,
    entry: &RecordedRequest,
) -> bool {
    let path = entry.path.as_str();
    if path == "/_ping" {
        let headers = format!(
            "HTTP/1.1 200 OK\r\nApi-Version: {DAEMON_API_VERSION}\r\nOstype: linux\r\n\
             Docker-Experimental: false\r\nSwarm: active/manager\r\n\
             Content-Type: text/plain; charset=utf-8\r\nContent-Length: 2\r\n\r\n"
        );
        let mut out = headers.into_bytes();
        if head.method == "GET" {
            out.extend_from_slice(b"OK");
        }
        return stream.write_all(&out).await.is_ok();
    }
    let fixture = |name: &str| read_fixture(shared, name);
    let answer = match path {
        "/events" => return serve_events(shared, stream, &head.method).await,
        "/nodes" => fixture("nodes.json"),
        "/services" => fixture("services.json"),
        "/tasks" => fixture("tasks.json"),
        "/containers/json" => fixture("containers.json"),
        _ => None,
    };
    if let Some(body) = answer {
        return respond(stream, 200, "application/json", &body, &head.method).await;
    }
    if let Some(id) = path.strip_prefix("/services/") {
        return serve_object(
            shared,
            stream,
            &format!("service-{id}.json"),
            &format!("service {id}"),
            &head.method,
        )
        .await;
    }
    if let Some(id) = path
        .strip_prefix("/containers/")
        .and_then(|rest| rest.strip_suffix("/json"))
    {
        return serve_object(
            shared,
            stream,
            &format!("container-{id}.json"),
            &format!("container {id}"),
            &head.method,
        )
        .await;
    }
    lock(&shared.recorder)
        .unexpected
        .push(format!("{} {}", head.method, head.target));
    respond(
        stream,
        404,
        "application/json",
        br#"{"message":"page not found"}"#,
        &head.method,
    )
    .await
}

/// An inspect: the fixture when there is one, else the daemon's 404.
async fn serve_object(
    shared: &Shared,
    stream: &mut TcpStream,
    name: &str,
    what: &str,
    method: &str,
) -> bool {
    if let Some(body) = read_fixture(shared, name) {
        return respond(stream, 200, "application/json", &body, method).await;
    }
    let body = format!(r#"{{"message":"{what} not found"}}"#);
    respond(stream, 404, "application/json", body.as_bytes(), method).await
}

async fn serve_events(shared: &Shared, stream: &mut TcpStream, method: &str) -> bool {
    let connection = {
        let mut script = lock(&shared.script);
        let index = script.served;
        script.served += 1;
        script.connections.get(index).cloned()
    };
    let Some(connection) = connection else {
        // Recorded like every request; the tests count /events requests, so an extra one fails
        // them instead of hanging them.
        return respond(
            stream,
            500,
            "application/json",
            br#"{"message":"unexpected extra events request"}"#,
            method,
        )
        .await;
    };
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        shared.events_content_type
    )
    .into_bytes();
    for chunk in connection.chunks.iter().filter(|chunk| !chunk.is_empty()) {
        out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    if stream.write_all(&out).await.is_err() {
        return false;
    }
    match connection.end {
        StreamEnd::Close => stream.write_all(b"0\r\n\r\n").await.is_ok(),
        StreamEnd::Cut => {
            let _ = stream.shutdown().await;
            false
        }
        StreamEnd::Hold => {
            // Until the client goes away.
            let mut scratch = [0_u8; 1024];
            while matches!(stream.read(&mut scratch).await, Ok(read) if read > 0) {}
            false
        }
    }
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    method: &str,
) -> bool {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if method != "HEAD" {
        out.extend_from_slice(body);
    }
    stream.write_all(&out).await.is_ok()
}

fn record(shared: &Shared, head: &Head) -> RecordedRequest {
    let (raw_path, raw_query) = head
        .target
        .split_once('?')
        .map_or((head.target.as_str(), ""), |(path, query)| (path, query));
    let raw_path = percent_decode(raw_path, false);
    let (version_prefix, path) = split_version_prefix(&raw_path);
    let query = parse_query(raw_query);
    let header = |name: &str| -> Vec<&str> {
        head.headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    };
    let entry = RecordedRequest {
        method: head.method.clone(),
        normalized: normalize(&head.method, &path, &query),
        raw_path: raw_path.clone(),
        raw_query: raw_query.to_owned(),
        version_prefix,
        path,
        query,
        accept: header("accept").join(", "),
        user_agent: header("user-agent")
            .first()
            .copied()
            .unwrap_or_default()
            .to_owned(),
        host: header("host")
            .first()
            .copied()
            .unwrap_or_default()
            .to_owned(),
    };
    let mut recorder = lock(&shared.recorder);
    recorder.all.push(entry.clone());
    recorder.phase.push(entry.clone());
    entry
}

/// `^(/v\d+\.\d+)(/.*)$`.
fn split_version_prefix(path: &str) -> (String, String) {
    let versioned = path.strip_prefix("/v").and_then(|rest| {
        let slash = rest.find('/')?;
        let (major, minor) = rest[..slash].split_once('.')?;
        let digits =
            |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
        (digits(major) && digits(minor)).then(|| slash + 2)
    });
    match versioned {
        Some(split) => (path[..split].to_owned(), path[split..].to_owned()),
        None => (String::new(), path.to_owned()),
    }
}

/// Go's `url.ParseQuery`, pairs in order.
fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key, true), percent_decode(value, true))
        })
        .collect()
}

/// `%XX` decoding, and `+` as a space in a query.
fn percent_decode(text: &str, plus_is_space: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes
                    .get(index + 1..index + 3)
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(byte) = hex {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus_is_space => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The request without what legitimately varies: the version prefix (W1), the encoding of
/// `filters` (decoded to a sorted canonical form) and the values of `since` and `until`.
fn normalize(method: &str, path: &str, query: &[(String, String)]) -> String {
    let mut pairs: Vec<(&str, String)> = query
        .iter()
        .map(|(key, value)| {
            let value = match key.as_str() {
                "filters" => canonical_filters(value),
                "since" | "until" => "<time>".to_owned(),
                _ => value.clone(),
            };
            (key.as_str(), value)
        })
        .collect();
    pairs.sort_by(|left, right| left.0.cmp(right.0));
    if pairs.is_empty() {
        return format!("{method} {path}");
    }
    let joined: Vec<String> = pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    format!("{method} {path}?{}", joined.join("&"))
}

/// Both filter encodings the Engine API has used, `{"key":{"value":true}}` and the legacy
/// `{"key":["value"]}`, as `{key:[value,…];…}`.
fn canonical_filters(raw: &str) -> String {
    let Ok(Value::Object(terms)) = serde_json::from_str::<Value>(raw) else {
        return format!("<invalid filters {raw}>");
    };
    let mut decoded: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, values) in terms {
        let mut list: Vec<String> = match values {
            Value::Object(set) => set.keys().cloned().collect(),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().unwrap_or("<invalid>").to_owned())
                .collect(),
            other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
                vec![format!("<invalid {other}>")]
            }
        };
        list.sort();
        decoded.insert(key, list);
    }
    let parts: Vec<String> = decoded
        .iter()
        .map(|(key, values)| format!("{key}:[{}]", values.join(",")))
        .collect();
    format!("{{{}}}", parts.join(";"))
}
