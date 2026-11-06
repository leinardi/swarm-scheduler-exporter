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

//! What the exporter exposes, checked by meaning rather than by bytes: the inventory of families
//! against the list the Go exporter exposed, the exact value of every series for a controlled
//! state, the histogram's buckets, the disappearance of what the next poll no longer has, and
//! the whole-snapshot atomicity of a scrape racing the polls.

#[path = "../src/test_support/openmetrics.rs"]
mod openmetrics;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clock::{Clock, FakeClock};
use jiff::Timestamp;
use prometheus_client::registry::Registry;
use swarm_scheduler_exporter::collector::containers::ContainersConfig;
use swarm_scheduler_exporter::collector::desired::labels_for_metadata;
use swarm_scheduler_exporter::collector::exporter_metrics::ResyncResult;
use swarm_scheduler_exporter::collector::replicas_state::{ServiceCounter, TaskCounter};
use swarm_scheduler_exporter::collector::types::{CustomLabels, ServiceMetadata};
use swarm_scheduler_exporter::docker::types::{Node, NodeSpec, NodeStatus, Service};
use swarm_scheduler_exporter::metrics::{CONTENT_TYPE, DEF_BUCKETS, render};
use swarm_scheduler_exporter::run::{
    Metrics, MetricsOptions, register_metrics, register_runtime_metrics,
};

use openmetrics::{Exposition, parse, series_id};

/// Every exporter-owned series prefix: what is not the process collector's.
const OWN_PREFIX: &str = "swarm_";

/// The registry of `run`, and what the workers write.
struct Setup {
    registry: Registry,
    metrics: Metrics,
}

impl Setup {
    fn new(custom: &[&str]) -> Self {
        let mut registry = Registry::default();
        let raw: Vec<String> = custom.iter().map(|&name| name.to_owned()).collect();
        let metrics = register_metrics(
            &mut registry,
            &MetricsOptions {
                custom_labels: CustomLabels::new(&raw, &raw),
                poll_delay: Duration::from_secs(10),
                containers: ContainersConfig::new(true, true),
                build: [
                    "v1.2.3".to_owned(),
                    "abc".to_owned(),
                    "2026-01-01".to_owned(),
                ],
            },
            Arc::new(FakeClock::new(Timestamp::UNIX_EPOCH)) as Arc<dyn Clock>,
        );
        register_runtime_metrics(&mut registry);
        Self { registry, metrics }
    }

    fn scrape(&self) -> Exposition {
        parse(&render(&self.registry))
    }

    /// The exporter's own series of a scrape: no `process_*`.
    fn own_series(&self) -> BTreeMap<String, f64> {
        own(&self.scrape())
    }
}

fn own(scrape: &Exposition) -> BTreeMap<String, f64> {
    scrape
        .series()
        .into_iter()
        .filter(|(series, _)| series.starts_with(OWN_PREFIX))
        .collect()
}

fn metadata(stack: &str, service: &str, mode: &str, team: Option<&str>) -> ServiceMetadata {
    ServiceMetadata {
        stack: stack.to_owned(),
        service: service.to_owned(),
        service_mode: mode.to_owned(),
        custom_labels: team
            .map(|team| BTreeMap::from([("team".to_owned(), team.to_owned())]))
            .unwrap_or_default(),
        configured_replicas: 3.0,
        desired_replicas: 3.0,
        ..ServiceMetadata::default()
    }
}

/// The tasks of `metadata`'s service as one poll counted them.
fn counter(metadata: &ServiceMetadata, running: usize, failed: usize, desired: f64) -> TaskCounter {
    let mut counter = TaskCounter::new(labels_for_metadata(metadata));
    for _ in 0..running {
        counter.inc("running");
    }
    for _ in 0..failed {
        counter.inc("failed");
    }
    counter.desired = desired;
    counter.desired_known = true;
    counter
}

fn node(role: &str, availability: &str, status: &str) -> Node {
    Node {
        spec: NodeSpec {
            role: role.to_owned(),
            availability: availability.to_owned(),
            ..NodeSpec::default()
        },
        status: NodeStatus {
            state: status.to_owned(),
            ..NodeStatus::default()
        },
        ..Node::default()
    }
}

