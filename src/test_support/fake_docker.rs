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

//! A [`DockerApi`] answering from fields a test sets: the port of the Go tests' fake docker.

use std::collections::HashMap;
use std::future::{Future, pending, ready};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jiff::Timestamp;
use tokio::sync::{Mutex, mpsc};

use crate::docker::types::{ContainerInspect, ContainerSummary, Event, Node, Service, Task};
use crate::docker::{DockerApi, Error, EventStream, Filters, StreamError, StreamErrorKind};

/// What one fake event stream yields: an event, or the error that ends the stream.
pub(crate) type FakeEvent = Result<Event, StreamError>;

/// The fake's answers, set before it is shared; the call counters are atomic.
#[derive(Debug, Default)]
pub(crate) struct FakeDocker {
    pub(crate) nodes: Vec<Node>,
    pub(crate) services: Vec<Service>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) containers: Vec<ContainerSummary>,
    pub(crate) inspects: HashMap<String, ContainerInspect>,
    /// The `service_inspect` answer per service ID.
    pub(crate) service_by_id: HashMap<String, Service>,

    pub(crate) node_list_err: Option<Error>,
    pub(crate) service_list_err: Option<Error>,
    pub(crate) task_list_err: Option<Error>,
    pub(crate) container_list_err: Option<Error>,
    /// Returned by `container_inspect` for an ID not in `inspects`.
    pub(crate) inspect_err: Option<Error>,
    /// `container_inspect` never answers, as a hung daemon: only the caller's deadline ends it.
    pub(crate) inspect_blocks: bool,
    /// Returned by `service_inspect` for an ID not in `service_by_id`.
    pub(crate) service_inspect_err: Option<Error>,
    /// Returned by `events` instead of a stream: a connect that fails (Q6), which the Go fake
    /// could not express.
    pub(crate) events_err: Option<Error>,

    /// The events every stream reads from, shared as the Go fake shares its channels; without
    /// one, every stream ends at once, as the Go fake's closed channel does.
    pub(crate) events: Option<Arc<Mutex<mpsc::UnboundedReceiver<FakeEvent>>>>,

    pub(crate) node_list_calls: AtomicUsize,
    pub(crate) inspect_calls: AtomicUsize,
    pub(crate) service_inspect_calls: AtomicUsize,
}

impl FakeDocker {
    /// The sender of the events the streams yield; dropping it ends them as a closed channel.
    pub(crate) fn event_sender(&mut self) -> mpsc::UnboundedSender<FakeEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.events = Some(Arc::new(Mutex::new(receiver)));
        sender
    }

    /// How many times `node_list` was called.
    pub(crate) fn node_list_calls(&self) -> usize {
        self.node_list_calls.load(Ordering::SeqCst)
    }

    /// How many times `service_inspect` was called.
    pub(crate) fn service_inspect_calls(&self) -> usize {
        self.service_inspect_calls.load(Ordering::SeqCst)
    }

    /// How many times `container_inspect` was called.
    pub(crate) fn inspect_calls(&self) -> usize {
        self.inspect_calls.load(Ordering::SeqCst)
    }
}

fn answer<T: Clone>(error: Option<&Error>, items: &[T]) -> Result<Vec<T>, Error> {
    error.map_or_else(|| Ok(items.to_vec()), |error| Err(error.clone()))
}

impl DockerApi for FakeDocker {
    type Events = FakeEvents;

    fn service_list(&self) -> impl Future<Output = Result<Vec<Service>, Error>> + Send {
        ready(answer(self.service_list_err.as_ref(), &self.services))
    }

    fn node_list(&self) -> impl Future<Output = Result<Vec<Node>, Error>> + Send {
        self.node_list_calls.fetch_add(1, Ordering::SeqCst);
        ready(answer(self.node_list_err.as_ref(), &self.nodes))
    }

    fn service_inspect(&self, id: &str) -> impl Future<Output = Result<Service, Error>> + Send {
        self.service_inspect_calls.fetch_add(1, Ordering::SeqCst);
        ready(self.service_by_id.get(id).map_or_else(
            || {
                self.service_inspect_err
                    .clone()
                    .map_or_else(|| Ok(Service::default()), Err)
            },
            |service| Ok(service.clone()),
        ))
    }

    fn task_list(&self) -> impl Future<Output = Result<Vec<Task>, Error>> + Send {
        ready(answer(self.task_list_err.as_ref(), &self.tasks))
    }

    fn container_list(&self) -> impl Future<Output = Result<Vec<ContainerSummary>, Error>> + Send {
        ready(answer(self.container_list_err.as_ref(), &self.containers))
    }

    fn container_inspect(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<ContainerInspect, Error>> + Send {
        self.inspect_calls.fetch_add(1, Ordering::SeqCst);
        let reply = self.inspects.get(id).map_or_else(
            || {
                self.inspect_err
                    .clone()
                    .map_or_else(|| Ok(ContainerInspect::default()), Err)
            },
            |inspect| Ok(inspect.clone()),
        );
        let blocks = self.inspect_blocks;
        async move {
            if blocks {
                pending::<()>().await;
            }
            reply
        }
    }

    fn events(
        &self,
        _since: Timestamp,
        _filters: &Filters,
    ) -> impl Future<Output = Result<FakeEvents, Error>> + Send {
        ready(self.events_err.clone().map_or_else(
            || {
                Ok(FakeEvents {
                    events: self.events.clone(),
                })
            },
            Err,
        ))
    }
}

/// A stream of the fake's events.
#[derive(Debug)]
pub(crate) struct FakeEvents {
    pub(crate) events: Option<Arc<Mutex<mpsc::UnboundedReceiver<FakeEvent>>>>,
}

impl EventStream for FakeEvents {
    async fn next(&mut self) -> Result<Event, StreamError> {
        let closed = || StreamError::new(StreamErrorKind::Closed, "events stream closed");
        let Some(events) = &self.events else {
            return Err(closed());
        };
        events
            .lock()
            .await
            .recv()
            .await
            .unwrap_or_else(|| Err(closed()))
    }
}
