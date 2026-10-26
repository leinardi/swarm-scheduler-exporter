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
use std::time::Duration;

use tokio::time::Instant;

use super::*;
use crate::docker::Error;
use crate::docker::types::{ContainerConfig, ContainerState, ContainerSummary, Health, Present};
use crate::logger::Level;
use crate::metrics::testing::{gather_series, series_id};
use crate::test_support::{FakeDocker, capture_logs};

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// Go: `TestFirstContainerName`
#[test]
fn first_container_name_strips_one_slash() {
    let cases: [(&[&str], &str); 4] = [
        (&[], ""),
        (&["/mycontainer"], "mycontainer"),
        (&["mycontainer"], "mycontainer"),
        (&["/first", "/second"], "first"),
    ];
    for (given, want) in cases {
        assert_eq!(first_container_name(&names(given)), want, "{given:?}");
    }
    // Only the one leading slash goes, and nothing is sanitized.
    assert_eq!(first_container_name(&names(&["//odd name"])), "/odd name");
}

/// Go: `TestClassifyOrchestrator`
#[test]
fn classify_orchestrator_by_labels() {
    let cases: [(&str, Labels, [&str; 4]); 5] = [
        (
            "compose labels",
            labels(&[
                (COMPOSE_PROJECT_LABEL, "myproj"),
                (COMPOSE_SERVICE_LABEL, "web"),
            ]),
            ["myproj", "web", "", "compose"],
        ),
        (
            "swarm labels with stack: the stack prefix is dropped",
            labels(&[
                (SWARM_SERVICE_NAME_LABEL, "mystack_web"),
                (STACK_NAMESPACE_LABEL, "mystack"),
            ]),
            ["", "web", "mystack", "swarm"],
        ),
        (
            "swarm with explicit compose service, which takes precedence",
            labels(&[
                (SWARM_SERVICE_NAME_LABEL, "mystack_web"),
                (STACK_NAMESPACE_LABEL, "mystack"),
                (COMPOSE_SERVICE_LABEL, "api"),
            ]),
            ["", "api", "mystack", "swarm"],
        ),
        ("no labels", Labels::new(), ["", "", "", "none"]),
        (
            "spaces trimmed",
            labels(&[
                (COMPOSE_PROJECT_LABEL, "  proj  "),
                (COMPOSE_SERVICE_LABEL, " svc "),
            ]),
            ["proj", "svc", "", "compose"],
        ),
    ];
    for (name, given, [project, service, stack, orchestrator]) in cases {
        assert_eq!(
            classify_orchestrator(&given),
            Orchestration {
                project: project.to_owned(),
                service: service.to_owned(),
                stack: stack.to_owned(),
                orchestrator,
            },
            "{name}"
        );
    }
}

/// Go: `TestDisplayNameForContainer`
#[test]
fn display_name_falls_back_to_the_container_name() {
    for (group, service, want) in [
        ("", "", "mycontainer"),
        ("", "svc", "mycontainer"),
        ("grp", "", "mycontainer"),
        ("grp", "svc", "grp svc"),
        ("same", "same", "same"),
    ] {
        assert_eq!(
            display_name_for_container(group, service, "mycontainer"),
            want
        );
    }
}

/// Go's `makeRow`.
fn make_row(state: &str) -> Row {
    Row {
        id: "cid".to_owned(),
        state: state.to_owned(),
        labels: ContainerRow::from([
            (LABEL_STATE.to_owned(), String::new()),
            (LABEL_EXIT_CODE.to_owned(), String::new()),
        ]),
        need: InspectNeed::None,
    }
}

/// Go's `buildInspectWithHealth`.
fn inspect_with_health(status: &str) -> ContainerInspect {
    ContainerInspect {
        config: Some(ContainerConfig {
            healthcheck: Some(Present {}),
        }),
        state: Some(ContainerState {
            health: Some(Health {
                status: status.to_owned(),
            }),
            ..ContainerState::default()
        }),
    }
}

