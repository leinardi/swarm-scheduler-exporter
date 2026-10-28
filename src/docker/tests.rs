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

//! The Docker client against a fake daemon: ports of the Go client tests, Q5, and one test per
//! S1 negotiation case.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::BodyExt as _;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::RootCertStore;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject as _;
use socket2::SockRef;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, UnixListener};
use tokio_rustls::TlsAcceptor;

use super::client::Client;
use super::pool::Pool;
use super::version::validate_client_api_version;
use super::{DEFAULT_DOCKER_HOST, ErrorKind, PathError, Settings, tls};

fn find_source<T: std::error::Error + 'static>(error: &dyn std::error::Error) -> Option<&T> {
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(cause) = cause.downcast_ref::<T>() {
            return Some(cause);
        }
        source = cause.source();
    }
    None
}

#[tokio::test]
async fn request_decode_errors_keep_the_json_cause_when_cloned() {
    use super::DockerApi as _;

    let daemon = FakeDaemon::tcp(|_| ok(&[], r#"[{"ID":5}]"#)).await;
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    let error = client.node_list().await.unwrap_err();
    let cloned = error.clone();
    assert_eq!(error.kind(), ErrorKind::Decode);
    let cause = find_source::<serde_json::Error>(&error).expect("original JSON error");
    let cloned_cause = find_source::<serde_json::Error>(&cloned).expect("shared JSON error");
    assert!(cause.is_data());
    assert!(std::ptr::eq(cause, cloned_cause));
}

#[tokio::test]
async fn connection_errors_keep_the_original_io_cause() {
    use super::DockerApi as _;

    let dir = TempDir::new();
    let host = format!("unix://{}/absent.sock", dir.str());
    let client = Client::new(settings(&[
        ("DOCKER_HOST", &host),
        ("DOCKER_API_VERSION", "1.56"),
    ]));
    let error = client.node_list().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    let cause = find_source::<std::io::Error>(&error).expect("original connect error");
    assert_eq!(cause.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn exchange_errors_keep_the_original_hyper_cause() {
    use super::DockerApi as _;

    let daemon = FakeDaemon::tcp(|_| Reply::Reset).await;
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    let error = client.node_list().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert!(find_source::<hyper::Error>(&error).is_some());
}

#[tokio::test]
async fn gzip_body_errors_keep_the_original_io_cause() {
    use super::DockerApi as _;

    let daemon = FakeDaemon::tcp(|_| ok(&[("Content-Encoding", "gzip")], "invalid gzip")).await;
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    let error = client.node_list().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Body);
    assert!(error.to_string().starts_with("gzip: "));
    assert!(find_source::<std::io::Error>(&error).is_some());
}

#[tokio::test]
async fn response_decode_errors_keep_the_original_json_cause() {
    use super::DockerApi as _;

    let daemon =
        FakeDaemon::tcp(|_| respond(500, &[("Content-Type", "application/json")], "{")).await;
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    let error = client.node_list().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(
        error.status(),
        Some(hyper::StatusCode::INTERNAL_SERVER_ERROR)
    );
    assert!(error.to_string().starts_with("error reading JSON: "));
    let cause = find_source::<serde_json::Error>(&error).expect("original response decode error");
    assert!(cause.is_eof());
}

/// The environment the client reads, from pairs; anything else is unset.
fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| vars.get(name).cloned()
}

fn settings(vars: &[(&str, &str)]) -> Settings {
    Settings::from_env(env(vars)).unwrap()
}

/// A temporary directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sse-docker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn str(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A request the fake daemon received.
#[derive(Debug, Clone)]
struct Recorded {
    connection: usize,
    method: String,
    target: String,
    head: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Vec<String> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(": "))
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.to_owned())
            .collect()
    }
}

/// What the fake daemon does with a request.
#[derive(Debug, Clone)]
enum Reply {
    /// A response with a `Content-Length` (no body written for `HEAD`).
    Respond {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    },
    /// A TCP reset (`SO_LINGER` 0), before any response.
    Reset,
    /// The connection closed after the response, unannounced, as an idle timeout closes it.
    RespondThenClose(Box<Reply>),
    /// Never an answer.
    Silent,
    /// `Reply` after a delay.
    After(Duration, Box<Reply>),
}

