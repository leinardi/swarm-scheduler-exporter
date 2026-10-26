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

//! Six of the Go health tests. The other three (`TestPollAndPublishReplicasState_*`: the poll
//! timestamp moves on a full publish only) drive the poll protocol through a running reconciler,
//! so they live with the reconciler's poll tests.

use jiff::SignedDuration;

use super::*;
use crate::clock::FakeClock;
use crate::collector::desired::ReplicaGauges;
use crate::collector::exporter_metrics::ExporterMetrics;
use crate::collector::nodes::NodesByState;
use crate::collector::reconciler::{Collectors, Reconciler};
use crate::collector::replicas_state::ReplicasState;
use crate::collector::service_update::ServiceUpdateMetrics;
use crate::collector::types::CustomLabels;
use crate::logger::Level;
use crate::metrics::testing::{gather_series, series_id};
use crate::test_support::{FakeDocker, capture_logs};

const NOW_SECONDS: i64 = 1_767_225_600;

fn now() -> Timestamp {
    Timestamp::from_second(NOW_SECONDS).unwrap()
}

fn ago(seconds: i64) -> Timestamp {
    now() - SignedDuration::from_secs(seconds)
}

/// A reconciler's handle, never run.
fn reconciler_handle() -> ReconcilerHandle {
    let logger = capture_logs(Level::INFO).0;
    let custom = CustomLabels::default();
    let collectors = Collectors {
        custom_labels: custom.clone(),
        replica_gauges: Arc::new(ReplicaGauges::new(&custom)),
        service_update: Arc::new(ServiceUpdateMetrics::new(&custom)),
        nodes_by_state: Arc::new(NodesByState::new(logger.clone())),
        replicas_state: Arc::new(ReplicasState::new(&custom, logger.clone())),
        metrics: Arc::new(ExporterMetrics::new(Arc::new(Timestamps::default()))),
    };
    let clock = Arc::new(FakeClock::new(now()));
    Reconciler::new(Arc::new(FakeDocker::default()), collectors, clock, logger).handle()
}

struct Fixture {
    health: Health,
    reconciler: ReconcilerHandle,
    timestamps: Arc<Timestamps>,
    clock: Arc<FakeClock>,
}

/// Go's `resetHealthState`: no poll yet, and a reconciler whose first resync completed, so the
/// checks below see only the poll freshness unless they change it.
fn fixture(poll_delay: Duration) -> Fixture {
    let reconciler = reconciler_handle();
    reconciler.set_resync_state(1, 1, None);
    let timestamps = Arc::new(Timestamps::default());
    let clock = Arc::new(FakeClock::new(now()));
    Fixture {
        health: Health::new(
            reconciler.clone(),
            Arc::clone(&timestamps),
            poll_delay,
            Arc::clone(&clock) as Arc<dyn Clock>,
        ),
        reconciler,
        timestamps,
        clock,
    }
}

/// Go: `TestHealthSnapshot_NeverPolled`
#[test]
fn never_polled_is_unhealthy() {
    let fixture = fixture(Duration::from_secs(10));
    assert_eq!(
        fixture.health.snapshot(now()),
        (false, "no successful poll yet")
    );
}

/// Go: `TestHealthSnapshot_RecentPoll_IsHealthy`
#[test]
fn a_recent_poll_is_healthy() {
    let fixture = fixture(Duration::from_secs(10));
    fixture.timestamps.mark_poll_ok(ago(1));
    assert_eq!(fixture.health.snapshot(now()), (true, ""));
}

/// Go: `TestHealthSnapshot_StalePoll_IsUnhealthy`: past 3 × the poll delay.
#[test]
fn a_stale_poll_is_unhealthy() {
    let fixture = fixture(Duration::from_secs(10));
    fixture.timestamps.mark_poll_ok(ago(31));
    assert_eq!(fixture.health.snapshot(now()), (false, "last poll too old"));
    // At the window exactly, not yet: the check is strictly greater.
    fixture.timestamps.mark_poll_ok(ago(30));
    assert_eq!(fixture.health.snapshot(now()), (true, ""));
}

/// Go: `TestHealthSnapshot_ThirtySecondFloor`: a 1s delay still gets 30s.
#[test]
fn the_window_has_a_thirty_second_floor() {
    let fixture = fixture(Duration::from_secs(1));
    fixture.timestamps.mark_poll_ok(ago(20));
    assert_eq!(fixture.health.snapshot(now()), (true, ""));
}

/// Go: `TestHealthSnapshot_ThirtySecondFloor_StaleAfter30s`
#[test]
fn the_floor_window_goes_stale_after_thirty_seconds() {
    let fixture = fixture(Duration::from_secs(1));
    fixture.timestamps.mark_poll_ok(ago(31));
    assert!(!fixture.health.snapshot(now()).0);
}

