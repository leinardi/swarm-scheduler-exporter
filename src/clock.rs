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

//! The one place the exporter reads the wall clock. Everything else takes a [`Clock`], so a test
//! can run on tokio's paused clock ([`TokioClock`]) or set the time by hand ([`FakeClock`]).

use std::sync::{Mutex, PoisonError};

use jiff::{SignedDuration, Timestamp};

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// A closure is a clock, so a test can hand one in directly.
impl<F> Clock for F
where
    F: Fn() -> Timestamp + Send + Sync,
{
    fn now(&self) -> Timestamp {
        self()
    }
}

/// The system's wall clock, Go's `time.Now`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one wall-clock read: every other time comes from an injected Clock"
    )]
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that follows tokio's clock: a base time plus the tokio time elapsed since it was
/// created. Under a paused runtime (`start_paused`) it moves only as tokio's clock does.
#[derive(Debug, Clone, Copy)]
pub struct TokioClock {
    base: Timestamp,
    start: tokio::time::Instant,
}

impl TokioClock {
    /// A clock reading `base` now.
    #[must_use]
    pub fn new(base: Timestamp) -> Self {
        Self {
            base,
            start: tokio::time::Instant::now(),
        }
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Timestamp {
        let elapsed = SignedDuration::try_from(self.start.elapsed()).unwrap_or(SignedDuration::MAX);
        // Adding a SignedDuration saturates; only a span with calendar units could fail.
        self.base.saturating_add(elapsed).unwrap_or(self.base)
    }
}

/// A clock set by hand.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<Timestamp>,
}

impl FakeClock {
    /// A clock reading `now` until it is changed.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    /// Sets the time.
    pub fn set(&self, now: Timestamp) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    /// Moves the time by `by`, saturating at the ends of the range.
    pub fn advance(&self, by: SignedDuration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        // Adding a SignedDuration saturates; only a span with calendar units could fail.
        *now = now.saturating_add(by).unwrap_or(*now);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn tokio_clock_follows_the_paused_clock() {
        let clock = TokioClock::new(at(1_000));
        assert_eq!(clock.now(), at(1_000));
        tokio::time::sleep(Duration::from_secs(90)).await;
        assert_eq!(clock.now(), at(1_090));
    }

    #[test]
    fn fake_clock_is_set_and_advanced() {
        let clock = FakeClock::new(at(10));
        clock.advance(SignedDuration::from_secs(5));
        assert_eq!(clock.now(), at(15));
        clock.set(at(3));
        assert_eq!(clock.now(), at(3));
        clock.advance(SignedDuration::MIN);
        assert_eq!(clock.now(), Timestamp::MIN);
    }

    #[test]
    fn system_clock_reads_the_present() {
        // After 2026-01-01, and not frozen at the epoch.
        assert!(SystemClock.now() > at(1_767_225_600));
    }

    #[test]
    fn a_closure_is_a_clock() {
        let clock = || at(42);
        assert_eq!(Clock::now(&clock), at(42));
    }
}
