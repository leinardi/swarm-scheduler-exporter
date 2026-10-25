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

//! Go's `http.Server` deadlines, kept by a per-connection state that an I/O wrapper enforces and
//! the service updates at the request boundaries hyper does not expose.
//!
//! Go's `conn.serve` and `readRequest`:
//! - the first request's `t0` is when its read starts, right after the accept; the read deadline
//!   is `t0 + ReadHeaderTimeout` while the head is read, then `t0 + ReadTimeout` for the body;
//! - the write deadline is `now + WriteTimeout`, set once the head is read, and stays until the
//!   next request's head is read;
//! - after a response the read deadline is `now + IdleTimeout`, waiting for the next request's
//!   first bytes, whose arrival is that request's `t0`.
//!
//! hyper's own `header_read_timeout` cannot express this: hyper 1.12 arms it as soon as a
//! connection goes idle, so it also closes an idle keep-alive connection after the header
//! timeout, and it panics on the first poll when no hyper timer is set. It stays off.

use std::future::Future as _;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// The four `http.Server` timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// `ReadHeaderTimeout`.
    pub read_header: Duration,
    /// `ReadTimeout`.
    pub read: Duration,
    /// `WriteTimeout`.
    pub write: Duration,
    /// `IdleTimeout`.
    pub idle: Duration,
}

impl Timeouts {
    /// The exporter's: 5s, 10s, 15s and 60s.
    pub const EXPORTER: Self = Self {
        read_header: Duration::from_secs(5),
        read: Duration::from_secs(10),
        write: Duration::from_secs(15),
        idle: Duration::from_mins(1),
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Reading a request head.
    Header,
    /// Head read: the handler runs, or its response is written.
    Request,
    /// Response done, waiting for the next request's first bytes.
    Idle,
}

#[derive(Debug)]
struct Inner {
    phase: Phase,
    started: Instant,
    read_deadline: Instant,
    write_deadline: Option<Instant>,
    // A read that met its deadline while a handler ran. Go's background read then only cancels
    // the request's context: the response is still written and the connection stays usable. So
    // the read stalls until the response is done, and this waker resumes it. Stalling without
    // keeping the waker would hang the connection for good: hyper would never poll the read
    // side again, and the idle deadline would never be armed.
    stalled_read: Option<Waker>,
}

/// One connection's deadlines, shared by its I/O wrapper, its service and its response bodies.
#[derive(Debug, Clone)]
pub struct ConnState {
    timeouts: Timeouts,
    inner: Arc<Mutex<Inner>>,
}

impl ConnState {
    /// A connection accepted now: its first head is due within the header timeout.
    #[must_use]
    pub fn new(timeouts: Timeouts) -> Self {
        let now = Instant::now();
        Self {
            timeouts,
            inner: Arc::new(Mutex::new(Inner {
                phase: Phase::Header,
                started: now,
                read_deadline: now + timeouts.read_header,
                write_deadline: None,
                stalled_read: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// hyper parsed a request head and calls the service.
    pub fn on_request_head(&self) {
        let now = Instant::now();
        let mut inner = self.lock();
        inner.phase = Phase::Request;
        inner.read_deadline = inner.started + self.timeouts.read;
        inner.write_deadline = Some(now + self.timeouts.write);
    }

    /// hyper dropped the response body: written, or the connection ended.
    pub fn on_response_done(&self) {
        let now = Instant::now();
        let waker = {
            let mut inner = self.lock();
            inner.phase = Phase::Idle;
            inner.read_deadline = now + self.timeouts.idle;
            inner.stalled_read.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn on_bytes_read(&self) {
        let now = Instant::now();
        let mut inner = self.lock();
        if inner.phase == Phase::Idle {
            inner.phase = Phase::Header;
            inner.started = now;
            inner.read_deadline = now + self.timeouts.read_header;
        }
    }

    fn read_deadline(&self) -> Instant {
        self.lock().read_deadline
    }

    fn write_deadline(&self) -> Option<Instant> {
        self.lock().write_deadline
    }

    /// What a read that met its deadline does: stalls (none) while a request runs, else fails.
    fn on_read_deadline(&self, context: &Context<'_>) -> Option<io::Error> {
        let mut inner = self.lock();
        if inner.phase == Phase::Request {
            inner.stalled_read = Some(context.waker().clone());
            None
        } else {
            Some(timed_out("read"))
        }
    }
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, format!("{what} deadline exceeded"))
}

/// An I/O wrapper failing reads and writes once their deadline passed, as a Go `net.Conn` with
/// `SetReadDeadline` and `SetWriteDeadline` does.
#[derive(Debug)]
pub struct TimedIo<T> {
    io: T,
    state: ConnState,
    read_sleep: Pin<Box<Sleep>>,
    write_sleep: Pin<Box<Sleep>>,
}

impl<T> TimedIo<T> {
    /// `io` under the deadlines of `state`.
    #[must_use]
    pub fn new(io: T, state: ConnState) -> Self {
        let now = Instant::now();
        Self {
            io,
            read_sleep: Box::pin(tokio::time::sleep_until(state.read_deadline().max(now))),
            write_sleep: Box::pin(tokio::time::sleep_until(now)),
            state,
        }
    }
}

/// Polls `sleep`, moved to `deadline` first; whether the deadline passed.
fn expired(sleep: &mut Pin<Box<Sleep>>, deadline: Instant, context: &mut Context<'_>) -> bool {
    if sleep.deadline() != deadline {
        sleep.as_mut().reset(deadline);
    }
    sleep.as_mut().poll(context).is_ready()
}

impl<T: AsyncRead + Unpin> AsyncRead for TimedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let deadline = this.state.read_deadline();
        if Instant::now() >= deadline {
            return this
                .state
                .on_read_deadline(context)
                .map_or(Poll::Pending, |error| Poll::Ready(Err(error)));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.io).poll_read(context, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    this.state.on_bytes_read();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => {
                if expired(&mut this.read_sleep, deadline, context) {
                    this.state
                        .on_read_deadline(context)
                        .map_or(Poll::Pending, |error| Poll::Ready(Err(error)))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TimedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let deadline = this.state.write_deadline();
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Poll::Ready(Err(timed_out("write")));
        }
        match Pin::new(&mut this.io).poll_write(context, buf) {
            Poll::Pending => match deadline {
                Some(deadline) if expired(&mut this.write_sleep, deadline, context) => {
                    Poll::Ready(Err(timed_out("write")))
                }
                Some(_) | None => Poll::Pending,
            },
            ready @ Poll::Ready(_) => ready,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(context)
    }
}
