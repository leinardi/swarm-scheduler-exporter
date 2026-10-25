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

//! The Go listener tests: the 6 reconnect tests, the 3 listener tests that sat with the
//! reconciler's, the listener and dispatcher tests that sat with the desired replicas', and the
//! tests of the two Go quirks the listener fixes (Q6, Q7).

use crate::gofmt_compat::time_layout::format_rfc3339_nano;

use super::*;

/// Go's `errClosedBody`: the error a cut stream ends with.
fn closed_body() -> StreamError {
    StreamError::new(
        StreamErrorKind::UnexpectedEof,
        "read on closed response body",
    )
}

/// Go's `startListener`: the listener running on its own task until stopped.
struct Listening {
    cancel: CancellationToken,
    task: JoinHandle<ListenerStopped>,
}

impl Listening {
    fn start(fixture: &Fixture, anchor: Timestamp) -> Self {
        let cancel = CancellationToken::new();
        let docker = Arc::clone(&fixture.docker);
        let handle = fixture.handle.clone();
        let token = cancel.clone();
        let task =
            tokio::spawn(
                async move { listen_swarm_events(&*docker, &handle, anchor, &token).await },
            );
        Self { cancel, task }
    }

    async fn stop(self) -> ListenerStopped {
        self.cancel.cancel();
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("the listener did not return after cancellation")
            .unwrap()
    }
}

/// Go's `waitOpened`, counted: waits until `count` streams were opened.
async fn wait_opened(docker: &TestDocker, count: usize) {
    eventually("the event stream to open", WAIT, || {
        docker.streams_opened() >= count
    })
    .await;
}

fn resyncs_requested(handle: &ReconcilerHandle) -> u64 {
    handle.resync_state().0
}

async fn wait_resyncs_requested(handle: &ReconcilerHandle, count: u64, what: &str) {
    eventually(what, WAIT, || resyncs_requested(handle) == count).await;
}

fn anchor() -> Timestamp {
    Timestamp::from_second(1_767_225_600).unwrap()
}

fn node_update(time_nano: i64) -> Event {
    Event {
        time_nano,
        ..node_event("n1")
    }
}

impl Fixture {
    /// `timestamp` as the listener logs it, in the logger's zone.
    fn logged_time(&self, timestamp: Timestamp) -> String {
        let offset = self.handle.shared.logger.zone().to_offset(timestamp);
        format_rfc3339_nano(timestamp, offset)
    }

    /// The log lines that announce a reconnect, in order.
    fn reconnect_lines(&self) -> Vec<String> {
        self.sink
            .contents()
            .lines()
            .filter(|line| line.contains("reconnect"))
            .map(str::to_owned)
            .collect()
    }

    fn reconnects(&self) -> f64 {
        self.collectors.metrics.events_reconnects().get()
    }
}

/// A fixture past its first resync, on Docker with no service and no node.
async fn listener_fixture() -> Fixture {
    let mut fixture = fixture(TestDocker::default());
    fixture.complete_first_resync().await;
    fixture
}

// ---- The stream ----

/// Go: `TestListenSwarmEvents_StreamHasNoDeadlineAndIsCanceledOnShutdown`: the stream outlives
/// the request deadline many times over, and is dropped on shutdown.
#[tokio::test(start_paused = true)]
async fn the_stream_has_no_deadline_and_is_dropped_on_shutdown() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    tokio::time::sleep(DOCKER_REQUEST_TIMEOUT * 4).await;
    assert!(fixture.docker.stream_live(0), "the stream was cut");
    assert_eq!(fixture.docker.events_calls(), 1);
    listening.stop().await;
    assert!(
        !fixture.docker.stream_live(0),
        "the stream is still held after shutdown"
    );
}

/// Go: `TestListenSwarmEvents_StreamCanceledOnReconnect`
#[tokio::test(start_paused = true)]
async fn the_stream_is_dropped_on_reconnect() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    fixture.docker.send(0, Err(closed_body()));
    wait_opened(&fixture.docker, 2).await;
    assert!(
        !fixture.docker.stream_live(0),
        "the first stream is still held after the listener reconnected"
    );
    listening.stop().await;
}

/// Go: `TestListenSwarmEvents_SinceHasNanosecondPrecision`
#[tokio::test(start_paused = true)]
async fn since_has_nanosecond_precision() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(
        &fixture,
        Timestamp::new(1_700_000_000, 123_456_789).unwrap(),
    );
    wait_opened(&fixture.docker, 1).await;
    let since = fixture.docker.since(0);
    assert_eq!(
        since,
        Timestamp::new(1_699_999_999, 623_456_789).unwrap(),
        "the anchor minus the margin, to the nanosecond"
    );
    assert_eq!(
        crate::docker::since_parameter(since),
        "1699999999.623456789"
    );
    listening.stop().await;
}

