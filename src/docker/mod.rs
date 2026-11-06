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
//! code reaches Docker through, the [`types`] they decode into and the timeouts the callers put
//! around them. The transport is rust-common's `docker-client` over bollard, built from the
//! environment by [`settings_from_env`].
//!
//! - `DOCKER_HOST`, `DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY` and `DOCKER_API_VERSION` are read by
//!   `docker_client::Config::from_env`: a unix socket, `tcp://` and `http://`, and `https://` or
//!   `tcp://` with `DOCKER_TLS_VERIFY` (certificates from `DOCKER_CERT_PATH`, or `$HOME/.docker`).
//!   `DOCKER_TLS` (TLS without verification) is refused rather than ignored, as are a
//!   `DOCKER_CERT_PATH` without `DOCKER_TLS_VERIFY` on a `tcp://` host (Q5) and a pinned
//!   version that is not `<major>.<minor>` or is below [`query::MIN_API_VERSION`]; without a pin
//!   the version is negotiated with the first call.
//! - [`Client`] is the crate's client behind [`DockerApi`] alone, with the `User-Agent`
//!   `swarm-scheduler-exporter/<version>` and a [`RESPONSE_HEADER_TIMEOUT`] of 30s.
//! - Every call is bounded by the caller with the bare `context deadline exceeded`
//!   ([`with_docker_timeout`], [`with_bare_timeout`], [`with_bare_deadline`]); bollard's own
//!   timeout stops at the response headers, so the bound is what cuts a body that stalls.
//!
//! Read-only by construction: [`DockerApi`] has list, inspect and events calls only, and the
//! `attach` feature of `docker-client` is off (`meta/tests/docker_boundary.rs` checks both).

mod api;
mod convert;
mod error;
pub mod query;
pub mod types;

pub use api::{
    CONTAINERS_INSPECT_TIMEOUT, CONTAINERS_LIST_TIMEOUT, Client, ClientEventStream,
    DOCKER_REQUEST_TIMEOUT, DockerApi, EVENTS_CONNECT_GRACE, EventStream, with_docker_timeout,
};
pub use docker_client::{Config, ConnectError};
pub use error::{
    Error, ErrorKind, StreamError, StreamErrorKind, with_bare_deadline, with_bare_timeout,
};
pub use query::{Filters, since_parameter};

use std::time::Duration;

/// The Go exporter's `dockerResponseHeaderTimeout`: from the request written to the response
/// headers, so a daemon that accepts a connection and never answers cannot hold the event stream
/// forever.
pub const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// The program name of the `User-Agent`, and the whole of it when the version stamp cannot be
/// sent in a header.
const PROGRAM: &str = "swarm-scheduler-exporter";

/// Why the client settings could not be built.
#[derive(Debug)]
pub enum SettingsError {
    /// `docker_client::Config::from_env` refused the environment.
    Config(ConnectError),
    /// A `DOCKER_API_VERSION` pin below the supported minimum.
    Version(query::UnsupportedApiVersion),
    /// `DOCKER_CERT_PATH` with a `tcp://` host and no `DOCKER_TLS_VERIFY` (Q5): the certificates
    /// would be ignored and the daemon spoken to in the clear.
    CertsWithoutVerify,
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "docker client: {error}"),
            Self::Version(error) => error.fmt(formatter),
            Self::CertsWithoutVerify => formatter.write_str(
                "docker client: DOCKER_CERT_PATH is set but DOCKER_TLS_VERIFY is not: set \
                 DOCKER_TLS_VERIFY=1 to verify the daemon (TLS without verification is not \
                 supported)",
            ),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<ConnectError> for SettingsError {
    fn from(error: ConnectError) -> Self {
        Self::Config(error)
    }
}

impl From<query::UnsupportedApiVersion> for SettingsError {
    fn from(error: query::UnsupportedApiVersion) -> Self {
        Self::Version(error)
    }
}

/// The client settings: `docker_client::Config::from_env` with `getenv` for `os.Getenv`, the
/// exporter's `User-Agent` and the [`RESPONSE_HEADER_TIMEOUT`], and a pinned API version checked
/// against [`query::MIN_API_VERSION`].
///
/// # Errors
///
/// [`SettingsError`]: the environment `docker-client` refuses (an unknown scheme, TLS without
/// certificates, a malformed `DOCKER_API_VERSION`), a pin below the minimum, or certificates
/// without `DOCKER_TLS_VERIFY` (Q5).
pub fn settings_from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Config, SettingsError> {
    let set = |name: &str| getenv(name).is_some_and(|value| !value.is_empty());
    if set("DOCKER_CERT_PATH")
        && !set("DOCKER_TLS_VERIFY")
        && getenv("DOCKER_HOST").is_some_and(|host| host.starts_with("tcp://"))
    {
        return Err(SettingsError::CertsWithoutVerify);
    }
    if let Some(pin) = getenv("DOCKER_API_VERSION").filter(|pin| !pin.is_empty()) {
        query::validate_client_api_version(&pin)?;
    }
    Ok(Config::from_env(getenv)?
        .user_agent(user_agent(crate::version::VERSION))
        .response_header_timeout(RESPONSE_HEADER_TIMEOUT))
}

/// `swarm-scheduler-exporter/<version>`, or the bare program name when the version stamp holds
/// something no header may carry.
fn user_agent(version: &str) -> String {
    let user_agent = format!("{PROGRAM}/{version}");
    if http_header_safe(&user_agent) {
        user_agent
    } else {
        PROGRAM.to_owned()
    }
}

/// Whether `value` may be a header value: visible ASCII, space and tab, as Go's
/// `httpguts.ValidHeaderFieldValue` allows (obs-text excluded here).
fn http_header_safe(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
}

#[cfg(test)]
mod tests;
