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

use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::Timestamp;

use super::*;
use crate::collector::metrics_ids::SERVICE_MODE_REPLICATED;
use crate::collector::types::display_name;
use crate::docker::types::UpdateStatus;
use crate::metrics::testing::gather_series;

/// Go's `makeTestMetadata`.
fn test_metadata(stack: &str, service: &str, mode: &str) -> ServiceMetadata {
    ServiceMetadata {
        stack: stack.to_owned(),
        service: service.to_owned(),
        service_mode: mode.to_owned(),
        ..ServiceMetadata::default()
    }
}

/// Go's `serviceLabels`.
fn service_labels(stack: &str, service: &str, mode: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("stack".to_owned(), stack.to_owned()),
        ("service".to_owned(), service.to_owned()),
        ("service_mode".to_owned(), mode.to_owned()),
        ("display_name".to_owned(), display_name(stack, service)),
    ])
}

fn metrics() -> ServiceUpdateMetrics {
    ServiceUpdateMetrics::new(&CustomLabels::default())
}

fn time(seconds: i64) -> GoTime {
    GoTime::new(Timestamp::from_second(seconds).unwrap())
}

/// Go: `TestUpdateStateFromSwarm`, row for row, plus the empty string and no update status.
#[test]
fn update_state_from_swarm_maps_every_state() {
    for (input, want) in [
        (Some("updating"), UPDATE_STATE_UPDATING),
        (Some("completed"), UPDATE_STATE_COMPLETED),
        (Some("paused"), UPDATE_STATE_PAUSED),
        (Some("rollback_paused"), UPDATE_STATE_PAUSED),
        (Some("rollback_started"), UPDATE_STATE_ROLLBACK_STARTED),
        (Some("rollback_completed"), UPDATE_STATE_ROLLBACK_COMPLETED),
        (Some("unknown_state"), UPDATE_STATE_PAUSED),
        (Some(""), UPDATE_STATE_PAUSED),
        (Some("Updating"), UPDATE_STATE_PAUSED),
        (None, UPDATE_STATE_COMPLETED),
    ] {
        assert_eq!(update_state_from_swarm(input), want, "{input:?}");
    }
}

/// Go: `TestCloneLabelsWithState`
#[test]
fn clone_labels_with_state_adds_the_state() {
    let base = BTreeMap::from([
        ("stack".to_owned(), "s".to_owned()),
        ("service".to_owned(), "sv".to_owned()),
    ]);
    let labels = clone_labels_with_state(&base, "updating");
    assert_eq!(labels["state"], "updating");
    assert_eq!(
        (labels["stack"].as_str(), labels["service"].as_str()),
        ("s", "sv")
    );
    assert!(!base.contains_key("state"), "the original is not changed");
}

/// Go: `TestCloneLabelsWithState_DoesNotMutateOriginal`
#[test]
fn clone_labels_with_state_leaves_the_original() {
    let base = BTreeMap::from([("k".to_owned(), "v".to_owned())]);
    let _ = clone_labels_with_state(&base, "state1");
    let _ = clone_labels_with_state(&base, "state2");
    assert_eq!(base.len(), 1);
}

/// Go: `TestUpdateServiceUpdateMetricsForService_UpdatingState`
#[test]
fn updating_state_is_the_one_set() {
    let metrics = metrics();
    let service = Service {
        update_status: Some(UpdateStatus {
            state: "updating".to_owned(),
            started_at: Some(time(1_700_000_000)),
            ..UpdateStatus::default()
        }),
        ..Service::default()
    };
    metrics
        .update_for_service(
            &service,
            &test_metadata("stack", "svc", SERVICE_MODE_REPLICATED),
        )
        .unwrap();
    let base = service_labels("stack", "svc", SERVICE_MODE_REPLICATED);
    for state in KNOWN_STATES {
        let want = if state == UPDATE_STATE_UPDATING {
            1.0
        } else {
            0.0
        };
        assert_eq!(
            metrics
                .state()
                .get_with(&clone_labels_with_state(&base, state)),
            Some(want),
            "{state}"
        );
    }
}