/// Go: `TestListenSwarmEvents_CancelDuringPump_NoReconnectCounted`
#[tokio::test(start_paused = true)]
async fn a_cancellation_during_the_pump_is_not_a_reconnect() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    let stopped = listening.stop().await;
    assert_eq!(stopped, ListenerStopped::Stopping);
    assert_eq!(
        stopped.to_string(),
        "event listener stopping: context canceled"
    );
    assert_eq!(
        Some(fixture.reconnects()),
        Some(0.0),
        "shutdown is not a reconnect"
    );
    assert_eq!(
        fixture.docker.events_calls(),
        1,
        "reconnected after cancellation"
    );
}

/// A cancellation during the backoff ends the listener at once, saying so.
#[tokio::test(start_paused = true)]
async fn a_cancellation_during_the_backoff_ends_the_listener() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    fixture.docker.send(0, Err(closed_body()));
    eventually("the reconnect warning", WAIT, || {
        fixture.reconnect_lines().len() == 1
    })
    .await;
    let stopped = listening.stop().await;
    assert_eq!(
        stopped.to_string(),
        "event listener canceled during backoff: backoff of 500ms interrupted: context canceled"
    );
    assert_eq!(fixture.docker.events_calls(), 1);
}

/// Go: `TestDispatchEvents_CancelledStreamErrorReportsCancellation`: once cancelled, a stream
/// error is the cancellation. Both are ready at once and the select picks one at random, so it
/// is repeated.
#[tokio::test]
async fn a_cancelled_stream_error_reports_the_cancellation() {
    let fixture = fixture(TestDocker::default());
    for _ in 0..64 {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (sender, receiver) = mpsc::unbounded_channel();
        sender.send(Err(closed_body())).unwrap();
        let mut stream = TestStream {
            receiver,
            live: Arc::new(AtomicBool::new(true)),
        };
        let (_, end) = dispatch_events(&mut stream, &fixture.handle, &cancel).await;
        assert!(matches!(end, StreamEnd::Canceled), "{end}");
    }
}

/// Go: `TestDispatchEvents_CancelledStreamCloseReportsCancellation`
#[tokio::test]
async fn a_cancelled_stream_close_reports_the_cancellation() {
    let fixture = fixture(TestDocker::default());
    for _ in 0..64 {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (_, receiver) = mpsc::unbounded_channel();
        let mut stream = TestStream {
            receiver,
            live: Arc::new(AtomicBool::new(true)),
        };
        let (_, end) = dispatch_events(&mut stream, &fixture.handle, &cancel).await;
        assert!(matches!(end, StreamEnd::Canceled), "{end}");
        assert_eq!(
            end.to_string(),
            "event pump context canceled: context canceled"
        );
    }
}

/// The stream's end, as Go words it: a closed source, an error, the time of the last event
/// (`timeNano`, else `time`), and only node and service events acted on.
#[tokio::test]
async fn dispatch_reports_the_end_and_the_last_event_time() {
    let fixture = fixture(TestDocker::default());
    let cancel = CancellationToken::new();
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut stream = TestStream {
        receiver,
        live: Arc::new(AtomicBool::new(true)),
    };
    sender
        .send(Ok(node_update(1_700_000_000_987_654_321)))
        .unwrap();
    sender
        .send(Ok(Event {
            time: 1_700_000_001,
            ..service_event("svc1", "update")
        }))
        .unwrap();
    sender
        .send(Ok(Event {
            kind: "container".to_owned(),
            ..service_event("c1", "start")
        }))
        .unwrap();
    drop(sender);
    let (last_seen, end) = dispatch_events(&mut stream, &fixture.handle, &cancel).await;
    assert_eq!(
        last_seen,
        Some(Timestamp::from_second(1_700_000_001).unwrap())
    );
    assert_eq!(
        end.to_string(),
        "events stream closed: events stream closed"
    );
    assert!(!end.is_eof());
    assert_eq!(fixture.pending_lengths(), (1, 1), "the service event only");
    assert!(fixture.handle.shared.lock().nodes_dirty);

    let (sender, receiver) = mpsc::unbounded_channel();
    let mut stream = TestStream {
        receiver,
        live: Arc::new(AtomicBool::new(true)),
    };
    sender.send(Err(StreamError::eof())).unwrap();
    let (last_seen, end) = dispatch_events(&mut stream, &fixture.handle, &cancel).await;
    assert_eq!(last_seen, None);
    assert_eq!(end.to_string(), "events stream error: EOF");
    assert!(end.is_eof());
}

