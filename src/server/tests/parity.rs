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

//! What adopting go-http changed: Go's service names as ports, `net.Listen`'s wildcard and
//! mapped literals, its zones and its resolver's refusal of `inet_aton` shorthands, and a read
//! deadline brought forward while a read waits. Each failed on the exporter's own server.

use tokio::net::TcpStream;

use super::*;

/// Whether the host can make IPv6 sockets at all.
fn ipv6_available() -> bool {
    socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None).is_ok()
}

/// Go's `LookupPort` reads the service names of its built-in table, without regard to case, so
/// `--listen-addr :http` is port 80, not `unknown port`; a name the table lacks still is.
#[tokio::test]
async fn go_s_service_names_are_ports() {
    assert_eq!(go_http::parse_port("http"), Ok(80));
    assert_eq!(go_http::parse_port("HTTPS"), Ok(443));
    assert_eq!(go_http::parse_port("ssh"), Ok(22));
    assert_eq!(
        go_http::parse_port("domain"),
        Err("lookup tcp/domain: unknown port".to_owned())
    );
    // Port 80 itself may be refused or busy here, but never read as an unknown port.
    if let Err(error) = bind("127.0.0.1:http").await {
        let error = error.to_string();
        assert!(
            error.starts_with("http listen: listen tcp 127.0.0.1:80: bind: "),
            "{error}"
        );
    }
}

/// `0.0.0.0`, `[::]` and `[::ffff:0.0.0.0]` are the dual-stack wildcard, as Go's
/// `favoriteAddrFamily` makes any wildcard, and a busy one is shown as `0.0.0.0:<port>`; a mapped
/// address listens as IPv4.
#[tokio::test]
async fn wildcard_and_mapped_literals_bind_as_go_s() {
    let busy = bind(":0").await.unwrap();
    let port = busy.local_addr().unwrap().port();
    for host in ["0.0.0.0", "[::]", "[::ffff:0.0.0.0]"] {
        assert_eq!(
            bind(&format!("{host}:{port}"))
                .await
                .unwrap_err()
                .to_string(),
            format!("http listen: listen tcp 0.0.0.0:{port}: bind: address already in use"),
            "{host}"
        );
    }
    drop(busy);
    for address in ["0.0.0.0:0", "[::]:0", "[::ffff:0.0.0.0]:0"] {
        let listener = bind(address).await.unwrap();
        let local = listener.local_addr().unwrap();
        assert!(local.ip().is_unspecified(), "{address}: {local}");
        if ipv6_available() {
            assert!(local.is_ipv6(), "{address}: {local}");
            assert!(!socket2::SockRef::from(&listener).only_v6().unwrap());
            // An IPv4 client reaches the default `0.0.0.0:8888` form too.
            let port = local.port();
            let accepted = tokio::spawn(async move { listener.accept().await.map(|_| ()) });
            TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            accepted.await.unwrap().unwrap();
        }
    }
    let mapped = bind("[::ffff:127.0.0.1]:0").await.unwrap();
    assert_eq!(mapped.local_addr().unwrap().ip().to_string(), "127.0.0.1");
}

/// Go's resolver looks `inet_aton`'s shorthands up as names and finds nothing, where
/// getaddrinfo would bind `127.1` as `127.0.0.1` and `0` as the wildcard; an IPv6 zone is an
/// interface name, else Go's number of it, else 0, and is shown as given.
#[tokio::test]
async fn shorthands_and_zones_resolve_as_go_s() {
    for host in ["0", "127.1", "0x7f.0.0.1", "010.0.0.1"] {
        assert_eq!(
            bind(&format!("{host}:0")).await.unwrap_err().to_string(),
            format!("http listen: listen tcp: lookup {host}: no such host")
        );
    }
    if !ipv6_available() {
        return;
    }
    let listener = bind("[::1%lo]:0").await.unwrap();
    assert_eq!(listener.local_addr().unwrap().ip().to_string(), "::1");
    for (address, cause) in [
        ("[fe80::1%lo]:0", "cannot assign requested address"),
        ("[fe80::1%nosuchif]:0", "invalid argument"),
        ("[fe80::1%999]:0", "no such device"),
    ] {
        assert_eq!(
            bind(address).await.unwrap_err().to_string(),
            format!("http listen: listen tcp {address}: bind: {cause}")
        );
    }
}

/// A read is pending under the first request's read deadline (10 s) when its response ends with
/// the next request's head already read: that head is due `ReadHeaderTimeout` (5 s) after the
/// response, at 6 s, and the read waiting since before must fail then, not when its own 10 s
/// timer fires. Through hyper the case is already cut at 6 s (it polls the read side again after
/// a response); this is the read hyper left pending, on the exporter's timeouts.
#[tokio::test(start_paused = true)]
async fn an_earlier_deadline_set_while_a_read_waits_is_enforced_at_once() {
    let start = Instant::now();
    let (mut client, server) = tokio::io::duplex(1024);
    let state = ConnState::new(TIMEOUTS);
    let mut io = TimedIo::new(server, state.clone());
    let pipelined = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nGET /hea";
    client.write_all(pipelined).await.unwrap();
    let mut buf = [0_u8; 1024];
    assert_eq!(io.read(&mut buf).await.unwrap(), pipelined.len());
    state.on_request_head();
    let reading = tokio::spawn(async move {
        let mut buf = [0_u8; 64];
        let result = io.read(&mut buf).await;
        (result.map_err(|error| error.kind()), start.elapsed())
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    state.on_response_done();
    let (result, elapsed) = reading.await.unwrap();
    assert_eq!(result, Err(io::ErrorKind::TimedOut));
    assert_eq!(elapsed, Duration::from_secs(6));
    drop(client);
}