fn ok(headers: &[(&'static str, &str)], body: &str) -> Reply {
    respond(200, headers, body)
}

fn respond(status: u16, headers: &[(&'static str, &str)], body: &str) -> Reply {
    Reply::Respond {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
    }
}

type Script = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

/// A daemon answering by script, recording every request.
struct FakeDaemon {
    requests: Arc<Mutex<Vec<Recorded>>>,
    host: String,
    _dir: Option<TempDir>,
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

impl FakeDaemon {
    async fn tcp(script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        Self::start(
            Listener::Tcp(TcpListener::bind("127.0.0.1:0").await.unwrap()),
            None,
            None,
            Arc::new(script),
        )
    }

    fn unix(script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        let dir = TempDir::new();
        let path = dir.path().join("docker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        Self::start(Listener::Unix(listener), Some(dir), None, Arc::new(script))
    }

    async fn tls(
        acceptor: TlsAcceptor,
        script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self::start(
            Listener::Tcp(listener),
            None,
            Some(acceptor),
            Arc::new(script),
        )
    }

    fn start(
        listener: Listener,
        dir: Option<TempDir>,
        acceptor: Option<TlsAcceptor>,
        script: Script,
    ) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let host = match (&listener, &dir) {
            (Listener::Tcp(tcp), _) => format!("tcp://{}", tcp.local_addr().unwrap()),
            (Listener::Unix(_), Some(dir)) => {
                format!("unix://{}", dir.path().join("docker.sock").display())
            }
            (Listener::Unix(_), None) => unreachable!(),
        };
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            let connections = AtomicUsize::new(0);
            loop {
                let id = connections.fetch_add(1, Ordering::SeqCst);
                let script = Arc::clone(&script);
                let recorded = Arc::clone(&recorded);
                match &listener {
                    Listener::Tcp(tcp) => {
                        let Ok((stream, _)) = tcp.accept().await else {
                            return;
                        };
                        let acceptor = acceptor.clone();
                        tokio::spawn(async move {
                            let reset = SockRef::from(&stream).try_clone().ok();
                            let outcome = match acceptor {
                                Some(acceptor) => match acceptor.accept(stream).await {
                                    Ok(tls) => serve(tls, id, &script, &recorded).await,
                                    Err(_) => return,
                                },
                                None => serve(stream, id, &script, &recorded).await,
                            };
                            if matches!(outcome, Outcome::Reset)
                                && let Some(socket) = reset
                            {
                                let _ = socket.set_linger(Some(Duration::ZERO));
                            }
                        });
                    }
                    Listener::Unix(unix) => {
                        let Ok((stream, _)) = unix.accept().await else {
                            return;
                        };
                        tokio::spawn(async move {
                            serve(stream, id, &script, &recorded).await;
                        });
                    }
                }
            }
        });
        Self {
            requests,
            host,
            _dir: dir,
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// `METHOD target` of every request, in order.
    fn wire(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| format!("{} {}", request.method, request.target))
            .collect()
    }

    fn client(&self, vars: &[(&str, &str)]) -> Client {
        let mut all = vec![("DOCKER_HOST", self.host.as_str())];
        all.extend_from_slice(vars);
        Client::new(settings(&all))
    }
}

enum Outcome {
    Closed,
    Reset,
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    connection: usize,
    script: &Script,
    recorded: &Mutex<Vec<Recorded>>,
) -> Outcome {
    let mut buffer = Vec::new();
    loop {
        let head = loop {
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                buffer.drain(..end + 4);
                break head;
            }
            let mut chunk = [0_u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return Outcome::Closed,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        };
        let mut request_line = head.lines().next().unwrap_or_default().split(' ');
        let request = Recorded {
            connection,
            method: request_line.next().unwrap_or_default().to_owned(),
            target: request_line.next().unwrap_or_default().to_owned(),
            head: head.clone(),
        };
        recorded.lock().unwrap().push(request.clone());
        let mut reply = script(&request);
        let mut close_after = false;
        loop {
            match reply {
                Reply::After(delay, then) => {
                    tokio::time::sleep(delay).await;
                    reply = *then;
                }
                Reply::RespondThenClose(then) => {
                    close_after = true;
                    reply = *then;
                }
                Reply::Respond { .. } | Reply::Reset | Reply::Silent => break,
            }
        }
        match reply {
            Reply::Respond {
                status,
                headers,
                body,
            } => {
                let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
                for (name, value) in &headers {
                    write!(out, "{name}: {value}\r\n").unwrap();
                }
                write!(out, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
                let mut bytes = out.into_bytes();
                if request.method != "HEAD" {
                    bytes.extend_from_slice(&body);
                }
                if stream.write_all(&bytes).await.is_err() {
                    return Outcome::Closed;
                }
                let _ = stream.flush().await;
                if close_after {
                    let _ = stream.shutdown().await;
                    return Outcome::Closed;
                }
            }
            Reply::Reset => return Outcome::Reset,
            Reply::Silent => std::future::pending::<()>().await,
            Reply::After(..) | Reply::RespondThenClose(_) => unreachable!(),
        }
    }
}

fn reason(status: u16) -> &'static str {
    hyper::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Unknown")
}

/// A daemon that pings with `Api-Version` (or none) and lists nodes.
fn engine(api_version: Option<&'static str>) -> impl Fn(&Recorded) -> Reply + Send + Sync {
    move |request| {
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(version) = api_version {
            headers.push(("Api-Version", version));
        }
        if request.target == "/_ping" {
            ok(&headers, "OK")
        } else {
            ok(&headers, "[]")
        }
    }
}

async fn list_nodes(client: &Client) -> Result<String, super::Error> {
    let response = client.get("/nodes", "", &[]).await?;
    let body = response.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8(body.to_vec()).unwrap())
}

/// Two node lists, as S1's driver makes them, and the wire they produce.
async fn two_calls(daemon: &FakeDaemon, vars: &[(&str, &str)]) -> (Vec<String>, String) {
    let client = daemon.client(vars);
    for _ in 0..2 {
        assert_eq!(list_nodes(&client).await.unwrap(), "[]");
    }
    (daemon.wire(), client.client_version().await)
}

/// Go: `TestNewDockerClient_DefaultHost`
#[test]
fn default_host() {
    let settings = settings(&[]);
    assert_eq!(settings.daemon_host(), DEFAULT_DOCKER_HOST);
    assert_eq!(settings.client_version(), "1.56");
}

/// Go: `TestNewDockerClient_UnixSocketFromEnv`, plus the request Go writes over a socket: the
/// `api.moby.localhost` host and no `Accept-Encoding`.
#[tokio::test]
async fn unix_socket_from_env() {
    let daemon = FakeDaemon::unix(engine(Some("1.56")));
    let client = daemon.client(&[]);
    assert_eq!(list_nodes(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(daemon.wire(), ["HEAD /_ping", "GET /v1.56/nodes"]);
    let user_agent = format!("swarm-scheduler-exporter/{}", crate::version::VERSION);
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/nodes HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: {user_agent}"
        )
    );
    assert_eq!(
        requests[0].head,
        format!("HEAD /_ping HTTP/1.1\r\nHost: api.moby.localhost\r\nUser-Agent: {user_agent}")
    );
    // One connection for both: the ping's was reused.
    assert_eq!(requests[0].connection, requests[1].connection);
}

/// The request Go writes over TCP: `host:port` as the host, Go's header case and order, and the
/// transport's `Accept-Encoding: gzip` on a `GET` (not on the `HEAD`); `Accept` values as
/// separate lines.
#[tokio::test]
async fn tcp_requests_match_go_s_wire() {
    let daemon = FakeDaemon::tcp(engine(Some("1.56"))).await;
    let client = daemon.client(&[]);
    let response = client
        .get(
            "/events",
            "since=1",
            &["application/jsonl", "application/x-ndjson;q=0.9"],
        )
        .await
        .unwrap();
    drop(response);
    let requests = daemon.requests();
    let authority = daemon.host.trim_start_matches("tcp://");
    let user_agent = format!("swarm-scheduler-exporter/{}", crate::version::VERSION);
    assert_eq!(
        requests[0].head,
        format!("HEAD /_ping HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {user_agent}")
    );
    assert_eq!(
        requests[1].head,
        format!(
            "GET /v1.56/events?since=1 HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: \
             {user_agent}\r\nAccept: application/jsonl\r\nAccept: \
             application/x-ndjson;q=0.9\r\nAccept-Encoding: gzip"
        )
    );
    assert_eq!(requests[1].header("accept").len(), 2);
}

/// Go's transport decompresses a gzip body it asked for, and drops the encoding headers.
#[tokio::test]
async fn tcp_gzip_body_is_decompressed() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"[{\"ID\":\"n1\"}]").unwrap();
    let gzipped = encoder.finish().unwrap();
    let daemon = FakeDaemon::tcp(move |request| {
        if request.target == "/_ping" {
            return ok(&[("Api-Version", "1.56")], "OK");
        }
        Reply::Respond {
            status: 200,
            headers: vec![("Content-Encoding", "gzip".to_owned())],
            body: gzipped.clone(),
        }
    })
    .await;
    let client = daemon.client(&[]);
    let response = client.get("/nodes", "", &[]).await.unwrap();
    assert!(response.headers().get("content-encoding").is_none());
    assert!(response.headers().get("content-length").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"[{\"ID\":\"n1\"}]");
}

/// Go: `TestNewDockerClient_ResponseHeaderTimeoutBoundsTheEventStream`: a daemon that accepts
/// and never answers cannot hold the event stream; the pin skips the ping, so the stream request
/// is the first one sent.
#[tokio::test]
async fn response_header_timeout_bounds_the_event_stream() {
    let daemon = FakeDaemon::tcp(|_| Reply::Silent).await;
    let settings = settings(&[
        ("DOCKER_HOST", daemon.host.as_str()),
        ("DOCKER_API_VERSION", "1.56"),
    ]);
    let client = Client::with_pool(settings, |connector| {
        Pool::with_timeout(connector, Duration::from_millis(100))
    });
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), client.get("/events", "", &[]))
        .await
        .expect("the stream to a silent daemon was still waiting for headers after 5s");
    let error = result.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        format!(
            "Cannot connect to the Docker daemon at {}. Is the docker daemon running?",
            daemon.host
        )
    );
    assert_eq!(daemon.wire(), ["GET /v1.56/events"]);
}

