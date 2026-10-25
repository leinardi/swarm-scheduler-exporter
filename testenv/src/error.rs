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

//! The harness's one error type: a message that carries its context the way Go's wrapped errors
//! do (`bring up test swarm 1a2b3c4d: create node ...: ...`).

use std::fmt;

/// A harness failure, with the chain of what was being done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// This error with `context` in front, as Go's `fmt.Errorf("context: %w", err)`.
    #[must_use]
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self(format!("{context}: {}", self.0))
    }

    /// Several errors as one, one message per line, as Go's `errors.Join`; `None` when there are
    /// none.
    #[must_use]
    pub fn join(errors: Vec<Self>) -> Option<Self> {
        if errors.is_empty() {
            return None;
        }
        Some(Self(
            errors
                .into_iter()
                .map(|error| error.0)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
