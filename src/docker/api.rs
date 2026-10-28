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

//! The read-only Docker API the exporter uses: the seven calls of [`DockerApi`] and nothing else,
//! with moby client v0.6.0's requests, and the timeouts the callers put around them.
//!
//! The requests are the ones the Go exporter sends with the options it uses: `GET /services`,
//! `GET /nodes` and `GET /tasks` without filters, `GET /services/{id}` without
//! `insertDefaults`, `GET /containers/json?all=1`, `GET /containers/{id}/json`, and
//! `GET /events` with `filters` (moby's JSON, keys sorted) and `since` (`<seconds>.<9 digits>`),
//! the query keys sorted and escaped as Go's `url.Values.Encode` does.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use http_body_util::BodyExt as _;
use hyper::header::CONTENT_TYPE;
use jiff::Timestamp;
use serde::Deserialize;

use super::client::{Body, Client};
use super::error::Error;
use super::events::{EVENTS_ACCEPT, JsonStream, StreamError};
use super::host::push_percent;
use super::types::{
    ContainerInspect, ContainerSummary, Event, Node, Service, Task, decode_first, decode_list,
};
use crate::go_compat::quote::push_hex_escape;

/// The Go exporter's `dockerRequestTimeout`: the bound of one request and its response.
pub const DOCKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The Go exporter's `containersPollTimeout`: the bound of the container list.
pub const CONTAINERS_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// The Go exporter's `containersEnrichTimeout`: one deadline shared by all the container inspects
/// of a poll, so a slow daemon cannot hold a poll for the inspect cap times a per-call timeout.
pub const CONTAINERS_INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The Docker calls the collectors make. Every one but [`DockerApi::events`] is a single request
/// and response, which the caller bounds with [`with_docker_timeout`] (or the container
/// timeouts); the event stream is long-lived and has no deadline.
pub trait DockerApi: Send + Sync {
    /// The event stream [`DockerApi::events`] opens.
    type Events: EventStream;

    /// moby's `ServiceList` without filters or status: `GET /services`.
    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send;

    /// moby's `NodeList` without filters: `GET /nodes`.
    fn node_list(&self) -> impl Future<Output = Result<Vec<Node>, Error>> + Send;

    /// moby's `ServiceInspect` with `InsertDefaults` false: `GET /services/{id}`.
    fn service_inspect(&self, id: &str) -> impl Future<Output = Result<Service, Error>> + Send;

    /// moby's `TaskList` without filters: `GET /tasks`.
    fn task_list(&self) -> impl Future<Output = Result<Vec<Task>, Error>> + Send;

    /// moby's `ContainerList` with `All`: `GET /containers/json?all=1`.
    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send;

    /// moby's `ContainerInspect` without size: `GET /containers/{id}/json`.
    fn container_inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send;

    /// moby's `Events` from `since` with `filters`: resolves once the response headers passed
    /// moby's status check, which is when the stream counts as connected (Q6), and fails without
    /// a stream otherwise.
    fn events(
        &self,
        since: Timestamp,
        filters: &Filters,
    ) -> impl Future<Output = Result<Self::Events, Error>> + Send;
}

/// An open event stream.
pub trait EventStream: Send {
    /// The next event, or how the stream ended; after an error the stream is over.
    fn next(&mut self) -> impl Future<Output = Result<Event, StreamError>> + Send;
}

/// The event stream of the Docker client.
#[derive(Debug)]
pub struct ClientEventStream {
    stream: JsonStream<Body>,
}

impl ClientEventStream {
    /// Whether the response was a JSON text sequence, read through the RS filter.
    #[must_use]
    pub fn filters_rs(&self) -> bool {
        self.stream.filters_rs()
    }
}

impl EventStream for ClientEventStream {
    async fn next(&mut self) -> Result<Event, StreamError> {
        self.stream.next().await
    }
}

/// moby's `Filters`: terms with sets of values, sent as `{"term":{"value":true}}`.
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

    /// The `filters` parameter: Go's `json.Marshal` of the map, keys sorted.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("{");
        for (term_index, (term, values)) in self.terms.iter().enumerate() {
            if term_index > 0 {
                out.push(',');
            }
            push_json_string(&mut out, term);
            out.push_str(":{");
            for (value_index, value) in values.iter().enumerate() {
                if value_index > 0 {
                    out.push(',');
                }
                push_json_string(&mut out, value);
                out.push_str(":true");
            }
            out.push('}');
        }
        out.push('}');
        out
    }
}