/// The TLS material of a test: a CA, a client certificate in `dir`, and a server configuration
/// for 127.0.0.1 that requires a client certificate from the same CA (Go's
/// `writeTestTLSMaterial`).
fn write_test_tls_material(dir: &Path) -> TlsAcceptor {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let client_key = KeyPair::generate().unwrap();
    let mut client_params = CertificateParams::new(Vec::new()).unwrap();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "test client");
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client = client_params.signed_by(&client_key, &issuer).unwrap();

    let server_key = KeyPair::generate().unwrap();
    let mut server_params = CertificateParams::new(Vec::new()).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    server_params.subject_alt_names = vec![SanType::IpAddress([127, 0, 0, 1].into())];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server = server_params.signed_by(&server_key, &issuer).unwrap();

    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("cert.pem"), client.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), client_key.serialize_pem()).unwrap();

    let mut client_roots = RootCertStore::empty();
    client_roots.add(ca.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(client_roots), Arc::clone(&provider))
            .build()
            .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![server.der().clone()],
            PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// Go: `TestNewDockerClient_TLSFromEnv`: the daemon requires a client certificate signed by the
/// CA and the client verifies the daemon against `ca.pem`, so the call only succeeds with both
/// halves of the configuration.
#[tokio::test]
async fn tls_from_env() {
    let certs = TempDir::new();
    let acceptor = write_test_tls_material(certs.path());
    let daemon = FakeDaemon::tls(acceptor, engine(Some("1.56"))).await;
    let client = daemon.client(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]);
    assert_eq!(list_nodes(&client).await.unwrap(), "[]");
    // Over TLS the Go transport still asks for gzip.
    assert_eq!(daemon.requests()[1].header("accept-encoding"), ["gzip"]);
}

