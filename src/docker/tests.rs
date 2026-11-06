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

use docker_engine::test_util::fake_engine::{Connection, FakeEngine, responder};

use super::*;

fn docker_host(host: String) -> impl Fn(&str) -> Option<String> {
    move |name| (name == "DOCKER_HOST").then(|| host.clone())
}

/// The Go exporter's `User-Agent`: the program and its version, or the program alone when the
/// version stamp is something no header may carry.
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
/// response-header timeout, which moby's client does not set.
#[test]
fn settings_from_env_are_the_go_exporter_s() {
    let getenv = docker_host("tcp://127.0.0.1:2375".to_owned());
    let settings = settings_from_env(&getenv).unwrap();
    assert_eq!(
        settings.user_agent_value(),
        format!("swarm-scheduler-exporter/{}", crate::version::VERSION)
    );
    assert_eq!(RESPONSE_HEADER_TIMEOUT, Duration::from_secs(30));
    let want = Settings::from_env(&getenv)
        .unwrap()
        .user_agent(user_agent(crate::version::VERSION))
        .unwrap()
        .response_header_timeout(Some(Duration::from_secs(30)));
    assert_eq!(settings, want);
    assert_ne!(settings, want.response_header_timeout(None));
    assert_eq!(
        settings_from_env(docker_host("daemon".to_owned()))
            .unwrap_err()
            .to_string(),
        "docker client: unable to parse docker host `daemon`"
    );
}

/// moby's `ensureReaderClosed` on the ping's answers: after a `HEAD /_ping` 404, the `GET /_ping`
/// fallback's short body is drained, so the first call reuses its connection instead of dialling
/// another.
#[tokio::test]
async fn a_get_ping_fallback_leaves_its_connection_reusable() {
    let engine = FakeEngine::tcp(
        "127.0.0.1:0",
        responder(|mut connection: Connection| async move {
            while let Some(request) = connection.read_request().await {
                let method = request.method.as_str();
                let open = match (method, request.target.as_str()) {
                    ("HEAD", "/_ping") => {
                        connection.respond("404 Not Found", &[], b"", method).await
                    }
                    ("GET", "/_ping") => {
                        connection
                            .respond("200 OK", &[("Api-Version", "1.51")], b"OK", method)
                            .await
                    }
                    _ => {
                        connection
                            .respond(
                                "200 OK",
                                &[("Content-Type", "application/json")],
                                b"[]",
                                method,
                            )
                            .await
                    }
                };
                if !open {
                    return;
                }
            }
        }),
    )
    .await
    .unwrap();
    let client = Client::new(settings_from_env(docker_host(engine.host())).unwrap());
    assert!(client.node_list().await.unwrap().is_empty());
    let requests = engine.requests();
    let wire: Vec<String> = requests
        .iter()
        .map(|request| format!("{} {}", request.method, request.target))
        .collect();
    assert_eq!(wire, ["HEAD /_ping", "GET /_ping", "GET /v1.51/nodes"]);
    let connections: Vec<usize> = requests.iter().map(|request| request.conn).collect();
    assert_eq!(connections, [1, 1, 1], "{wire:?}");
}