// ---- Reconnect ----

/// Go: `TestListenSwarmEvents_ReconnectResumesFromLastEventAndRequestsResync`
#[tokio::test(start_paused = true)]
async fn a_reconnect_resumes_from_the_last_event_and_requests_a_resync() {
    let fixture = listener_fixture().await;
    let before = resyncs_requested(&fixture.handle);
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;

    let last_seen = Timestamp::new(1_700_000_000, 987_654_321).unwrap();
    fixture
        .docker
        .send(0, Ok(node_update(1_700_000_000_987_654_321)));
    fixture.docker.send(0, Err(closed_body()));
    wait_opened(&fixture.docker, 2).await;
    let want_since = last_seen.checked_sub(EVENTS_SINCE_MARGIN).unwrap();
    assert_eq!(
        fixture.docker.since(1),
        want_since,
        "the last seen event minus the margin"
    );
    wait_resyncs_requested(
        &fixture.handle,
        before + 1,
        "a resync requested by the first reconnect",
    )
    .await;

    // A connection that saw no event keeps the resume point, and still requests a resync.
    fixture.docker.send(1, Err(closed_body()));
    wait_opened(&fixture.docker, 3).await;
    assert_eq!(fixture.docker.since(2), want_since);
    wait_resyncs_requested(
        &fixture.handle,
        before + 2,
        "a resync requested by the second reconnect",
    )
    .await;
    assert_eq!(Some(fixture.reconnects()), Some(2.0));
    listening.stop().await;
}

/// Go: `TestListenSwarmEvents_ReconnectLogLevel`: a clean close (EOF, as a proxy's idle timeout)
/// is logged at info, another error at warn, and both still request a resync.
#[tokio::test(start_paused = true)]
async fn the_reconnect_log_level_follows_how_the_stream_ended() {
    let fixture = listener_fixture().await;
    let before = resyncs_requested(&fixture.handle);
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    fixture.docker.send(0, Err(StreamError::eof()));
    wait_opened(&fixture.docker, 2).await;
    wait_resyncs_requested(
        &fixture.handle,
        before + 1,
        "a resync requested after the clean close",
    )
    .await;
    fixture.docker.send(1, Err(closed_body()));
    wait_opened(&fixture.docker, 3).await;
    wait_resyncs_requested(
        &fixture.handle,
        before + 2,
        "a resync requested after the failed stream",
    )
    .await;
    let since = fixture.logged_time(anchor().checked_sub(EVENTS_SINCE_MARGIN).unwrap());
    assert_eq!(
        fixture.reconnect_lines(),
        [
            format!(
                "level=INFO event stream closed by the server; reconnecting \
                 err=\"events stream error: EOF\" backoff=500ms next_since={since}"
            ),
            format!(
                "level=WARN event stream ended; will reconnect \
                 err=\"events stream error: read on closed response body\" backoff=500ms \
                 next_since={since}"
            ),
        ]
    );
    listening.stop().await;
}

/// Go: `TestListenSwarmEvents_FirstConnectionRequestsNoResync`: the first connection's resync is
/// the reconciler's own first one.
#[tokio::test(start_paused = true)]
async fn the_first_connection_requests_no_resync() {
    let fixture = listener_fixture().await;
    let before = resyncs_requested(&fixture.handle);
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    // Dispatched only after the post-connect step ran.
    fixture.docker.send(0, Ok(node_update(0)));
    let handle = fixture.handle.clone();
    eventually("the event dispatched", WAIT, || {
        handle.shared.lock().nodes_dirty
    })
    .await;
    assert_eq!(resyncs_requested(&fixture.handle), before);
    assert!(
        fixture.sink.contents().contains(&format!(
            "level=INFO event stream connected since={}",
            fixture.logged_time(anchor().checked_sub(EVENTS_SINCE_MARGIN).unwrap())
        )),
        "{}",
        fixture.sink.contents()
    );
    assert_eq!(
        fixture
            .collectors
            .metrics
            .timestamps()
            .last_events_connect(),
        i64::try_from(fixture.clock.now().as_nanosecond()).unwrap()
    );
    listening.stop().await;
}

