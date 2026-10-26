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

//! Go's `context.Context`, as much of it as the harness and the suite use: cancellation and a
//! deadline, inherited by children. Every Docker call runs under a context derived from the
//! suite's, so once the suite deadline passes (or the run is interrupted) blocked calls fail fast
//! and teardown still runs on a context of its own.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Why a context ended, in Go's wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The context, or one it derives from, was cancelled.
    Canceled,
    /// Its deadline passed.
    DeadlineExceeded,
}

impl fmt::Display for Ended {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Canceled => "context canceled",
            Self::DeadlineExceeded => "context deadline exceeded",
        })
    }
}

impl std::error::Error for Ended {}

/// A cancellation signal with an optional deadline. Clones share the signal.
#[derive(Debug, Clone)]
pub struct Context {
    token: CancellationToken,
    deadline: Option<Instant>,
}

impl Context {
    /// `context.Background()`: never cancelled, no deadline.
    #[must_use]
    pub fn background() -> Self {
        Self {
            token: CancellationToken::new(),
            deadline: None,
        }
    }

    /// `context.WithTimeout`: a child cancelled with this one, whose deadline is the earlier of
    /// this one's and `timeout` from now.
    #[must_use]
    pub fn with_timeout(&self, timeout: Duration) -> Self {
        let deadline = Instant::now().checked_add(timeout);
        Self {
            token: self.token.child_token(),
            deadline: match (self.deadline, deadline) {
                (Some(parent), Some(own)) => Some(parent.min(own)),
                (parent, own) => parent.or(own),
            },
        }
    }

    /// `context.WithCancel`: a child that [`Context::cancel`] ends without ending this one.
    #[must_use]
    pub fn with_cancel(&self) -> Self {
        Self {
            token: self.token.child_token(),
            deadline: self.deadline,
        }
    }

    /// `context.WithoutCancel`: a context that neither this one's cancellation nor its deadline
    /// reaches, for a teardown that must run after the work it cleans up has been cut short.
    #[must_use]
    pub fn without_cancel(&self) -> Self {
        Self::background()
    }

    /// Cancels this context and every context derived from it.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// The deadline, if any.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Why the context has ended, or `None` while it is live.
    #[must_use]
    pub fn err(&self) -> Option<Ended> {
        if self.token.is_cancelled() {
            return Some(Ended::Canceled);
        }
        self.deadline
            .filter(|&deadline| Instant::now() >= deadline)
            .map(|_| Ended::DeadlineExceeded)
    }

    /// Runs `future` until it completes or the context ends, whichever comes first.
    ///
    /// # Errors
    ///
    /// Why the context ended, if it ended first.
    pub async fn run<F: Future>(&self, future: F) -> Result<F::Output, Ended> {
        let until_cancelled = self.token.run_until_cancelled(future);
        let outcome = match self.deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, until_cancelled)
                .await
                .map_err(|_| Ended::DeadlineExceeded)?,
            None => until_cancelled.await,
        };
        outcome.ok_or(Ended::Canceled)
    }
}
