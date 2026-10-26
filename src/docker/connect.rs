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

//! Dialing the daemon: a unix socket or TCP, optionally under TLS, each step bounded as Go's
//! transport bounds it.
//!
//! Always directly (Q14): Go's transport sent a TCP host through `HTTP_PROXY`, `HTTPS_PROXY` and
//! `NO_PROXY` (`http.ProxyFromEnvironment`), while this one reads no proxy variable. A proxy in
//! front of the daemon is named as the `DOCKER_HOST` instead.

use std::fmt;
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpStream, UnixStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::host::{Endpoint, dial_address};
use super::{DIAL_TIMEOUT, TLS_HANDSHAKE_TIMEOUT};
use crate::go_compat::errno::io_error_text;

/// Go's TCP keep-alive defaults since go1.23: probes after 15s idle, every 15s, 9 of them.
const KEEPALIVE: Duration = Duration::from_secs(15);
const KEEPALIVE_PROBES: u32 = 9;

/// Why a connection could not be made, worded as Go's `net.OpError` and net/http word it.
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// The connect system call failed.
    Dial {
        network: &'static str,
        address: String,
        error: io::Error,
    },
    /// The host name did not resolve.
    Lookup { host: String, error: io::Error },
    /// The dial did not finish within [`DIAL_TIMEOUT`].
    DialTimeout {
        network: &'static str,
        address: String,
    },
    /// The TLS handshake did not finish within [`TLS_HANDSHAKE_TIMEOUT`].
    TlsHandshakeTimeout,
    /// The TLS handshake failed.
    Tls(io::Error),
    /// The host is not a name TLS can verify.
    ServerName(String),
}

impl ConnectError {
    /// The rustls error behind a failed handshake, if that is what this is.
    pub(crate) fn tls_error(&self) -> Option<&rustls::Error> {
        match self {
            Self::Tls(error) => error.get_ref()?.downcast_ref::<rustls::Error>(),
            Self::Dial { .. }
            | Self::Lookup { .. }
            | Self::DialTimeout { .. }
            | Self::TlsHandshakeTimeout
            | Self::ServerName(_) => None,
        }
    }

    /// The `io::Error` of a failed connect.
    pub(crate) fn dial_error(&self) -> Option<&io::Error> {
        match self {
            Self::Dial { error, .. } => Some(error),
            Self::Lookup { .. }
            | Self::DialTimeout { .. }
            | Self::TlsHandshakeTimeout
            | Self::Tls(_)
            | Self::ServerName(_) => None,
        }
    }

    /// Whether Go's `net.Error.Timeout()` would be true.
    pub(crate) fn is_timeout(&self) -> bool {
        match self {
            Self::DialTimeout { .. } | Self::TlsHandshakeTimeout => true,
            Self::Dial { error, .. } => error.raw_os_error() == Some(110),
            Self::Lookup { .. } | Self::Tls(_) | Self::ServerName(_) => false,
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dial {
                network,
                address,
                error,
            } => write!(
                formatter,
                "dial {network} {address}: connect: {}",
                io_error_text(error)
            ),
            Self::Lookup { host, error } => {
                let text = error.to_string();
                let reason = if text.contains("not known") || text.contains("No address") {
                    "no such host".to_owned()
                } else {
                    text
                };
                write!(formatter, "dial tcp: lookup {host}: {reason}")
            }
            Self::DialTimeout { network, address } => {
                write!(formatter, "dial {network} {address}: i/o timeout")
            }
            Self::TlsHandshakeTimeout => formatter.write_str("net/http: TLS handshake timeout"),
            Self::Tls(error) => match error.get_ref() {
                Some(inner) => write!(formatter, "tls: {inner}"),
                None => write!(formatter, "tls: {}", io_error_text(error)),
            },
            Self::ServerName(host) => write!(formatter, "tls: invalid server name {host:?}"),
        }
    }
}

/// A connection, plain or under TLS.
#[derive(Debug)]
pub(crate) enum Stream {
    Unix(UnixStream),
    Tcp(TcpStream),
    Tls(Box<TlsStream<Stream>>),
}

macro_rules! delegate {
    ($self:ident, $stream:ident => $call:expr) => {
        match $self.get_mut() {
            Stream::Unix($stream) => {
                let $stream = Pin::new($stream);
                $call
            }
            Stream::Tcp($stream) => {
                let $stream = Pin::new($stream);
                $call
            }
            Stream::Tls($stream) => {
                let $stream = Pin::new($stream.as_mut());
                $call
            }
        }
    };
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, stream => stream.poll_read(context, buf))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, stream => stream.poll_write(context, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, stream => stream.poll_flush(context))
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, stream => stream.poll_shutdown(context))
    }
}