/// Go: `TestReconciler_LostRemoveRecoveredOnReconnect`. Go checks the metadata cache; the
/// reconciler owns it while it runs here, so its desired series stand for it.
#[tokio::test(start_paused = true)]
async fn a_lost_remove_is_recovered_on_reconnect() {
    let fixture = fixture(TestDocker::new(
        vec![
            replicated_service("svc1", "stack", "web", 1),
            replicated_service("svc2", "stack", "db", 1),
        ],
        vec![],
    ));
    let docker = Arc::clone(&fixture.docker);
    let handle = fixture.handle.clone();
    let listening = Listening::start(&fixture, anchor());
    let running = Running::start(fixture.reconciler);
    wait_opened(&docker, 1).await;
    wait_ready(&handle, "the first resync").await;
    let db = service_labels("stack", "db", SERVICE_MODE_REPLICATED);
    let gauges = Arc::clone(&fixture.collectors.replica_gauges);
    assert_eq!(gauges.desired().get_with(&db), Some(1.0));

    // svc2 is removed while the stream is down, and its event never arrives.
    docker.set_services(vec![replicated_service("svc1", "stack", "web", 1)]);
    docker.send(0, Err(closed_body()));
    eventually("svc2 dropped by the reconnect resync", WAIT, || {
        gauges.desired().get_with(&db).is_none()
    })
    .await;
    listening.stop().await;
    let reconciler = running.stop().await;
    assert!(reconciler.metadata().get("svc2").is_none());
    let registry = crate::metrics::Registry::new();
    gauges.register(&registry).unwrap();
    let desired_series: usize = registry
        .gather()
        .iter()
        .filter(|family| family.name == "swarm_service_desired_replicas")
        .map(|family| family.series.len())
        .sum();
    assert_eq!(desired_series, 1);
}

/// Go: `TestReconciler_LostNodeEventRecoveredOnReconnect`
#[tokio::test(start_paused = true)]
async fn a_lost_node_event_is_recovered_on_reconnect() {
    let fixture = fixture(TestDocker::new(
        vec![global_service("glb1", "infra", "agent")],
        nodes(1),
    ));
    let docker = Arc::clone(&fixture.docker);
    let listening = Listening::start(&fixture, anchor());
    let running = Running::start(fixture.reconciler);
    wait_opened(&docker, 1).await;
    let agent = service_labels("infra", "agent", SERVICE_MODE_GLOBAL);
    let gauges = Arc::clone(&fixture.collectors.replica_gauges);
    eventually("the first resync", WAIT, || {
        gauges.desired().get_with(&agent) == Some(1.0)
    })
    .await;

    // A node joins while the stream is down, and its event never arrives.
    docker.set_nodes(nodes(2));
    docker.send(0, Err(closed_body()));
    eventually("the global service desired on both nodes", WAIT, || {
        gauges.desired().get_with(&agent) == Some(2.0)
    })
    .await;
    listening.stop().await;
    running.stop().await;
}

// ---- Changes during a resync ----

/// A change [`changes_during_a_resync_are_reconciled`] makes while a resync runs, and whether
/// the reconciler caught up with it.
struct ResyncChange {
    name: &'static str,
    change: fn(&TestDocker, &ReconcilerHandle),
    settled: fn(&ReplicaGauges) -> bool,
}

/// Go: `TestReconciler_ChangesDuringResync`: the change and its event land after the resync
/// listed the services and before it applies them, and the key it marks dirty must survive the
/// resync. Go checks the metadata cache; the desired series stand for it here.
#[tokio::test]
async fn changes_during_a_resync_are_reconciled() {
    let cases = [
        ResyncChange {
            name: "create",
            change: |docker, handle| {
                docker.set_services(vec![
                    replicated_service("svc1", "stack", "web", 2),
                    replicated_service("svc2", "stack", "late", 4),
                ]);
                handle.enqueue_event(&service_event("svc2", "create"));
            },
            settled: |gauges| {
                let late = service_labels("stack", "late", SERVICE_MODE_REPLICATED);
                gauges.desired().get_with(&late) == Some(4.0)
            },
        },
        ResyncChange {
            name: "update",
            change: |docker, handle| {
                docker.set_services(vec![replicated_service("svc1", "stack", "web", 5)]);
                handle.enqueue_event(&service_event("svc1", "update"));
            },
            settled: |gauges| {
                let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
                gauges.desired().get_with(&web) == Some(5.0)
            },
        },
        ResyncChange {
            name: "remove",
            change: |docker, handle| {
                docker.set_services(vec![]);
                handle.enqueue_event(&service_event("svc1", "remove"));
            },
            settled: |gauges| {
                let web = service_labels("stack", "web", SERVICE_MODE_REPLICATED);
                gauges.desired().get_with(&web).is_none()
            },
        },
    ];
    for case in cases {
        let mut fixture = fixture(TestDocker::new(
            vec![replicated_service("svc1", "stack", "web", 2)],
            vec![],
        ));
        fixture.complete_first_resync().await;
        let changed = Arc::new(AtomicBool::new(false));
        {
            let docker = Arc::clone(&fixture.docker);
            let handle = fixture.handle.clone();
            let changed = Arc::clone(&changed);
            let change = case.change;
            fixture.docker.lock().after_service_list = Some(Box::new(move || {
                change(&docker, &handle);
                changed.store(true, Ordering::SeqCst);
            }));
        }
        fixture.handle.request_resync("test");
        let running = Running::start(fixture.reconciler);
        eventually("the resync to list the services", WAIT, || {
            changed.load(Ordering::SeqCst)
        })
        .await;
        let gauges = Arc::clone(&fixture.collectors.replica_gauges);
        let settled = case.settled;
        eventually(case.name, WAIT, || settled(&gauges)).await;
        assert!(
            fixture.handle.resync_state().1 >= 2,
            "{}: the requested resync is not done",
            case.name
        );
        running.stop().await;
    }
}

