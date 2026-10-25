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

//! The read-only Docker Engine client, in-tree on hyper (S1): the transport and moby client
//! v0.6.0's lazy API version negotiation, the read-only [`DockerApi`] on top of it, the
//! [`types`] it decodes and the [`events`] stream.
//!
//! - [`Settings::from_env`] reads `DOCKER_HOST`, `DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY` and
//!   `DOCKER_API_VERSION` as the Go exporter's `newDockerClient` does (moby's `FromEnv` parts on a
//!   transport it owns), with one change: a certificate path without `DOCKER_TLS_VERIFY` is refused
//!   instead of connecting without server verification (Q5).
//! - [`Client`] sends HTTP/1.1 only, with no proxy: unix sockets with the `api.moby.localhost`
//!   host, TCP with or without TLS 1.2+, a 10s dial and a 10s TLS handshake timeout, a 30s
//!   response-header timeout, up to two idle connections kept for 30s, and on TCP Go's
//!   `Accept-Encoding: gzip` with the transparent decompression that comes with it.
//! - The first request pings `/_ping` (`HEAD`, then `GET`) and negotiates the API version down
//!   from 1.56; a failed negotiation is not an error, exactly as in moby, and concurrent first
//!   requests share one ping (Q11).
//!
//! Read-only by construction: the request builder only makes `GET` requests, `HEAD` is set in
//! one place, the ping, on `/_ping`, and [`DockerApi`] has list, inspect and events calls only.

mod api;
mod client;
mod connect;
mod error;
pub mod events;
mod host;
mod pool;
mod tls;
pub mod types;
pub mod version;

pub use api::{
    CONTAINERS_INSPECT_TIMEOUT, CONTAINERS_LIST_TIMEOUT, ClientEventStream, DOCKER_REQUEST_TIMEOUT,
    DockerApi, EventStream, Filters, since_parameter, with_deadline, with_docker_timeout,
    with_timeout,
};
pub use client::{Body, Client, Settings};
pub use error::{ConfigError, Error, ErrorKind};
pub use events::{StreamError, StreamErrorKind};
pub use tls::PathError;
pub use version::{MAX_API_VERSION, MIN_API_VERSION, UnsupportedApiVersion};

use std::time::Duration;

/// moby's `DefaultDockerHost` on Unix.
pub const DEFAULT_DOCKER_HOST: &str = "unix:///var/run/docker.sock";

/// moby's `DummyHost`: the `Host` header of requests over a unix socket.
pub const DUMMY_HOST: &str = "api.moby.localhost";

/// go-connections' dial timeout, unix and TCP alike.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The Go exporter's `dockerTLSHandshakeTimeout`.
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The Go exporter's `dockerResponseHeaderTimeout`: from the request written to the response
/// headers, so a daemon that accepts a connection and never answers cannot hold the event stream
/// forever.
pub const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests;
