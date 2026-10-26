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

//! The Swarm events listener: it follows the Docker event stream for service and node changes
//! and hands every event to the reconciler, which only marks what changed dirty, so the stream is
//! always drained.

use std::fmt;
use std::time::Duration;

use jiff::Timestamp;
use tokio_util::sync::CancellationToken;

use crate::docker::types::Event;
use crate::docker::{DockerApi, Error, EventStream, Filters, StreamError, StreamErrorKind};
use crate::go_compat::{duration, time_layout};
use crate::logger::Attr;

use super::{BACKOFF_INITIAL_DELAY, BACKOFF_MAX_DELAY, BACKOFF_MULTIPLIER, ReconcilerHandle};
use super::{EVENT_TYPE_NODE, EVENT_TYPE_SERVICE, duration_nanos};

/// How far before the anchor, or the last event seen, a stream starts: the daemon's `since` is
/// inclusive, and the margin covers events stamped just before the moment captured.
pub const EVENTS_SINCE_MARGIN: Duration = Duration::from_millis(500);

/// Why the listener returned: always a cancellation, as the listener only stops on one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerStopped {
    /// Cancelled while a stream was open or being opened, or between two of them.
    Stopping,
    /// Cancelled during the backoff of this many nanoseconds before a reconnect.
    DuringBackoff(i64),
}

impl fmt::Display for ListenerStopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopping => formatter.write_str("event listener stopping: context canceled"),
            Self::DuringBackoff(nanos) => write!(
                formatter,
                "event listener canceled during backoff: backoff of {} interrupted: context \
                 canceled",
                duration::format(*nanos)
            ),
        }
    }
}

impl std::error::Error for ListenerStopped {}

/// How one stream connection ended.
#[derive(Debug, Clone)]
pub enum StreamEnd {
    /// The listener's cancellation fired.
    Canceled,
    /// The connect failed: no 2xx response with headers (Q6).
    Connect(Error),
    /// The stream ended with an error, or cleanly ([`StreamErrorKind::Eof`]).
    Failed(StreamError),
    /// The source of the events went away without an error, Go's closed message channel.
    Closed(StreamError),
}

impl StreamEnd {
    /// Go's `errors.Is(err, io.EOF)`: a stream the server or a proxy closed cleanly.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        matches!(self, Self::Failed(error) if error.is_eof())
    }
}

impl fmt::Display for StreamEnd {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled => formatter.write_str("event pump context canceled: context canceled"),
            Self::Connect(error) => write!(formatter, "events stream error: {error}"),
            Self::Failed(error) => write!(formatter, "events stream error: {error}"),
            Self::Closed(error) => write!(formatter, "events stream closed: {error}"),
        }
    }
}

impl std::error::Error for StreamEnd {}

/// What one connection left behind: whether it connected, the time of the last event it saw,
/// and how it ended.
#[derive(Debug)]
pub struct StreamOutcome {
    pub connected: bool,
    pub last_seen: Option<Timestamp>,
    pub end: StreamEnd,
}

/// The event filter: service and node events only.
pub(super) fn event_filters() -> Filters {
    Filters::new().add("type", &[EVENT_TYPE_SERVICE, EVENT_TYPE_NODE])
}

/// `timestamp` minus [`EVENTS_SINCE_MARGIN`].
fn with_margin(timestamp: Timestamp) -> Timestamp {
    timestamp
        .checked_sub(EVENTS_SINCE_MARGIN)
        .unwrap_or(Timestamp::MIN)
}

/// The backoff that follows `delay`: doubled, capped at [`BACKOFF_MAX_DELAY`].
fn next_backoff(delay: Duration) -> Duration {
    delay
        .saturating_mul(BACKOFF_MULTIPLIER)
        .min(BACKOFF_MAX_DELAY)
}