/// Go's `buildInspectWithExitCode`.
fn inspect_with_exit_code(code: i64) -> ContainerInspect {
    ContainerInspect {
        state: Some(ContainerState {
            exit_code: code,
            ..ContainerState::default()
        }),
        ..ContainerInspect::default()
    }
}

/// Go: `TestApplyHealthOverlay`
#[test]
fn health_overlay_maps_the_three_health_states() {
    let no_healthcheck = ContainerInspect {
        config: Some(ContainerConfig::default()),
        ..ContainerInspect::default()
    };
    // A health without a healthcheck in the config is not read.
    let mut health_only = inspect_with_health("healthy");
    health_only.config = None;
    let cases = [
        (
            "nil config falls back to base",
            ContainerInspect::default(),
            "running",
        ),
        (
            "nil healthcheck falls back to base",
            no_healthcheck,
            "running",
        ),
        (
            "health without healthcheck falls back to base",
            health_only,
            "running",
        ),
        ("healthy", inspect_with_health("healthy"), "healthy"),
        ("unhealthy", inspect_with_health("unhealthy"), "unhealthy"),
        (
            "starting",
            inspect_with_health("starting"),
            "health_starting",
        ),
        (
            "case insensitive",
            inspect_with_health("HEALTHY"),
            "healthy",
        ),
        (
            "whitespace trimmed",
            inspect_with_health("  unhealthy  "),
            "unhealthy",
        ),
        (
            "unknown status falls back to base",
            inspect_with_health("unknown"),
            "running",
        ),
        (
            "none falls back to base",
            inspect_with_health("none"),
            "running",
        ),
    ];
    for (name, inspect, want) in cases {
        let mut row = make_row("running");
        apply_health_overlay(&mut row, &inspect);
        assert_eq!(label(&row.labels, LABEL_STATE), want, "{name}");
    }
}

/// Go: `TestApplyExitCode`
#[test]
fn exit_code_only_for_exited_containers() {
    let cases = [
        (
            "nil state leaves exit_code empty",
            "exited",
            ContainerInspect::default(),
            "",
        ),
        (
            "non-exited state leaves exit_code empty",
            "running",
            inspect_with_exit_code(1),
            "",
        ),
        (
            "exited with code 0",
            "exited",
            inspect_with_exit_code(0),
            "0",
        ),
        (
            "exited with code 137 (OOM kill)",
            "exited",
            inspect_with_exit_code(137),
            "137",
        ),
        (
            "case-insensitive exited match",
            "Exited",
            inspect_with_exit_code(1),
            "1",
        ),
    ];
    for (name, state, inspect, want) in cases {
        let mut row = make_row(state);
        apply_exit_code(&mut row, &inspect);
        assert_eq!(label(&row.labels, LABEL_EXIT_CODE), want, "{name}");
        assert_eq!(
            label(&row.labels, LABEL_STATE),
            state,
            "{name}: the state is kept"
        );
    }
}

fn containers_state(config: ContainersConfig) -> ContainersState {
    ContainersState::new(config, capture_logs(Level::INFO).0)
}

fn enabled() -> ContainersState {
    containers_state(ContainersConfig::new(true, false))
}

fn summary(id: &str, state: &str, name: &str, pairs: &[(&str, &str)]) -> ContainerSummary {
    ContainerSummary {
        id: id.to_owned(),
        state: state.to_owned(),
        names: names(&[name]),
        labels: labels(pairs),
    }
}

fn fake(containers: Vec<ContainerSummary>) -> FakeDocker {
    FakeDocker {
        containers,
        ..FakeDocker::default()
    }
}

async fn poll(state: &ContainersState, docker: &FakeDocker) -> Vec<ContainerRow> {
    state
        .poll(docker)
        .await
        .unwrap_or_else(|error| panic!("poll: {error}"))
}

/// Go: `TestPollContainersState_Disabled_ReturnsNil`: no rows and no call, not even a list that
/// would fail.
#[tokio::test]
async fn disabled_polls_nothing() {
    let docker = FakeDocker {
        container_list_err: Some(Error::deadline_exceeded()),
        ..FakeDocker::default()
    };
    let state = containers_state(ContainersConfig::new(false, true));
    assert!(poll(&state, &docker).await.is_empty());
    assert_eq!(docker.inspect_calls(), 0);
}

