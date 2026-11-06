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

//! The errors of a Docker call and of an event stream, and the bounds the callers put around
//! calls.
//!
//! [`Error`] is `docker-client`'s [`ErrorKind`] with the text kept and cloneable (the test
//! doubles answer the same error more than once). A call cut short by a deadline of the caller is
//! the bare `context deadline exceeded` the Go exporter logged, as [`ErrorKind::Timeout`].

use std::fmt;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

pub use docker_client::ErrorKind;

/// A failed Docker call.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// An error of `kind` worded as `message`.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Go's `context.DeadlineExceeded`, bare: the caller's deadline passed before the answer.
    #[must_use]
    pub fn deadline_exceeded() -> Self {
        Self::new(ErrorKind::Timeout, "context deadline exceeded")
    }

    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Whether the engine answered 404 (the service or container is gone): Go's
    /// `errdefs.IsNotFound`.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind == ErrorKind::NotFound
    }
}

impl From<docker_client::Error> for Error {
    fn from(error: docker_client::Error) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// How an event stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    /// Go's `io.EOF`: the body ended cleanly between two events.
    Eof,
    /// Go's `io.ErrUnexpectedEOF`: the connection broke, or the body ended inside an event.
    UnexpectedEof,
    /// A value that is not an event.
    Decode,
    /// The source of the events went away without saying why: Go's `ErrEventsStreamClosed`, the
    /// closed channel of a test double.
    Closed,
}

/// Why an event stream ended; every stream ends with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    kind: StreamErrorKind,
    message: String,
}

impl StreamError {
    /// A stream that ended with `kind`, worded as `message`.
    #[must_use]
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Go's `io.EOF`.
    #[must_use]
    pub fn eof() -> Self {
        Self::new(StreamErrorKind::Eof, "EOF")
    }

    /// Go's `io.ErrUnexpectedEOF`.
    #[must_use]
    pub fn unexpected_eof() -> Self {
        Self::new(StreamErrorKind::UnexpectedEof, "unexpected EOF")
    }

    /// How the stream ended.
    #[must_use]
    pub fn kind(&self) -> StreamErrorKind {
        self.kind
    }

    /// Whether it is Go's `errors.Is(err, io.EOF)`: a clean close, logged at info.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.kind == StreamErrorKind::Eof
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StreamError {}

/// Bounds `call` by `timeout`, from the request to the end of the response body (the call decodes
/// the body before it resolves, so a body that stalls is cut as well), with the bare error:
/// `context deadline exceeded`.
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the time ran out first.
pub async fn with_bare_timeout<T>(
    timeout: Duration,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(timeout, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}

/// Bounds `call` by a `deadline` other calls share, with the bare error as [`with_bare_timeout`].
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the deadline passed first.
pub async fn with_bare_deadline<T>(
    deadline: Instant,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout_at(deadline, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}
