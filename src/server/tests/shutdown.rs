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

//! The listener and the drain on real loopback sockets: Go's `serveHTTP` and `serveUntilDone`
//! tests, Go's shutdown probe, `SO_REUSEADDR`, and how `net.Listen` binds.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::TcpStream;
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;

use super::*;

/// How long a test waits for something that should happen at once.
const BOUND: Duration = Duration::from_secs(5);

async fn get(stream: &mut TcpStream, path: &str) -> String {
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = vec![0_u8; 4096];
    let read = stream.read(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf[..read]).into_owned()
}

/// Go's shutdown probe: after the signal new connects are refused, an idle keep-alive
/// connection is closed at once, and the request in flight is answered, with `Connection:
/// close`, before the drain ends.
#[tokio::test]
async fn the_drain_waits_for_the_request_in_flight_and_refuses_new_ones() {
    let listener = bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        Arc::new(slow_app(Duration::from_millis(600))),
        Timeouts::EXPORTER,
        async {
            let _ = stopped.await;
        },
        || Instant::now() + Duration::from_secs(10),
    ));
    let mut idle = TcpStream::connect(address).await.unwrap();
    assert!(
        get(&mut idle, "/healthz")
            .await
            .starts_with("HTTP/1.1 200 OK")
    );
    let mut in_flight = TcpStream::connect(address).await.unwrap();
    in_flight
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let signalled = Instant::now();
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let refused = TcpStream::connect(address).await.unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    let mut rest = Vec::new();
    assert_eq!(
        idle.read_to_end(&mut rest).await.unwrap(),
        0,
        "the idle keep-alive closed at once"
    );
    assert!(
        !served.is_finished(),
        "the drain must wait for the request in flight"
    );
    let mut response = Vec::new();
    in_flight.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("\r\nConnection: close\r\n"), "{response}");
    let served = served.await.unwrap();
    assert_eq!(served.drained, Drained::Clean);
    assert!(served.error.is_none());
    assert!(signalled.elapsed() < Duration::from_millis(900));
}

/// The drain gives up at the deadline it is handed (Q8: one budget), not after a grace period
/// of its own.
#[tokio::test]
async fn the_drain_stops_at_the_deadline() {
    let listener = bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        Arc::new(slow_app(Duration::from_secs(30))),
        Timeouts::EXPORTER,
        async {
            let _ = stopped.await;
        },
        || Instant::now() + Duration::from_millis(300),
    ));
    let mut in_flight = TcpStream::connect(address).await.unwrap();
    in_flight
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let signalled = Instant::now();
    stop.send(()).unwrap();
    assert_eq!(served.await.unwrap().drained, Drained::DeadlineExceeded);
    let took = signalled.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_millis(450),
        "{took:?}"
    );
}

/// `SO_REUSEADDR` is on, read back with getsockopt: on the Go-style bind, and on tokio's own
/// bind too, which mio sets on Unix.
#[tokio::test]
async fn listeners_have_so_reuseaddr() {
    let tokio_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(
        socket2::SockRef::from(&tokio_listener)
            .reuse_address()
            .unwrap()
    );
    let listener = bind("127.0.0.1:0").await.unwrap();
    assert!(socket2::SockRef::from(&listener).reuse_address().unwrap());
}

/// Whether a socket on local `port` is in `TIME_WAIT`, from `/proc/net/tcp`.
fn in_time_wait(port: u16) -> bool {
    let table = std::fs::read_to_string("/proc/net/tcp").unwrap();
    let local = format!(":{port:04X}");
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields
            .get(1)
            .is_some_and(|address| address.ends_with(&local))
            && fields.get(3) == Some(&"06")
    })
}

/// A restart rebinds while the old server-side connection is in `TIME_WAIT`; a socket without
/// `SO_REUSEADDR` cannot (EADDRINUSE).
#[tokio::test]
async fn a_rebind_succeeds_with_a_connection_in_time_wait() {
    let first = bind("127.0.0.1:0").await.unwrap();
    let address = first.local_addr().unwrap();
    // The server closes first, so its side of the connection goes to TIME_WAIT.
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink).await;
    });
    let (accepted, _) = first.accept().await.unwrap();
    drop(accepted);
    client.await.unwrap();
    drop(first);
    assert!(in_time_wait(address.port()), "no TIME_WAIT entry");

    let control = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    let error = control.bind(&address.into()).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(98),
        "EADDRINUSE without SO_REUSEADDR"
    );
    drop(bind(&address.to_string()).await.unwrap());
    drop(TcpListener::bind(address).await.unwrap());
}

/// A busy port, a missing port and a bad port fail as Go's `net.Listen` does, wrapped
/// `http listen: `; tokio's own bind cannot even read Go's `:port`.
#[tokio::test]
async fn a_bind_failure_is_go_s_error() {
    let busy = bind("127.0.0.1:0").await.unwrap();
    let busy_address = busy.local_addr().unwrap();
    assert_eq!(
        bind(&busy_address.to_string())
            .await
            .unwrap_err()
            .to_string(),
        format!("http listen: listen tcp {busy_address}: bind: address already in use")
    );
    assert_eq!(
        bind("127.0.0.1").await.unwrap_err().to_string(),
        "http listen: listen tcp: address 127.0.0.1: missing port in address"
    );
    assert_eq!(
        bind("127.0.0.1:70000").await.unwrap_err().to_string(),
        "http listen: listen tcp: address 70000: invalid port"
    );
    assert_eq!(
        bind("127.0.0.1:nosuchsvc").await.unwrap_err().to_string(),
        "http listen: listen tcp: lookup tcp/nosuchsvc: unknown port"
    );
    let error = TcpListener::bind(":8888").await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "failed to lookup address information: Name or service not known"
    );
}

