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
//! made through `docker-client` and bollard, and the timeouts the callers put around them.
//!
//! The requests: `GET /services`, `GET /nodes` and `GET /tasks` without filters,
//! `GET /services/{id}`, `GET /containers/json?all=true`, `GET /containers/{id}/json`, and
//! `GET /events` with `filters` (`{"type":["node","service"]}`) and `since`
//! (`<seconds>.<9 digits>`). bollard's models are mapped to the exporter's [`types`](super::types)
//! by [`convert`](super::convert).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use docker_client::bollard::Docker;
use docker_client::bollard::errors::Error as BollardError;
use docker_client::bollard::models::EventMessage;
use docker_client::bollard::query_parameters::{
    EventsOptionsBuilder, ListContainersOptionsBuilder, ListNodesOptions, ListServicesOptions,
    ListTasksOptions,
};
use docker_client::{Config, ConnectError, connect, negotiate};
use futures_util::{Stream, StreamExt as _};
use jiff::Timestamp;
use tokio::sync::OnceCell;
use tracing::warn;

use super::convert;
use super::error::{Error, ErrorKind, StreamError, StreamErrorKind, with_bare_timeout};
use super::query::{Filters, since_parameter};
use super::types::{ContainerInspect, ContainerSummary, Event, Node, Service, Task};

/// The Go exporter's `dockerRequestTimeout`: the bound of one request and its response.
pub const DOCKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The Go exporter's `containersPollTimeout`: the bound of the container list.
pub const CONTAINERS_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// The Go exporter's `containersEnrichTimeout`: one deadline shared by all the container inspects
/// of a poll, so a slow daemon cannot hold a poll for the inspect cap times a per-call timeout.
pub const CONTAINERS_INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`DockerApi::events`] waits for the stream's first sign of life before it counts the
/// stream as connected. bollard opens the request when the stream is first polled and says
/// nothing when the response headers arrive, so a refused connection or a non-2xx answer shows up
/// as the first item; a quiet stream has none, and is connected once this has passed without one.
pub const EVENTS_CONNECT_GRACE: Duration = Duration::from_secs(2);

/// The Docker calls the collectors make. Every one but [`DockerApi::events`] is a single request
/// and response, which the caller bounds with [`with_docker_timeout`] (or the container
/// timeouts); the event stream is long-lived and has no deadline.
pub trait DockerApi: Send + Sync {
    /// The event stream [`DockerApi::events`] opens.
    type Events: EventStream;

    /// `GET /services`, without filters or status.
    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send;

    /// `GET /nodes`, without filters.
    fn node_list(&self) -> impl Future<Output = Result<Vec<Node>, Error>> + Send;

    /// `GET /services/{id}`, without the defaults inserted.
    fn service_inspect(&self, id: &str) -> impl Future<Output = Result<Service, Error>> + Send;

    /// `GET /tasks`, without filters.
    fn task_list(&self) -> impl Future<Output = Result<Vec<Task>, Error>> + Send;

    /// `GET /containers/json` with all the containers, stopped ones included.
    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send;

    /// `GET /containers/{id}/json`, without the size.
    fn container_inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send;

    /// `GET /events` from `since` with `filters`: resolves once the stream counts as connected
    /// (Q6, see [`EVENTS_CONNECT_GRACE`]), and fails without a stream otherwise.
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

type Wire = Pin<Box<dyn Stream<Item = Result<EventMessage, BollardError>> + Send>>;

/// The event stream of the Docker client. Dropping it closes the connection.
pub struct ClientEventStream {
    stream: Wire,
    /// What the connect wait already took off the stream.
    first: Option<Result<Event, StreamError>>,
}

impl std::fmt::Debug for ClientEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientEventStream")
            .finish_non_exhaustive()
    }
}

impl EventStream for ClientEventStream {
    async fn next(&mut self) -> Result<Event, StreamError> {
        if let Some(first) = self.first.take() {
            return first;
        }
        match self.stream.next().await {
            None => Err(StreamError::eof()),
            Some(Ok(message)) => Ok(convert::event(message)),
            Some(Err(error)) => Err(stream_error(&error)),
        }
    }
}