/// Go: `TestPollContainersState_SwarmContainerExcluded`
#[tokio::test]
async fn swarm_containers_are_excluded_by_default() {
    let docker = fake(vec![
        summary(
            "c1",
            "running",
            "/svc_web_1",
            &[(SWARM_SERVICE_NAME_LABEL, "mystack_web")],
        ),
        summary("c2", "running", "/plainapp", &[]),
    ]);
    let rows = poll(&enabled(), &docker).await;
    assert_eq!(rows.len(), 1, "swarm filtered");
    assert_eq!(label(&rows[0], LABEL_CONTAINER), "plainapp");
    // A blank Swarm service label is no Swarm container.
    let docker = fake(vec![summary(
        "c1",
        "created",
        "/x",
        &[(SWARM_SERVICE_NAME_LABEL, " ")],
    )]);
    assert_eq!(poll(&enabled(), &docker).await.len(), 1);
}

/// Go: `TestPollContainersState_SwarmContainerIncluded`, with the row it gives.
#[tokio::test]
async fn swarm_containers_are_included_when_asked() {
    let docker = fake(vec![summary(
        "c1",
        "created",
        "/svc_web_1",
        &[
            (SWARM_SERVICE_NAME_LABEL, "mystack_web"),
            (STACK_NAMESPACE_LABEL, "mystack"),
        ],
    )]);
    let rows = poll(
        &containers_state(ContainersConfig::new(true, true)),
        &docker,
    )
    .await;
    assert_eq!(
        rows,
        vec![ContainerRow::from(
            [
                (LABEL_PROJECT, ""),
                (LABEL_STACK, "mystack"),
                (LABEL_SERVICE, "web"),
                (LABEL_CONTAINER, "svc_web_1"),
                (LABEL_ORCHESTRATOR, "swarm"),
                (LABEL_DISPLAY_NAME, "mystack web"),
                (LABEL_STATE, "created"),
                (LABEL_EXIT_CODE, ""),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        )]
    );
    assert_eq!(
        docker.inspect_calls(),
        0,
        "a created container needs no inspect"
    );
}

/// Go: `TestPollContainersState_HealthOverlay`
#[tokio::test]
async fn a_healthy_container_reports_healthy() {
    let mut docker = fake(vec![summary("c1", "running", "/myapp", &[])]);
    docker
        .inspects
        .insert("c1".to_owned(), inspect_with_health("healthy"));
    let rows = poll(&enabled(), &docker).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(label(&rows[0], LABEL_STATE), "healthy");
}

/// Go: `TestPollContainersState_ExitCode`
#[tokio::test]
async fn an_exited_container_reports_its_exit_code() {
    let mut docker = fake(vec![summary("c1", "exited", "/done", &[])]);
    docker
        .inspects
        .insert("c1".to_owned(), inspect_with_exit_code(137));
    let rows = poll(&enabled(), &docker).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(label(&rows[0], LABEL_EXIT_CODE), "137");
}

/// Go: `TestPollContainersState_InspectFailure_FallsBackToBaseState`: the failure is logged,
/// not fatal.
#[tokio::test]
async fn a_failed_inspect_keeps_the_listed_state() {
    let docker = FakeDocker {
        inspect_err: Some(Error::deadline_exceeded()),
        ..fake(vec![summary("c1", "running", "/flaky", &[])])
    };
    let (logger, sink) = capture_logs(Level::INFO);
    let state = ContainersState::new(ContainersConfig::new(true, false), logger);
    let rows = poll(&state, &docker).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(label(&rows[0], LABEL_STATE), "running");
    assert_eq!(
        sink.contents(),
        "level=WARN container inspect failed; using base state container_id=c1 \
         err=\"context deadline exceeded\"\n"
    );
}

/// A failed list fails the poll with the Go wording, and leaves the published set alone: the
/// caller publishes only the rows of a poll that succeeded.
#[tokio::test]
async fn a_failed_list_keeps_the_previous_set() {
    let state = enabled();
    let rows = [container_row("web", "running", "")];
    state.update(&rows);
    let docker = FakeDocker {
        container_list_err: Some(Error::deadline_exceeded()),
        ..FakeDocker::default()
    };
    let error = state.poll(&docker).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "container list: context deadline exceeded"
    );
    assert_eq!(gathered(&state), expected_series(&rows));
}