/// encoding/json's string encoding with `json.Marshal`'s HTML escaping.
fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                out.push('\\');
                out.push(character);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' | '\0'..='\u{1f}' => {
                push_hex_escape(out, 'u', u32::from(character), 4);
            }
            _ => out.push(character),
        }
    }
    out.push('"');
}

/// Go's `url.Values.Encode`: the pairs sorted by key, each side `url.QueryEscape`d.
fn encode_query(pairs: &mut [(&str, String)]) -> String {
    pairs.sort_by(|left, right| left.0.cmp(right.0));
    let mut out = String::new();
    for (key, value) in pairs.iter() {
        if !out.is_empty() {
            out.push('&');
        }
        push_query_escaped(&mut out, key);
        out.push('=');
        push_query_escaped(&mut out, value);
    }
    out
}

/// Go's `url.QueryEscape`: the unreserved bytes kept, a space as `+`, the rest `%XX`.
fn push_query_escaped(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            push_percent(out, byte);
        }
    }
}

/// moby's `GetTimestamp` of the RFC 3339 time the Go exporter passes as `since`:
/// `<seconds>.<nanoseconds>`, nine digits, the seconds rounded down.
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

/// moby's `trimID`: surrounding whitespace dropped, an empty ID refused.
fn trim_id<'id>(kind: &str, id: &'id str) -> Result<&'id str, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::invalid_parameter(format!(
            "invalid {kind} name or ID: value is empty"
        )));
    }
    Ok(id)
}

/// Bounds `call` by `timeout`, as `context.WithTimeout` bounds a Go call.
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the time ran out first.
pub async fn with_timeout<T>(
    timeout: Duration,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(timeout, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}

/// Bounds `call` by a `deadline` other calls share.
///
/// # Errors
///
/// The call's [`Error`], or [`Error::deadline_exceeded`] when the deadline passed first.
pub async fn with_deadline<T>(
    deadline: tokio::time::Instant,
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout_at(deadline, call)
        .await
        .unwrap_or_else(|_| Err(Error::deadline_exceeded()))
}

/// The Go exporter's `withDockerTimeout`: [`with_timeout`] at [`DOCKER_REQUEST_TIMEOUT`].
///
/// # Errors
///
/// As [`with_timeout`].
pub async fn with_docker_timeout<T>(
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    with_timeout(DOCKER_REQUEST_TIMEOUT, call).await
}

impl Client {
    /// The whole body of a `GET`, read to its end.
    async fn get_body(&self, path: &str, query: &str) -> Result<Vec<u8>, Error> {
        let response = self.get(path, query, &[]).await?;
        Ok(response.into_body().collect().await?.to_bytes().to_vec())
    }

    async fn get_list<T>(&self, path: &str, query: &str) -> Result<Vec<T>, Error>
    where
        T: Default + for<'de> Deserialize<'de>,
    {
        decode_list(&self.get_body(path, query).await?)
    }

    async fn get_object<T>(&self, path: &str) -> Result<T, Error>
    where
        T: Default + for<'de> Deserialize<'de>,
    {
        decode_first(&self.get_body(path, "").await?)
    }
}

impl DockerApi for Client {
    type Events = ClientEventStream;

    async fn service_list(&self) -> Result<Vec<Service>, Error> {
        self.get_list("/services", "").await
    }

    async fn node_list(&self) -> Result<Vec<Node>, Error> {
        self.get_list("/nodes", "").await
    }

    async fn service_inspect(&self, id: &str) -> Result<Service, Error> {
        let id = trim_id("service", id)?;
        self.get_object(&format!("/services/{id}")).await
    }

    async fn task_list(&self) -> Result<Vec<Task>, Error> {
        self.get_list("/tasks", "").await
    }

    async fn container_list(&self) -> Result<Vec<ContainerSummary>, Error> {
        self.get_list(
            "/containers/json",
            &encode_query(&mut [("all", "1".to_owned())]),
        )
        .await
    }

    async fn container_inspect(&self, id: &str) -> Result<ContainerInspect, Error> {
        let id = trim_id("container", id)?;
        self.get_object(&format!("/containers/{id}/json")).await
    }

    async fn events(
        &self,
        since: Timestamp,
        filters: &Filters,
    ) -> Result<ClientEventStream, Error> {
        let mut pairs = vec![("since", since_parameter(since))];
        if !filters.is_empty() {
            pairs.push(("filters", filters.to_json()));
        }
        let response = self
            .get("/events", &encode_query(&mut pairs), &EVENTS_ACCEPT)
            .await?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(ClientEventStream {
            stream: JsonStream::new(response.into_body(), content_type.as_deref()),
        })
    }
}

#[cfg(test)]
mod tests;
