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

//! Connections kept for reuse, and sending one request over them, as Go's `http.Transport`
//! does for the Docker client: at most two idle connections (`MaxIdleConnsPerHost`), each closed
//! after 30s idle (go-connections' `IdleConnTimeout`), the most recently used taken first, a
//! request that fails on a reused connection before any response retried on another, and the
//! response-header timeout counted from the request written.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Empty;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;

use super::RESPONSE_HEADER_TIMEOUT;
use super::connect::{ConnectError, Connector};

/// net/http's `DefaultMaxIdleConnsPerHost`, the bound that applies with one daemon.
const MAX_IDLE: usize = 2;

/// go-connections' `IdleConnTimeout`.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many times a request is sent before its failure is reported: Go retries on another
/// connection for as long as the failed one was reused, and only two can be idle.
const MAX_ATTEMPTS: usize = MAX_IDLE + 1;

/// Why a request got no response.
#[derive(Debug)]
pub(crate) enum SendError {
    /// No connection could be made.
    Connect(ConnectError),
    /// The headers did not arrive within the response-header timeout.
    HeaderTimeout,
    /// The exchange failed.
    Http(hyper::Error),
}

struct IdleConnection {
    id: u64,
    sender: SendRequest<Empty<Bytes>>,
}

/// The idle connections of one daemon.
pub(crate) struct Pool {
    connector: Connector,
    response_header_timeout: Duration,
    idle: Mutex<Vec<IdleConnection>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pool")
            .field("connector", &self.connector)
            .field("idle", &self.lock().len())
            .finish_non_exhaustive()
    }
}

impl Pool {
    pub(crate) fn new(connector: Connector) -> Arc<Self> {
        Self::with_timeout(connector, RESPONSE_HEADER_TIMEOUT)
    }

    pub(crate) fn with_timeout(
        connector: Connector,
        response_header_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            connector,
            response_header_timeout,
            idle: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<IdleConnection>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The most recently returned idle connection that is still open.
    async fn checkout(&self) -> Option<SendRequest<Empty<Bytes>>> {
        loop {
            let mut sender = self.lock().pop()?.sender;
            if !sender.is_closed() && sender.ready().await.is_ok() {
                return Some(sender);
            }
        }
    }

    /// A new connection, its HTTP/1.1 state driven by a task of its own; header names are
    /// written in Go's canonical case.
    async fn connect(&self) -> Result<SendRequest<Empty<Bytes>>, SendError> {
        let stream = self.connector.connect().await.map_err(SendError::Connect)?;
        let (sender, connection) = http1::Builder::new()
            .title_case_headers(true)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(SendError::Http)?;
        tokio::spawn(async move {
            // The connection ends with an error the request already reported, or cleanly.
            let _ = connection.await;
        });
        Ok(sender)
    }

    /// Keeps `sender` for reuse if there is room, and closes it after [`IDLE_TIMEOUT`] unused.
    fn give_back(self: &Arc<Self>, sender: SendRequest<Empty<Bytes>>) {
        if sender.is_closed() {
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut idle = self.lock();
            if idle.len() >= MAX_IDLE {
                return;
            }
            idle.push(IdleConnection { id, sender });
        }
        let pool = Arc::downgrade(self);
        // Outside a runtime there is no timer; the connection then closes when next checked out
        // or dropped with the pool.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(IDLE_TIMEOUT).await;
                if let Some(pool) = pool.upgrade() {
                    pool.lock().retain(|connection| connection.id != id);
                }
            });
        }
    }

    /// Sends the request `build` makes, on an idle connection or a new one. A failure on a
    /// reused connection before any response, which is how a connection the daemon closed while
    /// idle shows, sends it again on another, as Go does for a `GET` or `HEAD`.
    pub(crate) async fn send(
        self: &Arc<Self>,
        build: impl Fn() -> Request<Empty<Bytes>>,
    ) -> Result<Response<PooledBody>, SendError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let (mut sender, reused) = match self.checkout().await {
                Some(sender) => (sender, true),
                None => (self.connect().await?, false),
            };
            let request = build();
            match tokio::time::timeout(self.response_header_timeout, sender.send_request(request))
                .await
            {
                Err(_) => return Err(SendError::HeaderTimeout),
                Ok(Err(error)) => {
                    let retry = reused
                        && attempts < MAX_ATTEMPTS
                        && (error.is_canceled()
                            || error.is_closed()
                            || error.is_incomplete_message());
                    if !retry {
                        return Err(SendError::Http(error));
                    }
                }
                Ok(Ok(response)) => {
                    let (parts, incoming) = response.into_parts();
                    let body = PooledBody::new(incoming, sender, Arc::downgrade(self));
                    return Ok(Response::from_parts(parts, body));
                }
            }
        }
    }
}

/// A response body that returns its connection to the pool once read to the end, as Go's
/// transport does; a body dropped before its end closes the connection.
#[derive(Debug)]
pub(crate) struct PooledBody {
    inner: Incoming,
    connection: Option<(SendRequest<Empty<Bytes>>, Weak<Pool>)>,
}

impl PooledBody {
    fn new(inner: Incoming, sender: SendRequest<Empty<Bytes>>, pool: Weak<Pool>) -> Self {
        let mut body = Self {
            inner,
            connection: Some((sender, pool)),
        };
        if body.inner.is_end_stream() {
            body.release();
        }
        body
    }

    fn release(&mut self) {
        if let Some((sender, pool)) = self.connection.take()
            && let Some(pool) = pool.upgrade()
        {
            pool.give_back(sender);
        }
    }
}

impl Body for PooledBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let poll = Pin::new(&mut self.inner).poll_frame(context);
        match &poll {
            Poll::Ready(None) => self.release(),
            Poll::Ready(Some(Err(_))) => self.connection = None,
            Poll::Ready(Some(Ok(_))) => {
                if self.inner.is_end_stream() {
                    self.release();
                }
            }
            Poll::Pending => {}
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