fn capped(cap: usize) -> ContainersState {
    containers_state(ContainersConfig {
        inspect_cap: cap,
        ..ContainersConfig::new(true, false)
    })
}

/// Go: `TestPollContainersState_InspectCapHonored`
#[tokio::test]
async fn the_inspect_cap_is_honoured() {
    let mut docker = fake(vec![
        summary("c1", "exited", "/a", &[]),
        summary("c2", "exited", "/b", &[]),
    ]);
    docker
        .inspects
        .insert("c1".to_owned(), inspect_with_exit_code(0));
    docker
        .inspects
        .insert("c2".to_owned(), inspect_with_exit_code(1));
    let rows = poll(&capped(1), &docker).await;
    assert_eq!(docker.inspect_calls(), 1);
    // In list order: the first got its exit code, the second was not reached.
    let exit_codes: Vec<&str> = rows.iter().map(|row| label(row, LABEL_EXIT_CODE)).collect();
    assert_eq!(exit_codes, ["0", ""]);
}

/// Go: `TestPollContainersState_FailedInspectsCountTowardCap`
#[tokio::test]
async fn failed_inspects_count_toward_the_cap() {
    let docker = FakeDocker {
        inspect_err: Some(Error::deadline_exceeded()),
        ..fake(vec![
            summary("c1", "exited", "/a", &[]),
            summary("c2", "exited", "/b", &[]),
        ])
    };
    let rows = poll(&capped(1), &docker).await;
    assert_eq!(
        docker.inspect_calls(),
        1,
        "a failed inspect still counts toward cap=1"
    );
    for row in &rows {
        assert_eq!(label(row, LABEL_STATE), "exited", "{row:?}");
        assert_eq!(label(row, LABEL_EXIT_CODE), "", "{row:?}");
    }
}