/// Without the client certificate, or with a CA that did not sign the daemon's, the handshake
/// fails: verification is on.
#[tokio::test]
async fn tls_verifies_both_sides() {
    let certs = TempDir::new();
    let acceptor = write_test_tls_material(certs.path());
    let daemon = FakeDaemon::tls(acceptor, engine(Some("1.56"))).await;
    let other = TempDir::new();
    let _ = write_test_tls_material(other.path());
    // The other CA did not sign the daemon's certificate.
    std::fs::copy(
        other.path().join("ca.pem"),
        certs.path().join("ca-other.pem"),
    )
    .unwrap();
    let mixed = TempDir::new();
    for name in ["cert.pem", "key.pem"] {
        std::fs::copy(certs.path().join(name), mixed.path().join(name)).unwrap();
    }
    std::fs::copy(other.path().join("ca.pem"), mixed.path().join("ca.pem")).unwrap();
    let client = daemon.client(&[
        ("DOCKER_CERT_PATH", mixed.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]);
    let error = list_nodes(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert!(
        error
            .to_string()
            .starts_with("error during connect: Get \"https://127.0.0.1:"),
        "{error}"
    );
    assert!(
        error.to_string().contains("tls: invalid peer certificate"),
        "{error}"
    );
}

/// The trust roots are Go's `tlsconfig.Client` ones: the system pool plus `ca.pem`.
#[test]
fn tls_roots_are_the_system_pool_plus_ca_pem() {
    let certs = TempDir::new();
    let _ = write_test_tls_material(certs.path());
    let roots = tls::cert_pool(&certs.path().join("ca.pem")).unwrap();
    let mut system = RootCertStore::empty();
    let (system_count, _) =
        system.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    assert_eq!(roots.len(), system_count + 1);
    // A CA file with no certificate in it is refused, as AppendCertsFromPEM's false is.
    std::fs::write(certs.path().join("ca.pem"), "not a certificate\n").unwrap();
    let error = tls::cert_pool(&certs.path().join("ca.pem")).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "failed to append certificates from PEM file: \"{}\"",
            certs.path().join("ca.pem").display()
        )
    );
}