/// Whether the host can make IPv6 sockets at all.
fn ipv6_available() -> bool {
    Socket::new(Domain::IPV6, Type::STREAM, None).is_ok()
}

/// `:port` is the dual-stack wildcard, as Go's is: an IPv4 client reaches it.
#[tokio::test]
async fn an_empty_host_is_dual_stack() {
    let listener = bind(":0").await.unwrap();
    let local = listener.local_addr().unwrap();
    assert!(local.ip().is_unspecified(), "{local}");
    if ipv6_available() {
        assert!(local.is_ipv6(), "{local}");
        assert!(!socket2::SockRef::from(&listener).only_v6().unwrap());
    }
    let port = local.port();
    let accepted = tokio::spawn(async move { listener.accept().await.map(|(_, peer)| peer) });
    TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let peer = accepted.await.unwrap().unwrap();
    assert_eq!(peer.ip().to_canonical().to_string(), "127.0.0.1");
}

/// Go: `TestServeHTTP_ShutdownWaitsForInFlightRequest`: once shutdown starts, the server keeps
/// waiting for the request in flight, and returns once it is answered.
#[tokio::test]
async fn the_shutdown_waits_for_the_request_in_flight() {
    let listener = bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut app = healthy_app();
    let (route_started, route_release) = (Arc::clone(&started), Arc::clone(&release));
    app.slow = Some(Arc::new(move || {
        let (started, release) = (Arc::clone(&route_started), Arc::clone(&route_release));
        Box::pin(async move {
            started.notify_one();
            release.notified().await;
        })
    }));
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let served = tokio::spawn(async move {
        serve(
            listener,
            Arc::new(app),
            Timeouts::EXPORTER,
            shutdown.cancelled_owned(),
            || Instant::now() + Duration::from_secs(10),
        )
        .await
    });
    let response = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    });
    tokio::time::timeout(BOUND, started.notified())
        .await
        .expect("request not received");
    cancel.cancel();
    // It must keep waiting for the request in flight.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !served.is_finished(),
        "returned while a request was in flight"
    );
    release.notify_one();
    let served = tokio::time::timeout(BOUND, served).await.unwrap().unwrap();
    assert_eq!(served.drained, Drained::Clean);
    let response = tokio::time::timeout(BOUND, response)
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
}

/// Go: `TestServeUntilDone_ListenFailure`: a busy port is exit code 1, logged, and the workers
/// are stopped.
#[tokio::test]
async fn serve_until_done_fails_on_a_busy_port() {
    let busy = bind("127.0.0.1:0").await.unwrap();
    let address = busy.local_addr().unwrap().to_string();
    let root = CancellationToken::new();
    let worker_done = Arc::new(AtomicBool::new(false));
    let workers = {
        let (root, done) = (root.clone(), Arc::clone(&worker_done));
        async move {
            root.cancelled().await;
            done.store(true, Ordering::SeqCst);
        }
    };
    let (logger, sink) = capture_logs(Level::INFO);
    let mut app = healthy_app();
    app.logger = logger;
    let exit_code = tokio::time::timeout(
        BOUND,
        serve_until_done(&root, workers, &address, Arc::new(app), || {
            Instant::now() + Duration::from_secs(10)
        }),
    )
    .await
    .expect("serve_until_done did not return after a bind failure");
    assert_eq!(exit_code, 1);
    assert!(worker_done.load(Ordering::SeqCst), "the worker still runs");
    assert_eq!(
        sink.contents(),
        format!(
            "level=ERROR http server error err=\"http listen: listen tcp {address}: bind: address \
             already in use\"\n"
        )
    );
}

/// Go: `TestServeUntilDone_CancelledExitsZero`
#[tokio::test]
async fn serve_until_done_exits_zero_when_cancelled() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap().to_string();
    drop(free);
    let root = CancellationToken::new();
    let workers = {
        let root = root.clone();
        async move { root.cancelled().await }
    };
    let serving = {
        let (root, address) = (root.clone(), address.clone());
        tokio::spawn(async move {
            serve_until_done(&root, workers, &address, Arc::new(healthy_app()), || {
                Instant::now() + Duration::from_secs(10)
            })
            .await
        })
    };
    let deadline = Instant::now() + BOUND;
    while TcpStream::connect(&address).await.is_err() {
        assert!(Instant::now() < deadline, "never listened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    root.cancel();
    let exit_code = tokio::time::timeout(BOUND, serving)
        .await
        .expect("serve_until_done did not return after cancellation")
        .unwrap();
    assert_eq!(exit_code, 0);
}

/// A drain cut short by the deadline is warned about as Go's failed `Shutdown` is, and still
/// exits 0.
#[tokio::test]
async fn serve_until_done_warns_about_a_drain_past_the_deadline() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap();
    drop(free);
    let root = CancellationToken::new();
    let (logger, sink) = capture_logs(Level::INFO);
    let mut app = slow_app(Duration::from_secs(30));
    app.logger = logger;
    let serving = {
        let root = root.clone();
        let address = address.to_string();
        tokio::spawn(async move {
            serve_until_done(&root, async {}, &address, Arc::new(app), || {
                Instant::now() + Duration::from_millis(200)
            })
            .await
        })
    };
    let deadline = Instant::now() + BOUND;
    let mut stream = loop {
        if let Ok(stream) = TcpStream::connect(address).await {
            break stream;
        }
        assert!(Instant::now() < deadline, "never listened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    root.cancel();
    let exit_code = tokio::time::timeout(BOUND, serving).await.unwrap().unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(
        sink.contents(),
        "level=WARN HTTP server shutdown err=\"context deadline exceeded\"\n"
    );
}
