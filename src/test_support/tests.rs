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

use std::time::Duration;

use super::*;
use crate::docker::types::{ContainerInspect, Event, Node, Service};
use crate::docker::{DockerApi, Error, EventStream, Filters, StreamError, StreamErrorKind};

fn service(id: &str) -> Service {
    Service {
        id: id.to_owned(),
        ..Service::default()
    }
}

#[tokio::test]
async fn fake_docker_answers_from_its_fields_and_counts_calls() {
    let mut fake = FakeDocker {
        nodes: vec![Node {
            id: "n1".to_owned(),
            ..Node::default()
        }],
        services: vec![service("s1")],
        ..FakeDocker::default()
    };
    fake.service_by_id.insert("s1".to_owned(), service("s1"));
    fake.inspects
        .insert("c1".to_owned(), ContainerInspect::default());

    assert_eq!(fake.node_list().await.unwrap()[0].id, "n1");
    assert_eq!(fake.node_list().await.unwrap().len(), 1);
    assert_eq!(fake.node_list_calls(), 2);
    assert_eq!(fake.service_list().await.unwrap()[0].id, "s1");
    assert_eq!(fake.service_inspect("s1").await.unwrap().id, "s1");
    // Unknown IDs get the zero value unless an error is set, as in the Go fake.
    assert_eq!(
        fake.service_inspect("s2").await.unwrap(),
        Service::default()
    );
    assert!(fake.task_list().await.unwrap().is_empty());
    assert!(fake.container_list().await.unwrap().is_empty());
    fake.container_inspect("c1").await.unwrap();
    fake.container_inspect("c2").await.unwrap();
    assert_eq!(fake.inspect_calls(), 2);
}

#[tokio::test]
async fn fake_docker_returns_the_errors_it_is_given() {
    let failure = || Error::deadline_exceeded();
    let fake = FakeDocker {
        nodes: vec![Node::default()],
        node_list_err: Some(failure()),
        service_list_err: Some(failure()),
        task_list_err: Some(failure()),
        container_list_err: Some(failure()),
        inspect_err: Some(failure()),
        service_inspect_err: Some(failure()),
        events_err: Some(failure()),
        ..FakeDocker::default()
    };
    assert!(fake.node_list().await.is_err());
    assert_eq!(fake.node_list_calls(), 1);
    assert!(fake.service_list().await.is_err());
    assert!(fake.task_list().await.is_err());
    assert!(fake.container_list().await.is_err());
    assert!(fake.container_inspect("c").await.is_err());
    assert!(fake.service_inspect("s").await.is_err());
    assert!(
        fake.events(jiff::Timestamp::UNIX_EPOCH, &Filters::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn fake_events_stream_from_the_channel_and_end_when_it_closes() {
    let mut fake = FakeDocker::default();
    let mut stream = fake
        .events(jiff::Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap_err().kind(),
        StreamErrorKind::Closed
    );

    let sender = fake.event_sender();
    let mut stream = fake
        .events(jiff::Timestamp::UNIX_EPOCH, &Filters::new())
        .await
        .unwrap();
    let event = Event {
        kind: "service".to_owned(),
        ..Event::default()
    };
    sender.send(Ok(event.clone())).unwrap();
    sender.send(Err(StreamError::eof())).unwrap();
    assert_eq!(stream.next().await.unwrap(), event);
    assert!(stream.next().await.unwrap_err().is_eof());
    drop(sender);
    let end = stream.next().await.unwrap_err();
    assert_eq!(end.kind(), StreamErrorKind::Closed);
    assert_eq!(end.to_string(), "events stream closed");
}

#[test]
fn capture_logs_records_what_is_logged_at_the_level() {
    let (logger, sink) = capture_logs(crate::logger::Level::INFO);
    logger.debug("hidden", &[]);
    logger.warn("shown", &[crate::logger::Attr::string("key", "value")]);
    assert_eq!(sink.contents(), "level=WARN shown key=value\n");
}

#[tokio::test(start_paused = true)]
async fn eventually_returns_once_the_condition_holds() {
    let start = tokio::time::Instant::now();
    eventually("a second", Duration::from_secs(5), || {
        start.elapsed() >= Duration::from_secs(1)
    })
    .await;
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "timed out after 1s waiting for nothing")]
async fn eventually_fails_once_the_wait_is_over() {
    eventually("nothing", Duration::from_secs(1), || false).await;
}