/// Go: `TestNewDockerClient_TLSFromEnvMissingFiles`, with `DOCKER_TLS_VERIFY` set, since Q5
/// refuses Go's unset one first: the missing `ca.pem` is reported with its path, and the
/// `io::Error` is the source.
#[test]
fn tls_from_env_missing_files() {
    let certs = TempDir::new();
    let error = Settings::from_env(env(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap_err();
    let ca = certs.path().join("ca.pem");
    assert_eq!(
        error.to_string(),
        format!(
            "configure TLS from DOCKER_CERT_PATH={}: could not read CA certificate \"{}\": open \
             {}: no such file or directory",
            certs.str(),
            ca.display(),
            ca.display()
        )
    );
    let source = std::error::Error::source(&error).expect("the path error");
    let path_error = source.downcast_ref::<PathError>().expect("a PathError");
    assert_eq!(path_error.path(), ca);
    let io = std::error::Error::source(path_error)
        .and_then(|io| io.downcast_ref::<std::io::Error>())
        .expect("the io::Error");
    assert_eq!(io.kind(), std::io::ErrorKind::NotFound);

    // With ca.pem there, the client certificate is next, under Go's key pair prefix.
    let _ = write_test_tls_material(certs.path());
    std::fs::remove_file(certs.path().join("key.pem")).unwrap();
    let error = Settings::from_env(env(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "configure TLS from DOCKER_CERT_PATH={}: could not load X509 key pair: open {}: no \
             such file or directory",
            certs.str(),
            certs.path().join("key.pem").display()
        )
    );
}

/// Q5: a certificate path with `DOCKER_TLS_VERIFY` empty is refused, with the fix in the
/// message, where Go connected without verifying the daemon. Any non-empty value, `0` included,
/// verifies, as in Go and the Docker CLI.
#[test]
fn tls_without_verify_is_refused() {
    let certs = TempDir::new();
    let _ = write_test_tls_material(certs.path());
    for verify in [None, Some("")] {
        let mut vars = vec![("DOCKER_CERT_PATH", certs.str())];
        if let Some(verify) = verify {
            vars.push(("DOCKER_TLS_VERIFY", verify));
        }
        let error = Settings::from_env(env(&vars)).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "configure TLS from DOCKER_CERT_PATH={}: DOCKER_TLS_VERIFY is empty, which would \
                 connect without verifying the daemon's certificate; set DOCKER_TLS_VERIFY=1",
                certs.str()
            )
        );
    }
    for verify in ["1", "0", "false"] {
        assert!(
            Settings::from_env(env(&[
                ("DOCKER_CERT_PATH", certs.str()),
                ("DOCKER_TLS_VERIFY", verify),
            ]))
            .is_ok(),
            "{verify}"
        );
    }
    // No certificate path: plain TCP, whatever DOCKER_TLS_VERIFY says.
    assert!(Settings::from_env(env(&[("DOCKER_TLS_VERIFY", "1")])).is_ok());
}

