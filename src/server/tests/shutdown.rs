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

//! The stop and drain sequence: an accept error that stops the server at once, the listen
//! failure exit, and `serve_until_done`'s shared deadline for the drain and the worker wait.

use std::sync::atomic::{AtomicBool, Ordering};

use axum::routing::get;

use super::*;
use crate::test_support::capture_logs;
use tracing::level_filters::LevelFilter;

/// How long a test waits for something that should happen at once.
const BOUND: Duration = Duration::from_secs(5);

/// The exporter's routes plus a `/slow` one, which waits `duration` before it answers.
fn slow_app(duration: Duration) -> Router {
    healthy_app().route(
        "/slow",
        get(move || async move {
            tokio::time::sleep(duration).await;
            "slow done\n"
        }),
    )
}

/// A free loopback address: bound, then released.
fn free_address() -> SocketAddr {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    free.local_addr().unwrap()
}

async fn wait_listening(address: SocketAddr) {
    let deadline = Instant::now() + BOUND;
    while TcpStream::connect(address).await.is_err() {
        assert!(Instant::now() < deadline, "never listened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// An accept error that is not transient stops the server at once, with the caller's stop still
/// pending: the listener closes, the drain runs, and the error is returned. A listening socket
/// shut down for reading fails its accept with `EINVAL` on Linux.
#[tokio::test]
async fn a_fatal_accept_error_stops_the_server_at_once() {
    let listener = bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    socket2::SockRef::from(&listener)
        .shutdown(std::net::Shutdown::Read)
        .unwrap();
    let lifecycle = Lifecycle::new();
    let outcome = tokio::time::timeout(
        BOUND,
        serve(listener, healthy_app(), &lifecycle, Duration::from_secs(10)),
    )
    .await
    .expect("serve did not return after the accept error");
    assert!(matches!(outcome, Outcome::ListenerFailed(_)), "{outcome:?}");
    assert_eq!(
        TcpStream::connect(address).await.unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
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
    let logs = capture_logs(LevelFilter::INFO);
    let exit_code = tokio::time::timeout(
        BOUND,
        serve_until_done(&root, workers, &address, healthy_app(), || {
            Instant::now() + Duration::from_secs(10)
        }),
    )
    .await
    .expect("serve_until_done did not return after a bind failure");
    assert_eq!(exit_code, 1);
    assert!(worker_done.load(Ordering::SeqCst), "the worker still runs");
    assert_eq!(logs.lines().len(), 1, "{}", logs.dump());
    let line = logs
        .find("ERROR", "http server error", &[])
        .unwrap_or_else(|| panic!("{}", logs.dump()));
    let error = line["error"].as_str().unwrap();
    assert!(
        error.starts_with("http listen: listen tcp ") && error.contains("Address already in use"),
        "{error}"
    );
}

/// A requested shutdown exits 0, and the server stops listening.
#[tokio::test]
async fn serve_until_done_stops_on_cancellation() {
    let address = free_address();
    let root = CancellationToken::new();
    let serving = {
        let root = root.clone();
        tokio::spawn(async move {
            serve_until_done(&root, async {}, &address.to_string(), healthy_app(), || {
                Instant::now() + Duration::from_secs(10)
            })
            .await
        })
    };
    wait_listening(address).await;
    root.cancel();
    let exit_code = tokio::time::timeout(BOUND, serving)
        .await
        .expect("serve_until_done did not return after cancellation")
        .unwrap();
    assert_eq!(exit_code, 0);
    assert!(TcpStream::connect(address).await.is_err());
}

/// A request in flight at the stop is answered before the deadline, with nothing warned.
#[tokio::test]
async fn a_request_in_flight_finishes_within_the_drain() {
    let address = free_address();
    let root = CancellationToken::new();
    let logs = capture_logs(LevelFilter::INFO);
    let serving = {
        let root = root.clone();
        tokio::spawn(async move {
            serve_until_done(
                &root,
                async {},
                &address.to_string(),
                slow_app(Duration::from_millis(300)),
                || Instant::now() + Duration::from_secs(5),
            )
            .await
        })
    };
    wait_listening(address).await;
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    root.cancel();
    let mut answer = Vec::new();
    tokio::time::timeout(BOUND, stream.read_to_end(&mut answer))
        .await
        .expect("no answer")
        .unwrap();
    let answer = String::from_utf8(answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("connection: close"), "{answer}");
    assert!(answer.ends_with("slow done\n"), "{answer}");
    let exit_code = tokio::time::timeout(BOUND, serving).await.unwrap().unwrap();
    assert_eq!(exit_code, 0);
    assert!(logs.lines().is_empty(), "{}", logs.dump());
}

/// A drain cut short by the deadline is warned about as Go's failed `Shutdown` is, still exits 0,
/// and the request that did not finish gets no answer.
#[tokio::test]
async fn serve_until_done_warns_about_a_drain_past_the_deadline() {
    let address = free_address();
    let root = CancellationToken::new();
    let logs = capture_logs(LevelFilter::INFO);
    let serving = {
        let root = root.clone();
        tokio::spawn(async move {
            serve_until_done(
                &root,
                async {},
                &address.to_string(),
                slow_app(Duration::from_secs(30)),
                || Instant::now() + Duration::from_millis(200),
            )
            .await
        })
    };
    wait_listening(address).await;
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled = Instant::now();
    root.cancel();
    let exit_code = tokio::time::timeout(BOUND, serving).await.unwrap().unwrap();
    assert_eq!(exit_code, 0);
    assert!(
        cancelled.elapsed() < Duration::from_secs(3),
        "the drain is cut near the 200ms deadline, not the 30s request"
    );
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&answer).contains("slow done"),
        "{answer:?}"
    );
    assert_eq!(logs.lines().len(), 1, "{}", logs.dump());
    assert!(
        logs.contains(
            "WARN",
            "HTTP server shutdown",
            &[("error", "context deadline exceeded")]
        ),
        "{}",
        logs.dump()
    );
}

/// The worker wait shares the drain's deadline: workers that never return are abandoned at it,
/// measured from the stop, not from the end of the drain.
#[tokio::test(start_paused = true)]
async fn the_worker_wait_shares_the_deadline() {
    let address = free_address();
    let root = CancellationToken::new();
    let start = Instant::now();
    let serving = {
        let root = root.clone();
        tokio::spawn(async move {
            serve_until_done(
                &root,
                std::future::pending::<()>(),
                &address.to_string(),
                healthy_app(),
                || Instant::now() + Duration::from_secs(10),
            )
            .await
        })
    };
    wait_listening(address).await;
    root.cancel();
    let exit_code = serving.await.unwrap();
    assert_eq!(exit_code, 0);
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(11),
        "{elapsed:?}"
    );
}
