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

use std::time::Duration;

use docker_client::test_util::{FakeEngine, Reply};
use jiff::Timestamp;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

/// The program and its version, or the program alone when the version stamp is something no
/// header may carry.
#[test]
fn the_user_agent_is_the_program_and_its_version() {
    assert_eq!(user_agent("2.1.0"), "swarm-scheduler-exporter/2.1.0");
    assert_eq!(user_agent("dev"), "swarm-scheduler-exporter/dev");
    assert_eq!(
        user_agent("2.1.0\r\nX-Injected: 1"),
        "swarm-scheduler-exporter"
    );
    assert_eq!(user_agent("bad\u{7f}"), "swarm-scheduler-exporter");
    // A tab is allowed in a header value, as in Go.
    assert_eq!(user_agent("a\tb"), "swarm-scheduler-exporter/a\tb");
}

/// The settings `run` builds: the environment's, with the exporter's `User-Agent` and the 30s
/// response-header timeout.
#[test]
fn settings_from_env_carry_the_user_agent_and_the_header_timeout() {
    assert_eq!(RESPONSE_HEADER_TIMEOUT, Duration::from_secs(30));
    let getenv = env(&[("DOCKER_HOST", "tcp://127.0.0.1:2375")]);
    let want = Config::from_env(&getenv)
        .unwrap()
        .user_agent(format!(
            "swarm-scheduler-exporter/{}",
            crate::version::VERSION
        ))
        .response_header_timeout(Duration::from_secs(30));
    assert_eq!(settings_from_env(&getenv).unwrap(), want);
    assert_ne!(
        settings_from_env(&getenv).unwrap(),
        want.response_header_timeout(Duration::from_secs(31))
    );
}