/// Go: `TestValidateClientAPIVersion`
#[test]
fn validate_client_api_version_cases() {
    let cases = [
        ("", true),
        ("1.39", false),
        ("1.40", true),
        ("1.56", true),
        ("1.57", false),
        ("v1.44", true),
    ];
    for (pin, valid) in cases {
        let settings = settings(&[("DOCKER_API_VERSION", pin)]);
        let result = validate_client_api_version(settings.client_version());
        assert_eq!(result.is_ok(), valid, "{pin:?}: {result:?}");
        if !valid {
            assert_eq!(
                result.unwrap_err().to_string(),
                format!(
                    "unsupported Docker API version {pin} (DOCKER_API_VERSION): supported range \
                     is 1.40 to 1.56"
                )
            );
        }
    }
}

/// `DOCKER_API_VERSION` is trimmed and loses one `v`; a malformed one is moby's error, under
/// the Go exporter's `docker client:` prefix.
#[test]
fn api_version_from_env_follows_moby() {
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", " v1.45 ")]).client_version(),
        "1.45"
    );
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", "01.050")]).client_version(),
        "1.50"
    );
    assert_eq!(
        settings(&[("DOCKER_API_VERSION", " v ")]).client_version(),
        "1.56"
    );
    for (pin, reason) in [
        ("vv1.45", "must be formatted <major>.<minor>"),
        ("1", "must be formatted <major>.<minor>"),
        (
            "x.1",
            "invalid major version: must be formatted <major>.<minor>",
        ),
        (
            "1.x",
            "invalid minor version: must be formatted <major>.<minor>",
        ),
        (
            "1.45.2",
            "invalid minor version: must be formatted <major>.<minor>",
        ),
    ] {
        assert_eq!(
            Settings::from_env(env(&[("DOCKER_API_VERSION", pin)]))
                .unwrap_err()
                .to_string(),
            format!("docker client: invalid API version ({pin}): {reason}"),
        );
    }
    assert_eq!(
        Settings::from_env(env(&[("DOCKER_HOST", "nope")]))
            .unwrap_err()
            .to_string(),
        "docker client: unable to parse docker host `nope`"
    );
}

/// S1 case 1 (moby: 2 calls, version 1.51): a `HEAD` 200 is used as is, with no `GET`, and the
/// version is negotiated once.
#[tokio::test]
async fn ping_head_200_negotiates_down() {
    let daemon = FakeDaemon::tcp(engine(Some("1.51"))).await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        ["HEAD /_ping", "GET /v1.51/nodes", "GET /v1.51/nodes"]
    );
    assert_eq!(version, "1.51");
}

/// S1 case 2 (moby: version 1.56): no `Api-Version` means 1.56.
#[tokio::test]
async fn ping_without_api_version_is_1_56() {
    let daemon = FakeDaemon::tcp(engine(None)).await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        ["HEAD /_ping", "GET /v1.56/nodes", "GET /v1.56/nodes"]
    );
    assert_eq!(version, "1.56");
}

/// S1 case 3 (moby: version 1.56, a ping before each call): 1.39 is below the minimum; moby's
/// error is discarded, the request goes out at 1.56, and the next request pings again.
#[tokio::test]
async fn ping_below_minimum_is_not_refused_and_pings_again() {
    let daemon = FakeDaemon::tcp(engine(Some("1.39"))).await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        [
            "HEAD /_ping",
            "GET /v1.56/nodes",
            "HEAD /_ping",
            "GET /v1.56/nodes"
        ]
    );
    assert_eq!(version, "1.56");
}