fn container(name: &str, state: &str, exit_code: &str) -> BTreeMap<String, String> {
    [
        ("project", ""),
        ("stack", ""),
        ("service", ""),
        ("container", name),
        ("orchestrator", "none"),
        ("display_name", name),
        ("state", state),
        ("exit_code", exit_code),
    ]
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .into()
}

/// `result`'s value, or a panic naming the error.
fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|error| panic!("{error}"))
}

/// Publishes one poll's worth of every family: the services (`running` tasks each), the nodes and
/// the containers; the per-service gauges are set, as the reconciler sets them.
fn publish_poll(setup: &Setup, services: &[(&ServiceMetadata, usize)], nodes: &[Node]) {
    let collectors = &setup.metrics.collectors;
    let mut counters = ServiceCounter::new();
    for (index, (metadata, running)) in services.iter().enumerate() {
        counters.insert(
            format!("svc{index}"),
            counter(metadata, *running, 0, metadata.desired_replicas),
        );
        ok(collectors.replica_gauges.set_desired(metadata, 3.0));
        ok(collectors.replica_gauges.set_schedulable(metadata, 2.0));
        ok(collectors
            .service_update
            .update_for_service(&Service::default(), metadata));
    }
    ok(collectors.replicas_state.update(&counters));
    collectors.nodes_by_state.update_from_slice(nodes);
}

