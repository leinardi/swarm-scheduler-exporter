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

//! The client's errors, worded as moby client v0.6.0 words them.

use std::error::Error as StdError;
use std::fmt;

use hyper::StatusCode;

/// Why the client could not be built from the environment. The text is the Go exporter's:
/// `configure TLS from DOCKER_CERT_PATH=<path>: …` for the TLS material, `docker client: …` for
/// the host and the API version.
#[derive(Debug)]
pub struct ConfigError {
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl ConfigError {
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            source: None,
        }
    }

    pub(crate) fn with_source(
        message: String,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            message,
            source: Some(Box::new(source)),
        }
    }

    /// Prefixes the message, as Go's `fmt.Errorf("…: %w", err)` does.
    pub(crate) fn context(self, prefix: &str) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
            source: self.source,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for ConfigError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// What kind of failure an [`Error`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// moby's `errConnectionFailed`: no response, for a reason in the message.
    ConnectionFailed,
    /// The daemon answered with a status of 400 or above; [`Error::status`] has it.
    Response,
    /// The response body failed after the headers.
    Body,
    /// The response body is not the JSON the call expects.
    Decode,
    /// The call was refused before any request, as moby refuses an empty ID.
    InvalidParameter,
    /// The caller's deadline passed first: Go's `context.DeadlineExceeded`.
    DeadlineExceeded,
}

/// A failed request.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    status: Option<StatusCode>,
}

impl Error {
    pub(crate) fn connection_failed(message: String) -> Self {
        Self {
            kind: ErrorKind::ConnectionFailed,
            message,
            status: None,
        }
    }

    pub(crate) fn response(status: StatusCode, message: String) -> Self {
        Self {
            kind: ErrorKind::Response,
            message,
            status: Some(status),
        }
    }

    pub(crate) fn body(message: String) -> Self {
        Self {
            kind: ErrorKind::Body,
            message,
            status: None,
        }
    }

    pub(crate) fn decode(message: String) -> Self {
        Self {
            kind: ErrorKind::Decode,
            message,
            status: None,
        }
    }

    pub(crate) fn invalid_parameter(message: String) -> Self {
        Self {
            kind: ErrorKind::InvalidParameter,
            message,
            status: None,
        }
    }

    /// Go's `context.DeadlineExceeded`, which moby passes through undecorated.
    #[must_use]
    pub fn deadline_exceeded() -> Self {
        Self {
            kind: ErrorKind::DeadlineExceeded,
            message: "context deadline exceeded".to_owned(),
            status: None,
        }
    }

    /// containerd's `errdefs.IsNotFound` of a moby error: the daemon answered 404.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind == ErrorKind::Response && self.status == Some(StatusCode::NOT_FOUND)
    }

    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The daemon's status, for a [`ErrorKind::Response`].
    #[must_use]
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Error {}