/// S1 case 4 (moby: version 1.51): a `HEAD` 404 falls back to `GET /_ping`.
#[tokio::test]
async fn ping_head_404_falls_back_to_get() {
    let daemon = FakeDaemon::tcp(|request| {
        if request.method == "HEAD" {
            respond(404, &[], "")
        } else {
            engine(Some("1.51"))(request)
        }
    })
    .await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        [
            "HEAD /_ping",
            "GET /_ping",
            "GET /v1.51/nodes",
            "GET /v1.51/nodes"
        ]
    );
    assert_eq!(version, "1.51");
}

/// S1 case 5 (moby: version 1.51): a reset `HEAD` falls back to `GET /_ping`.
#[tokio::test]
async fn ping_head_reset_falls_back_to_get() {
    let daemon = FakeDaemon::tcp(|request| {
        if request.method == "HEAD" {
            Reply::Reset
        } else {
            engine(Some("1.51"))(request)
        }
    })
    .await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        [
            "HEAD /_ping",
            "GET /_ping",
            "GET /v1.51/nodes",
            "GET /v1.51/nodes"
        ]
    );
    assert_eq!(version, "1.51");
}

/// S1 case 6 (moby: version 1.56, both pings before each call): `HEAD` and `GET` both 500; the
/// request still goes out at 1.56 and the next one pings again.
#[tokio::test]
async fn ping_both_failing_still_sends_and_pings_again() {
    let daemon = FakeDaemon::tcp(|request| {
        if request.target == "/_ping" {
            respond(500, &[("Api-Version", "1.51")], "boom")
        } else {
            engine(Some("1.51"))(request)
        }
    })
    .await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        [
            "HEAD /_ping",
            "GET /_ping",
            "GET /v1.56/nodes",
            "HEAD /_ping",
            "GET /_ping",
            "GET /v1.56/nodes",
        ]
    );
    assert_eq!(version, "1.56");
}

/// S1 case 7 (moby: version 1.56): a daemon above the maximum negotiates 1.56, once.
#[tokio::test]
async fn ping_above_maximum_is_1_56() {
    let daemon = FakeDaemon::tcp(engine(Some("1.60"))).await;
    let (wire, version) = two_calls(&daemon, &[]).await;
    assert_eq!(
        wire,
        ["HEAD /_ping", "GET /v1.56/nodes", "GET /v1.56/nodes"]
    );
    assert_eq!(version, "1.56");
}

/// S1 case 8 (moby: version 1.45, no ping): `DOCKER_API_VERSION=' v1.45 '` pins the version.
#[tokio::test]
async fn pinned_version_never_pings() {
    let daemon = FakeDaemon::tcp(engine(Some("1.51"))).await;
    let (wire, version) = two_calls(&daemon, &[("DOCKER_API_VERSION", " v1.45 ")]).await;
    assert_eq!(wire, ["GET /v1.45/nodes", "GET /v1.45/nodes"]);
    assert_eq!(version, "1.45");
}

/// Q11: concurrent first requests share one ping (moby sends one each, 8 for 8 in S1), whether
/// it negotiates or fails; after a failed one, the next request pings again.
#[tokio::test]
async fn concurrent_first_requests_share_one_ping() {
    let slow_ping = |version: Option<&'static str>, status: u16| {
        move |request: &Recorded| {
            if request.target == "/_ping" {
                let mut headers = Vec::new();
                if let Some(version) = version {
                    headers.push(("Api-Version", version));
                }
                Reply::After(
                    Duration::from_millis(100),
                    Box::new(respond(status, &headers, "OK")),
                )
            } else {
                ok(&[], "[]")
            }
        }
    };
    for (version, status, pings) in [(Some("1.51"), 200, 1), (None, 500, 2)] {
        let daemon = FakeDaemon::tcp(slow_ping(version, status)).await;
        let client = Arc::new(daemon.client(&[]));
        let mut calls = Vec::new();
        for _ in 0..8 {
            let client = Arc::clone(&client);
            calls.push(tokio::spawn(
                async move { list_nodes(&client).await.unwrap() },
            ));
        }
        for call in calls {
            call.await.unwrap();
        }
        let wire = daemon.wire();
        let ping_requests = wire.iter().filter(|line| line.ends_with("/_ping")).count();
        assert_eq!(ping_requests, pings, "{status}: {wire:?}");
        let want = if status == 200 {
            "GET /v1.51/nodes"
        } else {
            "GET /v1.56/nodes"
        };
        assert_eq!(
            wire.iter().filter(|line| *line == want).count(),
            8,
            "{wire:?}"
        );
        if status != 200 {
            list_nodes(&client).await.unwrap();
            let wire = daemon.wire();
            assert_eq!(
                wire[wire.len() - 3..],
                ["HEAD /_ping", "GET /_ping", "GET /v1.56/nodes"]
            );
        }
    }
}

