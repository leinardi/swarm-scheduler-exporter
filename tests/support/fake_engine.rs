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
//! It is built on `docker-engine`'s fake engine core (the listener, the HTTP/1.1 reading and
//! writing, keep-alive, `Content-Length` answers and chunked bodies) and its recorder's
//! normalisation; what it answers is this test's: the captures, the ping of a Docker 29 engine,
//! the daemon's 404s and the scripted event streams, each of which ends with the terminating
//! chunk, stays open until the client goes away, or is cut without the terminating chunk.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use docker_engine::test_util::fake_engine::{self, Connection, Listen, Request};
use docker_engine::test_util::recorder::Target;

use swarm_scheduler_exporter::docker::{Client, settings_from_env};

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
    // Serves until dropped.
    _engine: fake_engine::FakeEngine,
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
        Self::start_on("127.0.0.1:0", set, events_content_type, body_overrides).await
    }

    /// As [`FakeEngine::start`], listening on the IPv6 loopback.
    pub async fn start_ipv6(set: FixtureSet) -> Self {
        Self::start_on("[::1]:0", set, set.events_content_type, HashMap::new()).await
    }

    async fn start_on(
        bind: &str,
        set: FixtureSet,
        events_content_type: &str,
        body_overrides: HashMap<String, Vec<u8>>,
    ) -> Self {
        let fixtures: PathBuf = [
            env!("CARGO_MANIFEST_DIR"),
            "tests/testdata/engine_wire",
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
        let server_shared = Arc::clone(&shared);
        let engine = fake_engine::FakeEngine::tcp(
            bind,
            fake_engine::responder(move |connection| {
                serve_connection(Arc::clone(&server_shared), connection)
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("fake engine: bind: {error}"));
        let Listen::Tcp(addr) = *engine.listen() else {
            panic!("fake engine: not on TCP: {:?}", engine.listen());
        };
        Self {
            shared,
            addr,
            _engine: engine,
        }
    }

    /// A Docker client pointed at the fake over TCP, built from the environment as main builds
    /// it: it negotiates its API version lazily, with a ping before its first request.
    #[must_use]
    pub fn client(&self) -> Client {
        Self::client_for(&format!("tcp://{}", self.addr))
    }

    /// A Docker client built as [`FakeEngine::client`] builds it, with `host` as `DOCKER_HOST`.
    #[must_use]
    pub fn client_for(host: &str) -> Client {
        let settings = settings_from_env(|name| (name == "DOCKER_HOST").then(|| host.to_owned()))
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

async fn serve_connection(shared: Arc<Shared>, mut connection: Connection) {
    while let Some(request) = connection.read_request().await {
        let entry = record(&shared, &request);
        let keep_open = if let Some(body) = shared.body_overrides.get(&entry.path) {
            respond(
                &mut connection,
                200,
                "application/json",
                body,
                &request.method,
            )
            .await
        } else {
            route(&shared, &mut connection, &request, &entry).await
        };
        if !keep_open {
            return;
        }
    }
}

/// Answers one request; false when the connection is over.
async fn route(
    shared: &Shared,
    connection: &mut Connection,
    request: &Request,
    entry: &RecordedRequest,
) -> bool {
    let method = request.method.as_str();
    let path = entry.path.as_str();
    if path == "/_ping" {
        let headers = [
            ("Api-Version", DAEMON_API_VERSION),
            ("Ostype", "linux"),
            ("Docker-Experimental", "false"),
            ("Swarm", "active/manager"),
            ("Content-Type", "text/plain; charset=utf-8"),
        ];
        return connection.respond("200 OK", &headers, b"OK", method).await;
    }
    let fixture = |name: &str| read_fixture(shared, name);
    let answer = match path {
        "/events" => return serve_events(shared, connection, method).await,
        "/nodes" => fixture("nodes.json"),
        "/services" => fixture("services.json"),
        "/tasks" => fixture("tasks.json"),
        "/containers/json" => fixture("containers.json"),
        _ => None,
    };
    if let Some(body) = answer {
        return respond(connection, 200, "application/json", &body, method).await;
    }
    if let Some(id) = path.strip_prefix("/services/") {
        return serve_object(
            shared,
            connection,
            &format!("service-{id}.json"),
            &format!("service {id}"),
            method,
        )
        .await;
    }
    if let Some(id) = path
        .strip_prefix("/containers/")
        .and_then(|rest| rest.strip_suffix("/json"))
    {
        return serve_object(
            shared,
            connection,
            &format!("container-{id}.json"),
            &format!("container {id}"),
            method,
        )
        .await;
    }
    lock(&shared.recorder)
        .unexpected
        .push(format!("{} {}", request.method, request.target));
    respond(
        connection,
        404,
        "application/json",
        br#"{"message":"page not found"}"#,
        method,
    )
    .await
}

/// An inspect: the fixture when there is one, else the daemon's 404.
async fn serve_object(
    shared: &Shared,
    connection: &mut Connection,
    name: &str,
    what: &str,
    method: &str,
) -> bool {
    if let Some(body) = read_fixture(shared, name) {
        return respond(connection, 200, "application/json", &body, method).await;
    }
    let body = format!(r#"{{"message":"{what} not found"}}"#);
    respond(connection, 404, "application/json", body.as_bytes(), method).await
}

async fn serve_events(shared: &Shared, connection: &mut Connection, method: &str) -> bool {
    let events = {
        let mut script = lock(&shared.script);
        let index = script.served;
        script.served += 1;
        script.connections.get(index).cloned()
    };
    let Some(events) = events else {
        // Recorded like every request; the tests count /events requests, so an extra one fails
        // them instead of hanging them.
        return respond(
            connection,
            500,
            "application/json",
            br#"{"message":"unexpected extra events request"}"#,
            method,
        )
        .await;
    };
    // The head and every chunk in one write, as a daemon's buffered writer sends them.
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        shared.events_content_type
    )
    .into_bytes();
    for data in &events.chunks {
        out.extend_from_slice(&fake_engine::chunk(data));
    }
    if !connection.write_all(&out).await {
        return false;
    }
    match events.end {
        StreamEnd::Close => connection.end_chunks().await,
        StreamEnd::Cut => {
            connection.shutdown().await;
            false
        }
        StreamEnd::Hold => {
            connection.hold_until_closed().await;
            false
        }
    }
}

async fn respond(
    connection: &mut Connection,
    status: u16,
    content_type: &str,
    body: &[u8],
    method: &str,
) -> bool {
    let status = match status {
        200 => "200 OK",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    connection
        .respond(status, &[("Content-Type", content_type)], body, method)
        .await
}

fn record(shared: &Shared, request: &Request) -> RecordedRequest {
    let target = Target::parse(&request.target);
    let entry = RecordedRequest {
        method: request.method.clone(),
        normalized: target.normalized(&request.method),
        raw_path: target.raw_path,
        raw_query: target.raw_query,
        version_prefix: target.version_prefix,
        path: target.path,
        query: target.query,
        accept: request.header("accept").join(", "),
        user_agent: request.first_header("user-agent").to_owned(),
        host: request.first_header("host").to_owned(),
    };
    let mut recorder = lock(&shared.recorder);
    recorder.all.push(entry.clone());
    recorder.phase.push(entry.clone());
    entry
}