/// Go: `TestUpdateServiceUpdateMetricsForService_NilUpdateStatus_DefaultsCompleted`
#[test]
fn no_update_status_is_completed() {
    let metrics = metrics();
    metrics
        .update_for_service(
            &Service::default(),
            &test_metadata("stack", "svc", SERVICE_MODE_REPLICATED),
        )
        .unwrap();
    let base = service_labels("stack", "svc", SERVICE_MODE_REPLICATED);
    let completed = clone_labels_with_state(&base, UPDATE_STATE_COMPLETED);
    assert_eq!(metrics.state().get_with(&completed), Some(1.0));
    assert_eq!(metrics.started().get_with(&base), Some(0.0), "0 if unknown");
    assert_eq!(metrics.completed().get_with(&base), Some(0.0));
}

/// Go: `TestUpdateServiceUpdateMetricsForService_TimestampsPopulated`, plus a fraction, which
/// whole seconds drop.
#[test]
fn timestamps_are_whole_seconds() {
    let metrics = metrics();
    let service = Service {
        update_status: Some(UpdateStatus {
            state: "completed".to_owned(),
            started_at: Some(time(1_700_000_000)),
            completed_at: Some(GoTime::new(
                Timestamp::new(1_700_000_060, 999_999_999).unwrap(),
            )),
        }),
        ..Service::default()
    };
    metrics
        .update_for_service(
            &service,
            &test_metadata("stack", "svc", SERVICE_MODE_REPLICATED),
        )
        .unwrap();
    let base = service_labels("stack", "svc", SERVICE_MODE_REPLICATED);
    assert_eq!(metrics.started().get_with(&base), Some(1_700_000_000.0));
    assert_eq!(metrics.completed().get_with(&base), Some(1_700_000_060.0));
}

/// Go: `TestClearServiceUpdateMetrics_DeletesSeries`
#[test]
fn clear_deletes_every_series() {
    let metrics = metrics();
    let metadata = test_metadata("stack", "svc", SERVICE_MODE_REPLICATED);
    metrics
        .update_for_service(&Service::default(), &metadata)
        .unwrap();
    let state = Arc::clone(&metrics.state);
    let started = Arc::clone(&metrics.started);
    let completed = Arc::clone(&metrics.completed);
    assert_eq!(
        gather_series(Arc::clone(&state) as Arc<dyn Collector>).len(),
        5
    );
    metrics.clear(&metadata);
    for family in [state, started, completed] {
        assert!(gather_series(family as Arc<dyn Collector>).is_empty());
    }
}

#[test]
fn families_carry_go_names_help_and_label_order() {
    let custom = CustomLabels::new(&["team".to_owned()], &["team".to_owned()]);
    let metrics = ServiceUpdateMetrics::new(&custom);
    let desc = |family: &GaugeVec| Arc::clone(&family.descs()[0]);
    let state = desc(metrics.state());
    assert_eq!(state.name(), "swarm_service_update_state_info");
    assert_eq!(state.help(), UPDATE_STATE_HELP);
    assert_eq!(
        state.label_names(),
        [
            "stack",
            "service",
            "service_mode",
            "display_name",
            "team",
            "state"
        ],
        "state comes after the custom labels"
    );
    for (family, name, help) in [
        (
            desc(metrics.started()),
            "swarm_service_update_started_timestamp_seconds",
            UPDATE_STARTED_HELP,
        ),
        (
            desc(metrics.completed()),
            "swarm_service_update_completed_timestamp_seconds",
            UPDATE_COMPLETED_HELP,
        ),
    ] {
        assert_eq!(family.name(), name);
        assert_eq!(family.help(), help);
        assert_eq!(
            family.label_names(),
            ["stack", "service", "service_mode", "display_name", "team"]
        );
    }
    assert_eq!(
        UPDATE_STARTED_HELP,
        "Unix timestamp (seconds) when the current/last service update started (0 if unknown)."
    );
    let registry = Registry::new();
    metrics.register(&registry).unwrap();
    assert!(metrics.register(&registry).is_err());
}