/// Go: `TestHealthGauge_EvaluatedAtScrape`: no poller runs here, so only the scrape-time
/// evaluation can notice the poll went stale.
#[test]
fn the_health_gauge_is_evaluated_at_scrape() {
    let fixture = fixture(Duration::from_secs(10));
    let gauges = HealthGauges::new(fixture.health.clone(), "v1.2.3", "abc", "2026-01-01");
    let health = || {
        let series = gather_series(Arc::clone(gauges.health()) as Arc<dyn Collector>);
        series["swarm_exporter_health{}"]
    };
    assert_eq!(Some(health()), Some(0.0), "before any poll");
    fixture.timestamps.mark_poll_ok(now());
    assert_eq!(Some(health()), Some(1.0), "right after a successful poll");
    fixture.clock.advance(SignedDuration::from_hours(1));
    assert_eq!(Some(health()), Some(0.0), "with a stale poll");
}

/// Go: `TestHealthSnapshot_ResyncOutstandingPastGrace`
#[test]
fn a_resync_outstanding_past_the_window_is_unhealthy() {
    let fixture = fixture(Duration::from_secs(10));
    fixture.timestamps.mark_poll_ok(now());
    fixture.reconciler.set_resync_state(2, 1, Some(ago(20)));
    assert_eq!(
        fixture.health.snapshot(now()),
        (true, ""),
        "within the window"
    );
    fixture.reconciler.set_resync_state(2, 1, Some(ago(31)));
    assert_eq!(
        fixture.health.snapshot(now()),
        (false, "resync outstanding")
    );
}

/// Go: `TestHealthSnapshot_InitialResyncNotCompleted`, on a reconciler as built.
#[test]
fn health_waits_for_the_initial_resync() {
    let fixture = fixture(Duration::from_secs(10));
    let fresh = Health::new(
        reconciler_handle(),
        Arc::clone(&fixture.timestamps),
        Duration::from_secs(10),
        Arc::new(FakeClock::new(now())),
    );
    fixture.timestamps.mark_poll_ok(now());
    assert_eq!(
        fresh.snapshot(now()),
        (false, "initial resync not completed")
    );
    assert_eq!(
        fresh.snapshot_now(),
        (false, "initial resync not completed")
    );
}

/// Before the reconciler exists the check reads Go's unset `activeReconciler`: the initial resync
/// is not completed, whatever the poll timestamp says; once bound, it reads that reconciler.
#[test]
fn unbound_health_waits_for_its_reconciler() {
    let fixture = fixture(Duration::from_secs(10));
    let health = Health::unbound(
        Arc::clone(&fixture.timestamps),
        Duration::from_secs(10),
        Arc::clone(&fixture.clock) as Arc<dyn Clock>,
    );
    fixture.timestamps.mark_poll_ok(now());
    assert_eq!(
        health.snapshot(now()),
        (false, "initial resync not completed")
    );
    let clone = health.clone();
    health.bind(fixture.reconciler.clone());
    assert_eq!(clone.snapshot(now()), (true, ""));
}

/// `swarm_exporter_build_info` carries the build at 1, with Go's names and HELP texts.
#[test]
fn build_info_is_one_with_the_build() {
    let fixture = fixture(Duration::from_secs(10));
    let gauges = HealthGauges::new(fixture.health, "v1.2.3", "abc", "2026-01-01");
    let series = gather_series(Arc::clone(gauges.build_info()) as Arc<dyn Collector>);
    let id = series_id(
        "swarm_exporter_build_info",
        &[
            ("version", "v1.2.3"),
            ("commit", "abc"),
            ("date", "2026-01-01"),
        ],
    );
    assert_eq!(series.get(&id), Some(&1.0));
    let registry = Registry::new();
    gauges.register(&registry).unwrap();
    let help: Vec<(String, String)> = registry
        .gather()
        .into_iter()
        .map(|family| (family.name, family.help))
        .collect();
    assert_eq!(
        help,
        [
            (
                "swarm_exporter_build_info".to_owned(),
                "Build information for this exporter.".to_owned()
            ),
            (
                "swarm_exporter_health".to_owned(),
                "Exporter health status: 1=healthy, 0=unhealthy.".to_owned()
            ),
        ]
    );
}

/// The timestamps are unix nanoseconds, 0 until marked.
#[test]
fn timestamps_are_unix_nanoseconds() {
    let timestamps = Timestamps::default();
    assert_eq!(
        (
            timestamps.last_poll_success(),
            timestamps.last_resync_success(),
            timestamps.last_events_connect()
        ),
        (0, 0, 0)
    );
    let at = Timestamp::new(1_700_000_000, 123_456_789).unwrap();
    timestamps.mark_poll_ok(at);
    timestamps.mark_resync_ok(at);
    timestamps.mark_events_connected(at);
    assert_eq!(timestamps.last_poll_success(), 1_700_000_000_123_456_789);
    assert_eq!(timestamps.last_resync_success(), 1_700_000_000_123_456_789);
    assert_eq!(timestamps.last_events_connect(), 1_700_000_000_123_456_789);
}
