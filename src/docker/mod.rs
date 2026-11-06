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

//! The exporter's side of the Docker Engine API: [`DockerApi`], the read-only calls all collector
//! code reaches Docker through, the [`types`] they decode, and the timeouts the callers put around
//! them. The transport is rust-common's `docker-engine` (moby client v0.6.0's requests, its lazy
//! API version negotiation, its error wording and the event stream), built from the environment
//! by [`settings_from_env`] as the Go exporter's `newDockerClient` built its client.
//!
//! - `DOCKER_HOST`, `DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY` and `DOCKER_API_VERSION` are read by
//!   [`Settings::from_env`], with one change from Go: a certificate path without
//!   `DOCKER_TLS_VERIFY` is refused instead of connecting without server verification (Q5).
//! - [`Client`] is the crate's client behind [`DockerApi`] alone: HTTP/1.1 only, with no proxy,
//!   the `User-Agent` `swarm-scheduler-exporter/<version>` and a [`RESPONSE_HEADER_TIMEOUT`] of
//!   30s.
//! - Every call is bounded by the caller with the bare `context deadline exceeded`
//!   ([`with_docker_timeout`], [`with_bare_timeout`], [`with_bare_deadline`]).
//!
//! Read-only by construction: the exporter enables none of the crate's write features, so the
//! client sends `GET` only, plus one `HEAD /_ping`, and [`DockerApi`] has list, inspect and events
//! calls only.

mod api;
pub mod types;

pub use api::{
    CONTAINERS_INSPECT_TIMEOUT, CONTAINERS_LIST_TIMEOUT, Client, ClientEventStream,
    DOCKER_REQUEST_TIMEOUT, DockerApi, EventStream, with_docker_timeout,
};
pub use docker_engine::{
    ConfigError, Error, ErrorKind, Filters, Settings, StreamError, StreamErrorKind,
    since_parameter, version, with_bare_deadline, with_bare_timeout,
};

use std::time::Duration;

/// The Go exporter's `dockerResponseHeaderTimeout`: from the request written to the response
/// headers, so a daemon that accepts a connection and never answers cannot hold the event stream
/// forever.
pub const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// The program name of the `User-Agent`, and the whole of it when the version stamp cannot be
/// sent in a header.
const PROGRAM: &str = "swarm-scheduler-exporter";

/// The client settings the Go exporter's `newDockerClient` builds: [`Settings::from_env`] with
/// `getenv` for `os.Getenv`, the exporter's `User-Agent` and the [`RESPONSE_HEADER_TIMEOUT`]. A
/// pinned API version's range is checked by [`version::validate_client_api_version`], as the Go
/// exporter checks it after this.
///
/// # Errors
///
/// [`ConfigError`]: `configure TLS from DOCKER_CERT_PATH=<path>: …` for the TLS material, and
/// `docker client: …` for a host or API version moby refuses.
pub fn settings_from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Settings, ConfigError> {
    Ok(Settings::from_env(getenv)?
        .user_agent(user_agent(crate::version::VERSION))?
        .response_header_timeout(Some(RESPONSE_HEADER_TIMEOUT)))
}

/// `swarm-scheduler-exporter/<version>`, or the bare program name when the version stamp holds
/// something no header may carry.
fn user_agent(version: &str) -> String {
    let user_agent = format!("{PROGRAM}/{version}");
    if Settings::check_user_agent(&user_agent).is_ok() {
        user_agent
    } else {
        PROGRAM.to_owned()
    }
}

#[cfg(test)]
mod tests;