#[test]
fn settings_from_env_refuse_what_no_engine_could_be_reached_with() {
    let message = |pairs: &[(&str, &str)]| settings_from_env(env(pairs)).unwrap_err().to_string();
    assert_eq!(
        message(&[("DOCKER_HOST", "daemon")]),
        "docker client: DOCKER_HOST \"daemon\" has an unknown scheme"
    );
    assert_eq!(
        message(&[("DOCKER_API_VERSION", "abc")]),
        "unsupported Docker API version abc (DOCKER_API_VERSION): supported range is 1.40 to 1.56"
    );
    assert_eq!(
        message(&[("DOCKER_API_VERSION", "1.39")]),
        "unsupported Docker API version 1.39 (DOCKER_API_VERSION): supported range is 1.40 to 1.56"
    );
    assert_eq!(
        message(&[
            ("DOCKER_HOST", "tcp://10.0.0.1:2376"),
            ("DOCKER_CERT_PATH", "/certs")
        ]),
        "docker client: DOCKER_CERT_PATH is set but DOCKER_TLS_VERIFY is not: set \
         DOCKER_TLS_VERIFY=1 to verify the daemon (TLS without verification is not supported)"
    );
    // A unix socket ignores the certificates, as does a TLS host with verification on.
    settings_from_env(env(&[("DOCKER_CERT_PATH", "/certs")])).unwrap();
    settings_from_env(env(&[
        ("DOCKER_HOST", "tcp://10.0.0.1:2376"),
        ("DOCKER_CERT_PATH", "/certs"),
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap();
    assert!(message(&[("DOCKER_TLS", "1")]).starts_with("docker client: DOCKER_TLS"));
    assert_eq!(
        message(&[("DOCKER_API_VERSION", "1.57")]),
        "unsupported Docker API version 1.57 (DOCKER_API_VERSION): supported range is 1.40 to 1.56"
    );
    for pin in ["1.40", "1.51", "1.56", ""] {
        settings_from_env(env(&[("DOCKER_API_VERSION", pin)])).unwrap();
    }
}

#[test]
fn since_is_seconds_and_nine_digits_of_nanoseconds() {
    let at = |text: &str| since_parameter(text.parse::<Timestamp>().unwrap());
    assert_eq!(at("2026-09-20T10:00:00Z"), "1789898400.000000000");
    assert_eq!(at("2026-09-20T10:00:00.5Z"), "1789898400.500000000");
    assert_eq!(at("1969-12-31T23:59:59.5Z"), "-1.500000000");
}

#[test]
fn filters_are_terms_with_sorted_values() {
    assert!(Filters::new().is_empty());
    let filters = Filters::new().add("type", &["service", "node", "service"]);
    assert!(!filters.is_empty());
    assert_eq!(
        filters.to_map().get("type").map(Vec::as_slice),
        Some(["node".to_owned(), "service".to_owned()].as_slice())
    );
}

fn client(engine: &FakeEngine) -> Client {
    let config = Config::unix(engine.socket().to_string_lossy())
        .api_version("1.51")
        .unwrap()
        .user_agent("swarm-scheduler-exporter/test");
    Client::new(config)
        .unwrap()
        .with_events_grace(Duration::from_millis(150))
        .with_probe_timeout(Duration::from_millis(300))
}

fn wire(engine: &FakeEngine) -> Vec<String> {
    engine
        .requests()
        .iter()
        .map(|request| format!("{} {}", request.method, request.target))
        .collect()
}

const SERVICE: &str =
    r#"{"ID":"svc-api","Spec":{"Name":"web_api","Mode":{"Replicated":{"Replicas":3}}}}"#;
const EVENT: &str = r#"{"Type":"service","Action":"update","Actor":{"ID":"svc-api"},"time":1789902300,"timeNano":1789902300200000000}"#;

/// The calls send the requests the Go exporter sent, under the pinned version's prefix and with
/// the exporter's `User-Agent`.
#[tokio::test]
async fn the_calls_send_the_requests_of_the_go_exporter() {
    let engine = FakeEngine::start(&[
        ("/services", Reply::ok(&format!("[{SERVICE}]"))),
        ("/services/svc-api", Reply::ok(SERVICE)),
        ("/nodes", Reply::ok(r#"[{"ID":"node-1"}]"#)),
        ("/tasks", Reply::ok(r#"[{"ID":"t","ServiceID":"svc-api"}]"#)),
        (
            "/containers/json",
            Reply::ok(r#"[{"Id":"c-1","State":"running"}]"#),
        ),
        (
            "/containers/c-1/json",
            Reply::ok(r#"{"State":{"ExitCode":3}}"#),
        ),
    ])
    .unwrap();
    let client = client(&engine);
    assert_eq!(client.service_list().await.unwrap()[0].id, "svc-api");
    assert_eq!(client.node_list().await.unwrap()[0].id, "node-1");
    let service = client.service_inspect(" svc-api ").await.unwrap();
    assert_eq!(service.spec.mode.replicated.unwrap().replicas, Some(3));
    assert_eq!(client.task_list().await.unwrap()[0].service_id, "svc-api");
    assert_eq!(client.container_list().await.unwrap()[0].state, "running");
    let inspect = client.container_inspect("c-1").await.unwrap();
    assert_eq!(inspect.state.unwrap().exit_code, 3);
    assert_eq!(
        wire(&engine),
        [
            "GET /v1.51/services",
            "GET /v1.51/nodes",
            "GET /v1.51/services/svc-api",
            "GET /v1.51/tasks",
            "GET /v1.51/containers/json?all=true&size=false",
            "GET /v1.51/containers/c-1/json",
        ]
    );
    for request in engine.requests() {
        assert_eq!(
            request.header("user-agent"),
            Some("swarm-scheduler-exporter/test")
        );
    }
}

/// An answer of 404 is `ErrorKind::NotFound`, which the reconciler reads as "the service is
/// gone"; other statuses are not.
#[tokio::test]
async fn a_404_is_not_found_and_a_500_is_not() {
    let engine = FakeEngine::start(&[("/nodes", Reply::error(500, "boom"))]).unwrap();
    let client = client(&engine);
    let gone = client.service_inspect("svc-gone").await.unwrap_err();
    assert_eq!(gone.kind(), ErrorKind::NotFound);
    assert!(gone.is_not_found());
    let failed = client.node_list().await.unwrap_err();
    assert_eq!(failed.kind(), ErrorKind::Other);
    assert!(!failed.is_not_found());
    assert!(failed.to_string().contains("boom"), "{failed}");
}

/// An empty ID is refused before any request, as moby refuses it.
#[tokio::test]
async fn an_empty_id_sends_nothing() {
    let engine = FakeEngine::start(&[]).unwrap();
    let client = client(&engine);
    let error = client.service_inspect("  ").await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid service name or ID: value is empty"
    );
    let error = client.container_inspect("").await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid container name or ID: value is empty"
    );
    assert!(engine.requests().is_empty());
}

/// A body that stalls after the headers (bollard's own timeout is met) is cut by the caller's
/// bound, with the bare `context deadline exceeded`.
#[tokio::test]
async fn a_stalled_body_is_cut_by_the_callers_timeout() {
    let engine = FakeEngine::start(&[("/nodes", Reply::Stalled)]).unwrap();
    let client = client(&engine);
    let started = tokio::time::Instant::now();
    let error = with_bare_timeout(Duration::from_millis(200), client.node_list())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.to_string(), "context deadline exceeded");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Without a pinned version the first call asks the engine for its own (`GET /version`, itself
/// unversioned), once, and every later request carries it; a pinned one is used as it is, with
/// no such request.
#[tokio::test]
async fn the_api_version_is_negotiated_once_unless_pinned() {
    let engine = FakeEngine::start(&[
        (
            "/version",
            Reply::ok(r#"{"ApiVersion":"1.43","MinAPIVersion":"1.24"}"#),
        ),
        ("/nodes", Reply::ok("[]")),
    ])
    .unwrap();
    let client = Client::new(Config::unix(engine.socket().to_string_lossy())).unwrap();
    client.node_list().await.unwrap();
    client.node_list().await.unwrap();
    assert_eq!(
        wire(&engine),
        ["GET /version", "GET /v1.43/nodes", "GET /v1.43/nodes"]
    );
}

/// The stream asks for the events since the resume point, filtered to the types given.
#[tokio::test]
async fn events_send_since_and_filters() {
    let engine = FakeEngine::start(&[("/events", Reply::events(&[EVENT]))]).unwrap();
    let client = client(&engine);
    let since = Timestamp::from_nanosecond(1_789_902_299_700_000_000).unwrap();
    let filters = Filters::new().add("type", &["service", "node"]);
    let mut stream = client.events(since, &filters).await.unwrap();
    let event = stream.next().await.unwrap();
    assert_eq!(
        (
            event.kind.as_str(),
            event.action.as_str(),
            event.actor.id.as_str()
        ),
        ("service", "update", "svc-api")
    );
    assert_eq!(event.time_nano, 1_789_902_300_200_000_000);
    let requests = engine.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path(), "/events");
    let query = requests[0].query();
    assert!(query.contains("since=1789902299.700000000"), "{query}");
    assert!(
        query.contains("filters=%7B%22type%22%3A%5B%22node%22%2C%22service%22%5D%7D"),
        "{query}"
    );
}

/// A stream the engine closes after its last event ends with a clean EOF, which the listener
/// logs at info and reconnects from; one that was closed before any event is connected, then EOF.
#[tokio::test]
async fn a_closed_stream_ends_with_eof_after_its_events() {
    let engine = FakeEngine::start(&[("/events", Reply::events(&[EVENT, EVENT]))]).unwrap();
    let client = client(&engine);
    let mut stream = client
        .events(Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap();
    stream.next().await.unwrap();
    stream.next().await.unwrap();
    let end = stream.next().await.unwrap_err();
    assert!(end.is_eof(), "{end}");
    assert!(!engine.requests()[0].query().contains("filters"));

    let engine = FakeEngine::start(&[("/events", Reply::events(&[]))]).unwrap();
    let client = self::client(&engine);
    let mut stream = client
        .events(Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap();
    assert!(stream.next().await.unwrap_err().is_eof());
}

/// A refused connect (a non-2xx answer, no socket) yields no stream: Q6, the stream counts as
/// connected only once the engine has accepted the request.
#[tokio::test]
async fn a_refused_events_request_yields_no_stream() {
    let engine = FakeEngine::start(&[("/events", Reply::error(500, "no events today"))]).unwrap();
    let client = client(&engine);
    let error = client
        .events(Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no events today"), "{error}");
}

/// A quiet stream, headers sent and nothing after, is connected once the grace has passed and
/// the daemon has answered the probe.
#[tokio::test]
async fn a_quiet_stream_is_connected_once_the_daemon_answers_the_probe() {
    let engine = FakeEngine::start(&[
        ("/events", Reply::Stalled),
        ("/version", Reply::ok(r#"{"ApiVersion":"1.51"}"#)),
    ])
    .unwrap();
    let client = client(&engine);
    let started = tokio::time::Instant::now();
    let stream = client.events(Timestamp::UNIX_EPOCH, &Filters::new()).await;
    assert!(stream.is_ok());
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(
        wire(&engine)
            .iter()
            .any(|line| line == "GET /v1.51/version")
    );
}

/// A daemon that accepts the connection and never answers, to the stream and to the probe alike,
/// is a failed connect: no stream, so no "connected", no backoff reset and no resync.
#[tokio::test]
async fn a_daemon_that_never_answers_is_a_failed_connect() {
    let engine =
        FakeEngine::start(&[("/events", Reply::Stalled), ("/version", Reply::Stalled)]).unwrap();
    let client = client(&engine);
    let started = tokio::time::Instant::now();
    let error = client
        .events(Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout, "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A refused negotiation (a socket proxy that does not forward `/version`) is logged and the
/// calls go on with the client's own version, which then carries no prefix (docker-client sends
/// the prefix only for a pinned or negotiated version).
#[tokio::test]
async fn a_refused_negotiation_keeps_the_default_version() {
    let engine = FakeEngine::start(&[
        ("/version", Reply::error(403, "Forbidden")),
        ("/nodes", Reply::ok("[]")),
    ])
    .unwrap();
    let client = Client::new(Config::unix(engine.socket().to_string_lossy())).unwrap();
    assert!(client.node_list().await.unwrap().is_empty());
    assert!(client.node_list().await.unwrap().is_empty());
    assert_eq!(wire(&engine), ["GET /version", "GET /nodes", "GET /nodes"]);
}

/// A negotiation that cannot reach the engine fails the call, which the next one retries.
#[tokio::test]
async fn an_unreachable_engine_fails_the_call_and_is_retried() {
    let dir = std::env::temp_dir().join(format!("sse-no-socket-{}", std::process::id()));
    let socket = dir.join("docker.sock");
    // `Client::new` accepts a socket that is not there yet, as a daemon not up at startup.
    let client = Client::new(Config::unix(socket.to_string_lossy())).unwrap();
    let first = client.node_list().await.unwrap_err();
    assert_eq!(first.kind(), ErrorKind::Unavailable, "{first}");
    assert!(client.node_list().await.is_err());
}
