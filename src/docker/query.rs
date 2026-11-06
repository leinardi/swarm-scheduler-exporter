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

//! What a request to the engine is built from: the event filters, the `since` parameter and the
//! check of a pinned API version.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use jiff::Timestamp;

/// moby's `Filters`: terms with sets of values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    terms: BTreeMap<String, BTreeSet<String>>,
}

impl Filters {
    /// No filters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// moby's `Filters.Add`.
    #[must_use]
    pub fn add(mut self, term: &str, values: &[&str]) -> Self {
        let set = self.terms.entry(term.to_owned()).or_default();
        set.extend(values.iter().map(|value| (*value).to_owned()));
        self
    }

    /// Whether there are none, in which case no `filters` parameter is sent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The terms as bollard sends them: `{"term": ["value", …]}`, the values sorted.
    pub(crate) fn to_map(&self) -> HashMap<String, Vec<String>> {
        self.terms
            .iter()
            .map(|(term, values)| (term.clone(), values.iter().cloned().collect()))
            .collect()
    }
}

/// moby's `GetTimestamp` of an RFC 3339 time passed as `since`: `<seconds>.<nanoseconds>`, nine
/// digits, the seconds rounded down.
#[must_use]
pub fn since_parameter(since: Timestamp) -> String {
    let mut seconds = since.as_second();
    let mut nanos = since.subsec_nanosecond();
    if nanos < 0 {
        seconds -= 1;
        nanos += 1_000_000_000;
    }
    format!("{seconds}.{nanos:09}")
}

/// The oldest Engine API version the exporter accepts as a `DOCKER_API_VERSION` pin: what the
/// service, task and node objects it reads (`JobStatus`, `ReplicatedJob`) are served from.
pub const MIN_API_VERSION: (usize, usize) = (1, 40);

/// The newest Engine API version the exporter is tested against: a pin above it is refused at
/// startup instead of failing every request when the daemon does not know it.
pub const MAX_API_VERSION: (usize, usize) = (1, 56);

/// A `DOCKER_API_VERSION` pin outside [`MIN_API_VERSION`] to [`MAX_API_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedApiVersion {
    version: String,
}

impl fmt::Display for UnsupportedApiVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsupported Docker API version {} (DOCKER_API_VERSION): supported range is {}.{} to {}.{}",
            self.version,
            MIN_API_VERSION.0,
            MIN_API_VERSION.1,
            MAX_API_VERSION.0,
            MAX_API_VERSION.1
        )
    }
}

impl std::error::Error for UnsupportedApiVersion {}

/// The pinned API version must lie in [`MIN_API_VERSION`] to [`MAX_API_VERSION`]. The format
/// (`<major>.<minor>`) is `docker-client`'s check, made when the settings are read; a pin that
/// passes it and is outside the range, used as it is with no negotiation, would only surface as
/// failing requests.
///
/// # Errors
///
/// [`UnsupportedApiVersion`] for a version outside the range, or one that is not
/// `<major>.<minor>`.
pub fn validate_client_api_version(version: &str) -> Result<(), UnsupportedApiVersion> {
    let parsed = version.split_once('.').and_then(|(major, minor)| {
        Some((major.parse::<usize>().ok()?, minor.parse::<usize>().ok()?))
    });
    match parsed {
        Some(found) if (MIN_API_VERSION..=MAX_API_VERSION).contains(&found) => Ok(()),
        _ => Err(UnsupportedApiVersion {
            version: version.to_owned(),
        }),
    }
}