/// How a bollard error ended the stream: a body that is not an event is a decode failure, a
/// connection that broke or timed out is Go's unexpected EOF.
fn stream_error(error: &BollardError) -> StreamError {
    let kind = if matches!(
        error,
        BollardError::JsonDataError { .. } | BollardError::JsonSerdeError { .. }
    ) {
        StreamErrorKind::Decode
    } else {
        StreamErrorKind::UnexpectedEof
    };
    StreamError::new(kind, error.to_string())
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

/// The exporter's Docker client: `docker-client`'s connection behind [`DockerApi`] alone, so no
/// code of the exporter can send a request the trait does not name.
#[derive(Debug)]
pub struct Client {
    config: Config,
    /// The connection, made on the first call (the socket may not exist yet at startup) with the
    /// API version negotiated then, unless the configuration pins one.
    docker: OnceCell<Docker>,
    events_grace: Duration,
}

impl Client {
    /// A client for `config` ([`super::settings_from_env`]), with no request sent yet.
    ///
    /// # Errors
    ///
    /// What [`connect`] refuses for anything but a unix socket that is not there yet (which is
    /// retried by every call): an unreadable certificate, a `User-Agent` no header can carry.
    pub fn new(config: Config) -> Result<Self, ConnectError> {
        match connect(config.clone()) {
            Ok(_) | Err(ConnectError::Client(BollardError::SocketNotFoundError(_))) => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            config,
            docker: OnceCell::new(),
            events_grace: EVENTS_CONNECT_GRACE,
        })
    }

    /// The same client with another [`EVENTS_CONNECT_GRACE`], for tests.
    #[must_use]
    pub fn with_events_grace(mut self, grace: Duration) -> Self {
        self.events_grace = grace;
        self
    }

    /// The connection, made and negotiated by the first caller; a failed attempt is retried by
    /// the next.
    async fn docker(&self) -> Result<&Docker, Error> {
        self.docker
            .get_or_try_init(|| async {
                let docker = connect(self.config.clone()).map_err(|error| match error {
                    ConnectError::Client(error) => engine_error(error),
                    other @ (ConnectError::Invalid(_) | ConnectError::Unsupported(_)) => {
                        Error::new(ErrorKind::Other, other.to_string())
                    }
                })?;
                // A refusal (a socket proxy with VERSION=0 answers 403) keeps the client's own
                // version, which is what an unpinned client sends anyway; a failure to reach
                // the engine is the call's and is retried by the next.
                match negotiate(docker.clone(), &self.config).await {
                    Ok(negotiated) => Ok(negotiated),
                    Err(error) if error.kind() == docker_client::ErrorKind::Other => {
                        warn!(
                            error = %error,
                            "API version negotiation was refused; using the client's default"
                        );
                        Ok(docker)
                    }
                    Err(error) => Err(Error::from(error)),
                }
            })
            .await
    }
}

fn engine_error(error: BollardError) -> Error {
    Error::from(docker_client::Error::from(error))
}

/// moby's `trimID`: surrounding whitespace dropped, an empty ID refused.
fn trim_id<'id>(kind: &str, id: &'id str) -> Result<&'id str, Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::new(
            ErrorKind::Other,
            format!("invalid {kind} name or ID: value is empty"),
        ));
    }
    Ok(id)
}

// Each call's future is boxed: the connection and negotiation state would make every caller's
// future (a poll, a resync) larger than clippy's `large_futures` allows.
impl DockerApi for Client {
    type Events = ClientEventStream;

    async fn service_list(&self) -> Result<Vec<Service>, Error> {
        Box::pin(async {
            let list = self
                .docker()
                .await?
                .list_services(None::<ListServicesOptions>)
                .await
                .map_err(engine_error)?;
            list.into_iter().map(convert::service).collect()
        })
        .await
    }

    async fn node_list(&self) -> Result<Vec<Node>, Error> {
        Box::pin(async {
            let list = self
                .docker()
                .await?
                .list_nodes(None::<ListNodesOptions>)
                .await
                .map_err(engine_error)?;
            Ok(list.into_iter().map(convert::node).collect())
        })
        .await
    }

    async fn service_inspect(&self, id: &str) -> Result<Service, Error> {
        let id = trim_id("service", id)?;
        Box::pin(async {
            let service = self
                .docker()
                .await?
                .inspect_service(id, None)
                .await
                .map_err(engine_error)?;
            convert::service(service)
        })
        .await
    }

    async fn task_list(&self) -> Result<Vec<Task>, Error> {
        Box::pin(async {
            let list = self
                .docker()
                .await?
                .list_tasks(None::<ListTasksOptions>)
                .await
                .map_err(engine_error)?;
            list.into_iter().map(convert::task).collect()
        })
        .await
    }

    async fn container_list(&self) -> Result<Vec<ContainerSummary>, Error> {
        Box::pin(async {
            let options = ListContainersOptionsBuilder::new().all(true).build();
            let list = self
                .docker()
                .await?
                .list_containers(Some(options))
                .await
                .map_err(engine_error)?;
            Ok(list.into_iter().map(convert::container_summary).collect())
        })
        .await
    }

    async fn container_inspect(&self, id: &str) -> Result<ContainerInspect, Error> {
        let id = trim_id("container", id)?;
        Box::pin(async {
            let inspect = self
                .docker()
                .await?
                .inspect_container(id, None)
                .await
                .map_err(engine_error)?;
            Ok(convert::container_inspect(inspect))
        })
        .await
    }

    async fn events(
        &self,
        since: Timestamp,
        filters: &Filters,
    ) -> Result<ClientEventStream, Error> {
        let docker = self.docker().await?.clone();
        let since = since_parameter(since);
        let mut options = EventsOptionsBuilder::new().since(&since);
        if !filters.is_empty() {
            options = options.filters(&filters.to_map());
        }
        let mut stream: Wire = Box::pin(docker.events(Some(options.build())));
        let first = match tokio::time::timeout(self.events_grace, stream.next()).await {
            Err(_quiet) => None,
            Ok(None) => Some(Err(StreamError::eof())),
            Ok(Some(Err(error))) => return Err(engine_error(error)),
            Ok(Some(Ok(message))) => Some(Ok(convert::event(message))),
        };
        Ok(ClientEventStream { stream, first })
    }
}

#[cfg(test)]
mod tests;
