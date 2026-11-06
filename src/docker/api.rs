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

use std::future::Future;
use std::time::Duration;

use docker_engine::{
    Body, Error, Filters, JsonStream, Settings, StreamError, encode_query, since_parameter,
    trim_id, with_bare_timeout,
};
use jiff::Timestamp;

use super::types::{ContainerInspect, ContainerSummary, Event, Node, Service, Task};

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

/// The Go exporter's `withDockerTimeout`: [`with_bare_timeout`] at [`DOCKER_REQUEST_TIMEOUT`].
///
/// # Errors
///
/// As [`with_bare_timeout`].
pub async fn with_docker_timeout<T>(
    call: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    with_bare_timeout(DOCKER_REQUEST_TIMEOUT, call).await
}

/// The exporter's Docker client: rust-common's `docker-engine` client behind [`DockerApi`] alone,
/// so no code of the exporter can send a request the trait does not name.
#[derive(Debug)]
pub struct Client {
    inner: docker_engine::Client,
}

impl Client {
    /// A client for `settings` ([`super::settings_from_env`]), with no connection open yet.
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: docker_engine::Client::new(settings),
        }
    }
}

// Each call's future is the crate's, boxed: its request, pool and negotiation state would make
// every caller's future (a poll, a resync) larger than clippy's `large_futures` allows.
impl DockerApi for Client {
    type Events = ClientEventStream;

    async fn service_list(&self) -> Result<Vec<Service>, Error> {
        Box::pin(self.inner.get_list("/services", "")).await
    }

    async fn node_list(&self) -> Result<Vec<Node>, Error> {
        Box::pin(self.inner.get_list("/nodes", "")).await
    }

    async fn service_inspect(&self, id: &str) -> Result<Service, Error> {
        let id = trim_id("service", id)?;
        Box::pin(self.inner.get_object(&format!("/services/{id}"))).await
    }

    async fn task_list(&self) -> Result<Vec<Task>, Error> {
        Box::pin(self.inner.get_list("/tasks", "")).await
    }

    async fn container_list(&self) -> Result<Vec<ContainerSummary>, Error> {
        let query = encode_query(&mut [("all", "1".to_owned())]);
        Box::pin(self.inner.get_list("/containers/json", &query)).await
    }

    async fn container_inspect(&self, id: &str) -> Result<ContainerInspect, Error> {
        let id = trim_id("container", id)?;
        Box::pin(self.inner.get_object(&format!("/containers/{id}/json"))).await
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
        let query = encode_query(&mut pairs);
        Ok(ClientEventStream {
            stream: Box::pin(self.inner.events(&query)).await?,
        })
    }
}

#[cfg(test)]
mod tests;