// ---- Q6 and Q7 ----

/// Q6: a failed connect is not a connection. Nothing is logged as connected, the health
/// timestamp stays, and no resync is requested even on a reconnect; the connect error is the
/// end of the stream.
#[tokio::test]
async fn a_failed_connect_is_not_a_connection() {
    let fixture = listener_fixture().await;
    let before = resyncs_requested(&fixture.handle);
    fixture.docker.push_connect_error(unavailable());
    let outcome = follow_event_stream(
        &*fixture.docker,
        &fixture.handle,
        &super::super::listener::event_filters(),
        anchor(),
        true,
        &CancellationToken::new(),
    )
    .await;
    assert!(!outcome.connected);
    assert_eq!(outcome.last_seen, None);
    assert_eq!(
        outcome.end.to_string(),
        "events stream error: list unavailable"
    );
    assert!(!outcome.end.is_eof());
    assert_eq!(resyncs_requested(&fixture.handle), before);
    assert_eq!(
        fixture
            .collectors
            .metrics
            .timestamps()
            .last_events_connect(),
        0
    );
    assert!(!fixture.sink.contents().contains("event stream connected"));
}

/// Q6: failed connects go to the reconnect warning, with a growing backoff since nothing
/// connected (Q7's flip side), and the first successful connect after them is a reconnect,
/// which requests the resync.
#[tokio::test(start_paused = true)]
async fn the_connect_after_failed_ones_is_a_reconnect() {
    let fixture = listener_fixture().await;
    let before = resyncs_requested(&fixture.handle);
    fixture.docker.push_connect_error(unavailable());
    fixture.docker.push_connect_error(unavailable());
    let listening = Listening::start(&fixture, anchor());
    wait_opened(&fixture.docker, 1).await;
    wait_resyncs_requested(
        &fixture.handle,
        before + 1,
        "the resync of the successful connect",
    )
    .await;
    assert_eq!(fixture.docker.events_calls(), 3);
    let since = fixture.logged_time(anchor().checked_sub(EVENTS_SINCE_MARGIN).unwrap());
    assert_eq!(
        fixture.reconnect_lines(),
        [
            format!(
                "level=WARN event stream ended; will reconnect \
                 err=\"events stream error: list unavailable\" backoff=500ms next_since={since}"
            ),
            format!(
                "level=WARN event stream ended; will reconnect \
                 err=\"events stream error: list unavailable\" backoff=1s next_since={since}"
            ),
        ]
    );
    let connected = fixture
        .sink
        .contents()
        .matches("event stream connected")
        .count();
    assert_eq!(connected, 1);
    assert_eq!(Some(fixture.reconnects()), Some(2.0));
    listening.stop().await;
}

/// Q7: the backoff resets after every stream that connected, even one that carried no event,
/// while the resume point stays where the last event left it.
#[tokio::test(start_paused = true)]
async fn the_backoff_resets_after_a_quiet_stream() {
    let fixture = listener_fixture().await;
    let listening = Listening::start(&fixture, anchor());
    for index in 0..3 {
        wait_opened(&fixture.docker, index + 1).await;
        fixture.docker.send(index, Err(StreamError::eof()));
    }
    wait_opened(&fixture.docker, 4).await;
    let since = fixture.logged_time(anchor().checked_sub(EVENTS_SINCE_MARGIN).unwrap());
    let want = format!(
        "level=INFO event stream closed by the server; reconnecting \
         err=\"events stream error: EOF\" backoff=500ms next_since={since}"
    );
    assert_eq!(
        fixture.reconnect_lines(),
        [want.clone(), want.clone(), want]
    );
    for index in 0..4 {
        assert_eq!(
            fixture.docker.since(index),
            anchor().checked_sub(EVENTS_SINCE_MARGIN).unwrap()
        );
    }
    listening.stop().await;
}