/// Follows the Docker event stream until `cancel` fires, reconnecting with capped exponential
/// backoff. The first connection starts [`EVENTS_SINCE_MARGIN`] before `anchor`, which the
/// caller captures before the reconciler's first resync lists anything, so no change made while
/// that resync runs is missed; a reconnect resumes the margin before the last event seen, and
/// requests a resync once the new stream is open, since the daemon may no longer hold every event
/// the listener missed (a restart, a trimmed history). It only returns once `cancel` fired.
///
/// Two Go quirks are fixed. A stream counts as connected only after a 2xx response with headers:
/// only then is the connection logged, marked for health and, on a reconnect, followed by a
/// resync (Q6); a failed connect goes straight to the reconnect warning, and the next successful
/// connect still counts as a reconnect. And the backoff resets after every stream that connected,
/// not only after one that carried an event, while the resume point still moves only with an
/// event seen (Q7).
pub async fn listen_swarm_events(
    docker: &impl DockerApi,
    handle: &ReconcilerHandle,
    anchor: Timestamp,
    cancel: &CancellationToken,
) -> ListenerStopped {
    let filters = event_filters();
    let shared = &handle.shared;
    let mut backoff = BACKOFF_INITIAL_DELAY;
    let mut since = with_margin(anchor);
    let mut reconnect = false;
    loop {
        if cancel.is_cancelled() {
            return ListenerStopped::Stopping;
        }
        let outcome = follow_event_stream(docker, handle, &filters, since, reconnect, cancel).await;
        reconnect = true;
        if outcome.connected {
            backoff = BACKOFF_INITIAL_DELAY;
        }
        if let Some(last_seen) = outcome.last_seen {
            since = with_margin(last_seen);
        }
        // Shutting down is not a reconnect: neither counted nor logged as one.
        if cancel.is_cancelled() {
            return ListenerStopped::Stopping;
        }
        shared.metrics.inc_event_reconnect();
        // A stream the server or a proxy closed cleanly is routine (HAProxy, in front of the
        // daemon in a socket proxy, may close an idle stream after its client timeout); anything
        // else is worth a warning. The reconnect resyncs either way: a clean close looks the same
        // whether a proxy timed out or the daemon restarted, only the restart loses event
        // history, and the resync is just two list calls.
        let backoff_nanos = duration_nanos(backoff);
        let attrs = [
            Attr::error("err", &outcome.end),
            Attr::duration("backoff", backoff_nanos),
            Attr::string("next_since", local_rfc3339_nano(handle, since)),
        ];
        if outcome.end.is_eof() {
            shared
                .logger
                .info("event stream closed by the server; reconnecting", &attrs);
        } else {
            shared
                .logger
                .warn("event stream ended; will reconnect", &attrs);
        }
        if cancel
            .run_until_cancelled(tokio::time::sleep(backoff))
            .await
            .is_none()
        {
            return ListenerStopped::DuringBackoff(backoff_nanos);
        }
        backoff = next_backoff(backoff);
    }
}

/// `timestamp` in the logger's zone, Go's `time.Local`, as `time.RFC3339Nano`.
fn local_rfc3339_nano(handle: &ReconcilerHandle, timestamp: Timestamp) -> String {
    let offset = handle.shared.logger.zone().to_offset(timestamp);
    time_layout::format_rfc3339_nano(timestamp, offset)
}

/// Opens one event stream connection and dispatches its events until it ends. The connection
/// has no deadline: the stream is long-lived, so the request deadline of the other calls would
/// cut it. It is dropped, which releases the connection, when it ends before a reconnect and when
/// `cancel` fires. On a reconnect it requests a resync once the stream is open, so the resync
/// and the replay from `since` overlap and together cover whatever happened while the listener
/// was disconnected.
pub async fn follow_event_stream<D: DockerApi>(
    docker: &D,
    handle: &ReconcilerHandle,
    filters: &Filters,
    since: Timestamp,
    reconnect: bool,
    cancel: &CancellationToken,
) -> StreamOutcome {
    let failed = |end| StreamOutcome {
        connected: false,
        last_seen: None,
        end,
    };
    let mut stream = match cancel
        .run_until_cancelled(docker.events(since, filters))
        .await
    {
        None => return failed(StreamEnd::Canceled),
        Some(Err(error)) => return failed(StreamEnd::Connect(error)),
        Some(Ok(stream)) => stream,
    };
    let shared = &handle.shared;
    shared
        .metrics
        .timestamps()
        .mark_events_connected(shared.clock.now());
    shared.logger.info(
        "event stream connected",
        &[Attr::string("since", local_rfc3339_nano(handle, since))],
    );
    if reconnect {
        handle.request_resync("event stream reconnected");
    }
    let (last_seen, end) = dispatch_events(&mut stream, handle, cancel).await;
    StreamOutcome {
        connected: true,
        last_seen,
        end,
    }
}

/// Hands every event of `stream` to the reconciler until the stream ends or `cancel` fires, and
/// returns the time of the last event seen, which a reconnect resumes from. A stream that ends
/// once `cancel` fired reports the cancellation, so a shutdown is not taken for a stream failure.
pub async fn dispatch_events(
    stream: &mut impl EventStream,
    handle: &ReconcilerHandle,
    cancel: &CancellationToken,
) -> (Option<Timestamp>, StreamEnd) {
    let mut last_seen = None;
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => return (last_seen, StreamEnd::Canceled),
            next = stream.next() => next,
        };
        match next {
            Err(_) if cancel.is_cancelled() => return (last_seen, StreamEnd::Canceled),
            Err(error) if error.kind() == StreamErrorKind::Closed => {
                return (last_seen, StreamEnd::Closed(error));
            }
            Err(error) => return (last_seen, StreamEnd::Failed(error)),
            Ok(event) => {
                if let Some(seen) = event_time(&event) {
                    last_seen = Some(seen);
                }
                handle.enqueue_event(&event);
            }
        }
    }
}

/// The time Docker stamped `event` with: `timeNano` when set, else `time` (seconds); none when
/// neither is, or when the value is out of a timestamp's range.
fn event_time(event: &Event) -> Option<Timestamp> {
    if event.time_nano > 0 {
        return Timestamp::from_nanosecond(i128::from(event.time_nano)).ok();
    }
    if event.time > 0 {
        return Timestamp::from_second(event.time).ok();
    }
    None
}
