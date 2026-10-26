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

//! The failure of a Docker call a collector makes, with the context the Go exporter wraps it in
//! (`service list: …`, `task list: …`, `service inspect <id>: …`).

use std::fmt;

use crate::docker;

/// A Docker call that failed, worded `<context>: <error>` as Go's `fmt.Errorf("…: %w", err)`.
#[derive(Debug, Clone)]
pub struct CallError {
    context: String,
    source: docker::Error,
}

impl CallError {
    /// `source`, prefixed with `context`.
    #[must_use]
    pub fn new(context: impl Into<String>, source: docker::Error) -> Self {
        Self {
            context: context.into(),
            source,
        }
    }

    /// The Docker error, for `errors.Is`-style checks such as [`docker::Error::is_not_found`].
    #[must_use]
    pub fn docker_error(&self) -> &docker::Error {
        &self.source
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for CallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