/// A connection the daemon closed while idle is replaced without failing the request, as Go's
/// transport retries a `GET` on a reused connection.
#[tokio::test]
async fn idle_connection_closed_by_the_daemon_is_retried() {
    let daemon =
        FakeDaemon::tcp(|request| Reply::RespondThenClose(Box::new(engine(Some("1.56"))(request))))
            .await;
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    assert_eq!(list_nodes(&client).await.unwrap(), "[]");
    // Let the close reach the client, as it would after an idle timeout.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(list_nodes(&client).await.unwrap(), "[]");
    let requests = daemon.requests();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0].connection, requests[1].connection);
}

/// moby's wording of the failures, and the status of a response error.
#[tokio::test]
async fn errors_are_worded_as_moby() {
    // Nothing listening on a TCP port.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let host = format!("tcp://{address}");
    let client = Client::new(settings(&[
        ("DOCKER_HOST", &host),
        ("DOCKER_API_VERSION", "1.56"),
    ]));
    assert_eq!(
        list_nodes(&client).await.unwrap_err().to_string(),
        format!("Cannot connect to the Docker daemon at {host}. Is the docker daemon running?")
    );

    // A socket path that does not exist.
    let missing = TempDir::new();
    let path = missing.path().join("docker.sock");
    let host = format!("unix://{}", path.display());
    let client = Client::new(settings(&[
        ("DOCKER_HOST", &host),
        ("DOCKER_API_VERSION", "1.56"),
    ]));
    assert_eq!(
        list_nodes(&client).await.unwrap_err().to_string(),
        format!(
            "failed to connect to the docker API at {host}; check if the path is correct and if \
             the daemon is running: dial unix {}: connect: no such file or directory",
            path.display()
        )
    );

    // Response errors: a JSON message, a plain body, an empty body, a JSON body without message.
    let daemon = FakeDaemon::unix(|request| match request.target.as_str() {
        "/v1.56/json" => respond(
            404,
            &[("Content-Type", "application/json")],
            "{\"message\":\" no such service \"}",
        ),
        "/v1.56/plain" => respond(500, &[("Content-Type", "text/plain")], " oops\n"),
        "/v1.56/empty" => respond(404, &[], ""),
        _ => respond(500, &[("Content-Type", "application/json")], "{}"),
    });
    let client = daemon.client(&[("DOCKER_API_VERSION", "1.56")]);
    let failure = |path: &'static str| {
        let client = &client;
        async move { client.get(path, "", &[]).await.unwrap_err() }
    };
    let error = failure("/json").await;
    assert_eq!(error.kind(), ErrorKind::Response);
    assert_eq!(error.status(), Some(hyper::StatusCode::NOT_FOUND));
    assert_eq!(
        error.to_string(),
        "Error response from daemon: no such service"
    );
    assert_eq!(
        failure("/plain").await.to_string(),
        "Error response from daemon: oops"
    );
    let socket = daemon
        .host
        .trim_start_matches("unix://")
        .replace('/', "%2F");
    assert_eq!(
        failure("/empty").await.to_string(),
        format!(
            "request returned 404 Not Found for API route and version \
             http://{socket}/v1.56/empty, check if the server supports the requested API version"
        )
    );
    assert_eq!(
        failure("/nomessage").await.to_string(),
        "Error response from daemon: API returned a 500 (Internal Server Error) but provided no \
         error-message"
    );
}
