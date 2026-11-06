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

//! Go's `serveUntilDone` tests, the exporter's listen errors, and the stop at once on a failed
//! accept that its `serve` adds to go-http's.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::TcpStream;
use tokio::time::Instant;

use super::*;

/// How long a test waits for something that should happen at once.
const BOUND: Duration = Duration::from_secs(5);

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

/// An accept error Go's `Serve` does not retry stops the server at once, as `Serve` returning
/// does, with the caller's stop still pending: the listener closes, the drain runs, and the
/// error is returned in Go's words. A listening socket shut down for reading fails its accept
/// with `EINVAL` on Linux.
#[tokio::test]
async fn a_fatal_accept_error_stops_the_server_at_once() {
    let listener = bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    socket2::SockRef::from(&listener)
        .shutdown(std::net::Shutdown::Read)
        .unwrap();
    let stop = CancellationToken::new();
    let (served, error) = tokio::time::timeout(
        BOUND,
        serve(
            listener,
            Arc::new(healthy_app()),
            TIMEOUTS,
            stop.cancelled(),
            || Instant::now() + Duration::from_secs(10),
        ),
    )
    .await
    .expect("serve did not return after the accept error");
    assert_eq!(served.drained, Drained::Clean);
    assert!(!stop.is_cancelled());
    assert_eq!(
        error.unwrap().to_string(),
        format!("accept tcp {address}: accept4: invalid argument")
    );
    assert_eq!(
        TcpStream::connect(address).await.unwrap_err().kind(),
        io::ErrorKind::ConnectionRefused
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
    let app = healthy_app();
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
    assert_eq!(logs.lines().len(), 1, "{}", logs.dump());
    assert!(
        logs.contains(
            "ERROR",
            "http server error",
            &[(
                "error",
                &format!("http listen: listen tcp {address}: bind: address already in use")
            )]
        ),
        "{}",
        logs.dump()
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
    let logs = capture_logs(LevelFilter::INFO);
    let app = slow_app(Duration::from_secs(30));
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