/// The committed inventory: `(family, type, label names)` per line, comments skipped.
fn committed_inventory() -> BTreeSet<(String, String, String)> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/metrics/inventory.txt");
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the inventory: {error}"))
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.splitn(3, ' ').collect();
            assert!(fields.len() >= 2, "an inventory line: {line:?}");
            (
                fields[0].to_owned(),
                fields[1].to_owned(),
                fields.get(2).copied().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

/// Every family of `scrape` as `(sample name of the family, type, sorted label names)`: a
/// counter under its `_total`, a histogram without `le`.
fn inventory(scrape: &Exposition) -> BTreeSet<(String, String, String)> {
    scrape
        .types
        .iter()
        .map(|(family, kind)| {
            let sample_name = if kind == "counter" {
                format!("{family}_total")
            } else {
                family.clone()
            };
            let mut labels = BTreeSet::new();
            for sample in &scrape.samples {
                let belongs = sample.name == sample_name
                    || (kind == "histogram"
                        && ["_bucket", "_sum", "_count"]
                            .iter()
                            .any(|suffix| sample.name == format!("{family}{suffix}")));
                if belongs {
                    labels.extend(
                        sample
                            .labels
                            .iter()
                            .map(|(name, _)| name.clone())
                            .filter(|name| name != "le"),
                    );
                }
            }
            (
                sample_name,
                kind.clone(),
                labels.into_iter().collect::<Vec<_>>().join(","),
            )
        })
        .collect()
}

/// Inventory: with every family populated, the set of names, types and label names is the Go
/// exporter's, minus the Go runtime and the scrape handler's own families (documented drops).
#[test]
fn the_inventory_is_the_go_exporters_minus_the_documented_drops() {
    let setup = Setup::new(&[]);
    let service = metadata("web", "api", "replicated", None);
    publish_poll(
        &setup,
        &[(&service, 2)],
        &[node("worker", "active", "ready")],
    );
    setup
        .metrics
        .containers
        .as_ref()
        .unwrap()
        .update(&[container("c1", "running", "")]);
    setup
        .metrics
        .collectors
        .metrics
        .observe_poll_duration(Duration::from_millis(30));
    let scrape = setup.scrape();
    assert!(scrape.eof, "OpenMetrics text ends with # EOF");
    let got = inventory(&scrape);
    let mut want = committed_inventory();
    if !cfg!(target_os = "linux") {
        want.retain(|(name, ..)| !name.starts_with("process_"));
    }
    assert_eq!(got, want);
    for (name, ..) in &got {
        assert!(
            !name.starts_with("go_") && !name.starts_with("promhttp_"),
            "{name} is a documented drop"
        );
    }
}

/// The scrape is OpenMetrics text: the content type says so and no family is gzip-negotiated.
#[test]
fn the_content_type_is_openmetrics() {
    assert_eq!(
        CONTENT_TYPE,
        "application/openmetrics-text; version=1.0.0; charset=utf-8"
    );
}

/// The exporter's own counters and gauges of [`expected_series`].
fn exporter_series(want: &mut BTreeMap<String, f64>) {
    let mut put = |name: &str, labels: &[(&str, &str)], value: f64| {
        want.insert(series_id(name, labels), value);
    };
    put("swarm_exporter_polls_total", &[], 2.0);
    put("swarm_exporter_poll_errors_total", &[], 1.0);
    put("swarm_exporter_events_reconnects_total", &[], 1.0);
    put("swarm_exporter_events_dropped_total", &[], 2.0);
    put("swarm_exporter_poll_rejections_total", &[], 1.0);
    put(
        "swarm_exporter_resyncs_total",
        &[("result", "success")],
        1.0,
    );
    put(
        "swarm_exporter_resyncs_total",
        &[("result", "failure")],
        0.0,
    );
    // Unbound: unhealthy until the reconciler is bound; no poll or resync yet.
    put("swarm_exporter_health", &[], 0.0);
    put(
        "swarm_exporter_last_poll_success_timestamp_seconds",
        &[],
        0.0,
    );
    put(
        "swarm_exporter_last_resync_success_timestamp_seconds",
        &[],
        0.0,
    );
    put(
        "swarm_exporter_build_info",
        &[
            ("commit", "abc"),
            ("date", "2026-01-01"),
            ("version", "v1.2.3"),
        ],
        1.0,
    );
}

/// What [`every_series_has_the_value_the_state_gives`] must see: the series of one replicated
/// service with 3 desired and 2 running tasks, four nodes and some counters.
fn expected_series() -> BTreeMap<String, f64> {
    let base = [
        ("display_name", "web api"),
        ("service", "api"),
        ("service_mode", "replicated"),
        ("stack", "web"),
    ];
    let with = |extra: &[(&'static str, &'static str)]| -> Vec<(&str, &str)> {
        let mut labels = base.to_vec();
        labels.extend_from_slice(extra);
        labels
    };
    let mut want: BTreeMap<String, f64> = BTreeMap::new();
    let mut put = |name: &str, labels: &[(&str, &str)], value: f64| {
        want.insert(series_id(name, labels), value);
    };
    put("swarm_service_desired_replicas", &base, 3.0);
    put("swarm_service_schedulable_replicas", &base, 2.0);
    put("swarm_service_running_replicas", &base, 2.0);
    // Desired is the poll's 3, running 2: not at desired.
    put("swarm_service_at_desired", &base, 0.0);
    put("swarm_service_update_started_timestamp_seconds", &base, 0.0);
    put(
        "swarm_service_update_completed_timestamp_seconds",
        &base,
        0.0,
    );
    for state in [
        "updating",
        "completed",
        "paused",
        "rollback_started",
        "rollback_completed",
    ] {
        put(
            "swarm_service_update_state_info",
            &with(&[("state", state)]),
            if state == "completed" { 1.0 } else { 0.0 },
        );
    }
    for state in [
        "new",
        "allocated",
        "pending",
        "assigned",
        "accepted",
        "preparing",
        "ready",
        "starting",
        "running",
        "complete",
        "shutdown",
        "failed",
        "rejected",
        "remove",
        "orphaned",
    ] {
        put(
            "swarm_task_replicas_state",
            &with(&[("state", state)]),
            if state == "running" { 2.0 } else { 0.0 },
        );
    }
    for (role, availability, count) in [
        ("manager", "active", 1.0),
        ("worker", "active", 2.0),
        ("worker", "drain", 1.0),
    ] {
        put(
            "swarm_cluster_nodes_by_state",
            &[
                ("availability", availability),
                ("role", role),
                ("status", "ready"),
            ],
            count,
        );
    }
    exporter_series(&mut want);
    want
}

/// Values: the exact value of every series for a controlled state.
#[test]
fn every_series_has_the_value_the_state_gives() {
    let setup = Setup::new(&[]);
    let api = metadata("web", "api", "replicated", None);
    publish_poll(
        &setup,
        &[(&api, 2)],
        &[
            node("manager", "active", "ready"),
            node("worker", "active", "ready"),
            node("worker", "active", "ready"),
            node("worker", "drain", "ready"),
        ],
    );
    let exporter = &setup.metrics.collectors.metrics;
    exporter.inc_polls();
    exporter.inc_polls();
    exporter.inc_poll_errors();
    exporter.inc_event_reconnect();
    exporter.inc_events_dropped();
    exporter.inc_events_dropped();
    exporter.inc_poll_rejections();
    exporter.inc_resync(ResyncResult::Success);

    let want = expected_series();
    let mut got = setup.own_series();
    // The histogram has no observation: its zero buckets, sum and count are checked below.
    got.retain(|series, _| !series.starts_with("swarm_exporter_poll_duration_seconds"));
    assert_eq!(got, want);
    let zero = setup.scrape();
    assert_eq!(
        zero.value("swarm_exporter_poll_duration_seconds_count", &[]),
        Some(0.0)
    );
}

/// Buckets: the histogram's bounds are the old `DEF_BUCKETS`, observations land in the right
/// buckets (a bound is inclusive), and the sum and the count are right.
#[test]
fn the_poll_duration_histogram_keeps_the_old_buckets() {
    assert_eq!(
        DEF_BUCKETS.map(f64::to_bits),
        [
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0
        ]
        .map(f64::to_bits)
    );
    let setup = Setup::new(&[]);
    let exporter = &setup.metrics.collectors.metrics;
    for seconds in [0.004, 0.005, 0.3, 11.0] {
        exporter.observe_poll_duration(Duration::from_secs_f64(seconds));
    }
    let scrape = setup.scrape();
    let name = "swarm_exporter_poll_duration_seconds";
    let buckets: Vec<(f64, f64)> = scrape
        .samples
        .iter()
        .filter(|sample| sample.name == format!("{name}_bucket"))
        .map(|sample| {
            let (_, bound) = sample
                .labels
                .iter()
                .find(|(label, _)| label == "le")
                .unwrap();
            let bound = if bound == "+Inf" {
                f64::INFINITY
            } else {
                bound.parse().unwrap()
            };
            (bound, sample.value)
        })
        .collect();
    let mut want_bounds = DEF_BUCKETS.to_vec();
    want_bounds.push(f64::INFINITY);
    assert_eq!(
        buckets.iter().map(|&(bound, _)| bound).collect::<Vec<_>>(),
        want_bounds,
        "the bounds are DEF_BUCKETS and +Inf"
    );
    // Cumulative: 0.004 and 0.005 in le=0.005, 0.3 from le=0.5, 11 only in +Inf.
    let want_counts = [2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0, 3.0, 4.0];
    assert_eq!(
        buckets.iter().map(|&(_, count)| count).collect::<Vec<_>>(),
        want_counts
    );
    assert_eq!(scrape.value(&format!("{name}_count"), &[]), Some(4.0));
    let sum = scrape.value(&format!("{name}_sum"), &[]).unwrap();
    assert!((sum - 11.309).abs() < 1e-9, "{sum}");
}

/// Removal: a service, a node status, a container and a custom label value that the next poll
/// no longer has are gone from the next scrape; the pre-seeded zero series stay.
#[test]
fn what_the_next_poll_drops_leaves_the_next_scrape() {
    let setup = Setup::new(&["team"]);
    let collectors = &setup.metrics.collectors;
    let containers = setup.metrics.containers.as_ref().unwrap();
    let api_a = metadata("web", "api", "replicated", Some("a"));
    let db = metadata("web", "db", "replicated", Some("x"));
    publish_poll(
        &setup,
        &[(&api_a, 1), (&db, 1)],
        &[
            node("worker", "active", "ready"),
            node("worker", "active", "down"),
        ],
    );
    containers.update(&[
        container("c1", "running", ""),
        container("c2", "exited", "1"),
    ]);
    let first = setup.own_series();
    let contains = |series: &BTreeMap<String, f64>, needle: &str| {
        series.keys().any(|series| series.contains(needle))
    };
    for needle in [
        "service=\"db\"",
        "team=\"a\"",
        "status=\"down\"",
        "container=\"c2\"",
    ] {
        assert!(contains(&first, needle), "{needle} before");
    }
    let resync_zero = series_id("swarm_exporter_resyncs_total", &[("result", "failure")]);
    assert_eq!(first.get(&resync_zero), Some(&0.0));

    // The next poll: the db service is gone, api's team label changed, one node status, one
    // container. The per-service gauges are deleted as the reconciler deletes a removed service
    // or a changed identity.
    let api_b = metadata("web", "api", "replicated", Some("b"));
    for gone in [&api_a, &db] {
        collectors
            .replica_gauges
            .desired()
            .delete_with(&labels_for_metadata(gone));
        collectors
            .replica_gauges
            .schedulable()
            .delete_with(&labels_for_metadata(gone));
        collectors.service_update.clear(gone);
    }
    publish_poll(&setup, &[(&api_b, 1)], &[node("worker", "active", "ready")]);
    containers.update(&[container("c1", "running", "")]);
    let second = setup.own_series();
    for needle in [
        "service=\"db\"",
        "team=\"a\"",
        "status=\"down\"",
        "container=\"c2\"",
    ] {
        assert!(!contains(&second, needle), "{needle} after: {second:?}");
    }
    for needle in ["team=\"b\"", "container=\"c1\"", "status=\"ready\""] {
        assert!(contains(&second, needle), "{needle} after");
    }
    assert_eq!(
        second.get(&resync_zero),
        Some(&0.0),
        "a pre-seeded zero series stays"
    );
    assert_eq!(
        second.get(&series_id(
            "swarm_exporter_resyncs_total",
            &[("result", "success")]
        )),
        Some(&0.0)
    );
}

/// Atomicity: scrapes racing repeated snapshot swaps each see one whole poll. The poll A has 3
/// services with 1 running task, B has 5 with 2: a scrape's three families (which share one
/// snapshot) must agree on the count and the running total, and the nodes' snapshot is A's 3
/// workers or B's 2 managers and 1 worker, never a mix.
#[test]
fn a_scrape_racing_the_polls_sees_one_whole_poll() {
    let setup = Setup::new(&[]);
    let services: Vec<ServiceMetadata> = (0..5)
        .map(|index| metadata("web", &format!("svc{index}"), "replicated", None))
        .collect();
    let refs = |count: usize, running: usize| -> Vec<(&ServiceMetadata, usize)> {
        services
            .iter()
            .take(count)
            .map(|service| (service, running))
            .collect()
    };
    let nodes_a = vec![node("worker", "active", "ready"); 3];
    let nodes_b = vec![
        node("manager", "active", "ready"),
        node("manager", "active", "ready"),
        node("worker", "active", "ready"),
    ];
    publish_poll(&setup, &refs(3, 1), &nodes_a);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut flip = false;
            while !stop.load(Ordering::Relaxed) {
                if flip {
                    publish_poll(&setup, &refs(3, 1), &nodes_a);
                } else {
                    publish_poll(&setup, &refs(5, 2), &nodes_b);
                }
                flip = !flip;
            }
        });
        for _ in 0..3000 {
            let scrape = setup.scrape();
            let count = |name: &str| {
                scrape
                    .samples
                    .iter()
                    .filter(|sample| sample.name == name)
                    .count()
            };
            let running: f64 = scrape
                .samples
                .iter()
                .filter(|sample| sample.name == "swarm_service_running_replicas")
                .map(|sample| sample.value)
                .sum();
            let services = count("swarm_service_running_replicas");
            assert!(
                (services, running) == (3, 3.0) || (services, running) == (5, 10.0),
                "{services} services running {running}"
            );
            assert_eq!(count("swarm_service_at_desired"), services);
            assert_eq!(count("swarm_task_replicas_state"), 15 * services);
            let nodes = scrape
                .series()
                .into_iter()
                .filter(|(series, _)| series.starts_with("swarm_cluster_nodes_by_state"))
                .collect::<BTreeMap<_, _>>();
            let workers = series_id(
                "swarm_cluster_nodes_by_state",
                &[
                    ("availability", "active"),
                    ("role", "worker"),
                    ("status", "ready"),
                ],
            );
            let managers = series_id(
                "swarm_cluster_nodes_by_state",
                &[
                    ("availability", "active"),
                    ("role", "manager"),
                    ("status", "ready"),
                ],
            );
            let only_workers = BTreeMap::from([(workers.clone(), 3.0)]);
            let both = BTreeMap::from([(managers, 2.0), (workers, 1.0)]);
            assert!(nodes == only_workers || nodes == both, "{nodes:?}");
        }
        stop.store(true, Ordering::Relaxed);
    });
}