/// How to reach the daemon.
#[derive(Debug, Clone)]
pub(crate) struct Connector {
    endpoint: Endpoint,
    tls: Option<Arc<ClientConfig>>,
}

impl Connector {
    pub(crate) fn new(endpoint: Endpoint, tls: Option<Arc<ClientConfig>>) -> Self {
        Self { endpoint, tls }
    }

    /// A new connection: dialed within [`DIAL_TIMEOUT`], then, with TLS, handshaken within
    /// [`TLS_HANDSHAKE_TIMEOUT`] against the host name of the address (the socket path, for a
    /// unix socket, as Go's transport takes it).
    pub(crate) async fn connect(&self) -> Result<Stream, ConnectError> {
        let (plain, server_host) = match &self.endpoint {
            Endpoint::Unix(path) => (dial_unix(path).await?, path.clone()),
            Endpoint::Tcp(addr) => {
                let (host, port) = dial_address(addr, self.tls.is_some());
                (dial_tcp(&host, port).await?, host)
            }
        };
        let Some(config) = &self.tls else {
            return Ok(plain);
        };
        let server_name = ServerName::try_from(server_host.clone())
            .map_err(|_| ConnectError::ServerName(server_host))?;
        let handshake = TlsConnector::from(Arc::clone(config)).connect(server_name, plain);
        match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, handshake).await {
            Ok(Ok(tls)) => Ok(Stream::Tls(Box::new(tls))),
            Ok(Err(error)) => Err(ConnectError::Tls(error)),
            Err(_) => Err(ConnectError::TlsHandshakeTimeout),
        }
    }
}

async fn dial_unix(path: &str) -> Result<Stream, ConnectError> {
    match tokio::time::timeout(DIAL_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(stream)) => Ok(Stream::Unix(stream)),
        Ok(Err(error)) => Err(ConnectError::Dial {
            network: "unix",
            address: path.to_owned(),
            error,
        }),
        Err(_) => Err(ConnectError::DialTimeout {
            network: "unix",
            address: path.to_owned(),
        }),
    }
}

/// An IPv6 literal with a zone (`fe80::1%eth0`, from `[fe80::1%25eth0]` in `DOCKER_HOST`), which
/// the resolver does not take: the zone becomes the interface index Go's dialer gives it, the
/// index of the interface of that name, else the zone read as a number, else 0.
fn scoped_ipv6(host: &str, port: u16) -> Option<SocketAddr> {
    let (address, zone) = host.split_once('%')?;
    let address: Ipv6Addr = address.parse().ok()?;
    let scope = interface_index(zone)
        .or_else(|| zone.parse().ok())
        .unwrap_or(0);
    Some(SocketAddr::V6(SocketAddrV6::new(address, port, 0, scope)))
}

/// The index of the network interface `name`, from sysfs (Go asks netlink for the same table).
fn interface_index(name: &str) -> Option<u32> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Go's dialer: the name resolved, each address tried in turn, the whole within
/// [`DIAL_TIMEOUT`]; the first error is the one reported. The connection gets Go's `TCP_NODELAY`
/// and keep-alive settings.
async fn dial_tcp(host: &str, port: u16) -> Result<Stream, ConnectError> {
    let display = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let attempt = async {
        let addresses: Vec<SocketAddr> = match scoped_ipv6(host, port) {
            Some(address) => vec![address],
            None => tokio::net::lookup_host((host, port))
                .await
                .map_err(|error| ConnectError::Lookup {
                    host: host.to_owned(),
                    error,
                })?
                .collect(),
        };
        let mut first_error = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    // A scoped address is reported with its zone as written, as Go does.
                    let address = if host.contains('%') {
                        display.clone()
                    } else {
                        address.to_string()
                    };
                    first_error.get_or_insert(ConnectError::Dial {
                        network: "tcp",
                        address,
                        error,
                    });
                }
            }
        }
        Err(first_error.unwrap_or_else(|| ConnectError::Lookup {
            host: host.to_owned(),
            error: io::Error::other("no such host"),
        }))
    };
    let stream = match tokio::time::timeout(DIAL_TIMEOUT, attempt).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(ConnectError::DialTimeout {
                network: "tcp",
                address: display,
            });
        }
    };
    // Best effort, as in Go, which ignores a failure to set either.
    let _ = stream.set_nodelay(true);
    let keepalive = TcpKeepalive::new()
        .with_time(KEEPALIVE)
        .with_interval(KEEPALIVE)
        .with_retries(KEEPALIVE_PROBES);
    let _ = SockRef::from(&stream).set_tcp_keepalive(&keepalive);
    Ok(Stream::Tcp(stream))
}

#[cfg(test)]
mod tests;