/// Go: `TestPollContainersState_EnrichmentDeadlineBoundsAllInspects`: with inspects that never
/// answer, one deadline ends the first and no other is attempted.
#[tokio::test]
async fn one_deadline_bounds_all_inspects() {
    let containers = ["c1", "c2", "c3", "c4", "c5"]
        .map(|id| summary(id, "running", &format!("/{id}"), &[]))
        .to_vec();
    let docker = FakeDocker {
        inspect_blocks: true,
        ..fake(containers)
    };
    let state = containers_state(ContainersConfig {
        enrich_timeout: Duration::from_millis(50),
        ..ContainersConfig::new(true, false)
    });
    let started = Instant::now();
    let rows = poll(&state, &docker).await;
    // Each inspect waiting out its own 5s would take 25s here.
    assert!(
        started.elapsed() < CONTAINERS_LIST_TIMEOUT,
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(
        docker.inspect_calls(),
        1,
        "none after the enrichment deadline"
    );
    for row in &rows {
        assert_eq!(label(row, LABEL_STATE), "running", "{row:?}");
    }
}

/// The skipped rows say why at debug, as Go's `canInspect` does for each.
#[tokio::test]
async fn skipped_inspects_are_logged_at_debug() {
    let (logger, sink) = capture_logs(Level::DEBUG);
    let state = ContainersState::new(
        ContainersConfig {
            inspect_cap: 0,
            ..ContainersConfig::new(true, false)
        },
        logger,
    );
    let docker = fake(vec![
        summary("c1", "exited", "/a", &[]),
        summary("c2", "paused", "/b", &[]),
    ]);
    poll(&state, &docker).await;
    assert_eq!(
        sink.contents(),
        "level=DEBUG containers inspect cap reached, skipping remaining enrichments cap=0\n"
    );
}

/// Go's `containerRow`: a row as a poll returns it, for a plain container.
fn container_row(name: &str, state: &str, exit_code: &str) -> ContainerRow {
    [
        (LABEL_PROJECT, ""),
        (LABEL_STACK, ""),
        (LABEL_SERVICE, ""),
        (LABEL_CONTAINER, name),
        (LABEL_ORCHESTRATOR, ORCHESTRATOR_NONE),
        (LABEL_DISPLAY_NAME, name),
        (LABEL_STATE, state),
        (LABEL_EXIT_CODE, exit_code),
    ]
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .into()
}

/// Go's `expectedContainerSeries`: every known state per row, 1 for its own.
fn expected_series(rows: &[ContainerRow]) -> BTreeMap<String, f64> {
    let mut expected = BTreeMap::new();
    for row in rows {
        for state in KNOWN_CONTAINER_STATES {
            let mut labels = row.clone();
            labels.insert(LABEL_STATE.to_owned(), state.to_owned());
            let pairs: Vec<(&str, &str)> = labels
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let value = if label(row, LABEL_STATE) == state {
                1.0
            } else {
                0.0
            };
            expected.insert(series_id(CONTAINER_STATE_NAME, &pairs), value);
        }
    }
    expected
}

fn gathered(state: &ContainersState) -> BTreeMap<String, f64> {
    gather_series(Arc::clone(state.family()) as Arc<dyn Collector>)
}

/// Go: `TestUpdateContainersStateGauge_OneHotPerContainer`
#[test]
fn one_hot_per_container() {
    let state = enabled();
    let rows = [
        container_row("web", "running", ""),
        container_row("job", "exited", "1"),
    ];
    state.update(&rows);
    assert_eq!(gathered(&state), expected_series(&rows));
}

/// Go: `TestUpdateContainersStateGauge_Disabled_PublishesNothing`
#[test]
fn disabled_publishes_nothing() {
    let state = containers_state(ContainersConfig::new(false, false));
    state.update(&[container_row("web", "running", "")]);
    assert!(gathered(&state).is_empty());
}

/// Go: `TestUpdateContainersStateGauge_BuildDoesNotPublish` (#72): a scrape sees the whole
/// previous set while the next one is built.
#[test]
fn a_build_publishes_nothing_until_published() {
    let state = enabled();
    let rows_a = [
        container_row("a1", "running", ""),
        container_row("a2", "exited", "137"),
    ];
    let rows_b = [container_row("b1", "healthy", "")];
    let (expected_a, expected_b) = (expected_series(&rows_a), expected_series(&rows_b));
    state.update(&rows_a);
    assert_eq!(gathered(&state), expected_a);
    let builder = build_containers_state(state.family(), &rows_b);
    assert_eq!(gathered(&state), expected_a);
    let families = builder.build().unwrap();
    assert_eq!(gathered(&state), expected_a);
    state.family().collector().publish(families);
    assert_eq!(gathered(&state), expected_b);
}

/// A row missing a label fails the build: logged, and the previous set stays.
#[test]
fn a_failed_build_keeps_the_previous_set() {
    let (logger, sink) = capture_logs(Level::INFO);
    let state = ContainersState::new(ContainersConfig::new(true, false), logger);
    let rows = [container_row("web", "running", "")];
    state.update(&rows);
    let mut broken = container_row("bad", "running", "");
    broken.remove(LABEL_PROJECT);
    state.update(&[broken]);
    assert_eq!(gathered(&state), expected_series(&rows));
    assert!(
        sink.contents()
            .starts_with("level=ERROR build container state snapshot; keeping previous err="),
        "{}",
        sink.contents()
    );
}

/// The family keeps Go's name, HELP and label order.
#[test]
fn family_carries_go_name_help_and_labels() {
    let state = enabled();
    let desc = state.family().desc();
    assert_eq!(desc.name(), CONTAINER_STATE_NAME);
    assert_eq!(desc.help(), CONTAINER_STATE_HELP);
    assert_eq!(
        desc.label_names(),
        [
            "project",
            "stack",
            "service",
            "container",
            "orchestrator",
            "display_name",
            "state",
            "exit_code"
        ]
    );
    let registry = Registry::new();
    state.register(&registry).unwrap();
    assert!(state.register(&registry).is_err());
}
