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

use std::path::PathBuf;

use docker_client::bollard::models as wire;
use serde::de::DeserializeOwned;

use super::*;

const FIXTURE_SETS: [&str; 2] = ["api-1.51", "api-1.56"];

fn fixture(set: &str, name: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests/testdata/engine_wire",
        set,
        name,
    ]
    .iter()
    .collect();
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn at(text: &str) -> GoTime {
    GoTime::new(text.parse().unwrap())
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> T {
    serde_json::from_slice(body).unwrap()
}

fn services(set: &str) -> Vec<Service> {
    decode::<Vec<wire::Service>>(&fixture(set, "services.json"))
        .into_iter()
        .map(|value| service(value).unwrap())
        .collect()
}

fn nodes(set: &str) -> Vec<Node> {
    decode::<Vec<wire::Node>>(&fixture(set, "nodes.json"))
        .into_iter()
        .map(node)
        .collect()
}

fn tasks(set: &str) -> Vec<Task> {
    decode::<Vec<wire::Task>>(&fixture(set, "tasks.json"))
        .into_iter()
        .map(|value| task(value).unwrap())
        .collect()
}

fn containers(set: &str) -> Vec<ContainerSummary> {
    decode::<Vec<wire::ContainerSummary>>(&fixture(set, "containers.json"))
        .into_iter()
        .map(container_summary)
        .collect()
}

fn inspect(set: &str, name: &str) -> ContainerInspect {
    container_inspect(decode(&fixture(set, name)))
}

/// Both fixture sets describe the same cluster in two API versions' shapes, so what the
/// exporter reads from them must be identical.
#[test]
fn both_fixture_sets_convert_to_the_same_objects() {
    let [old, new] = FIXTURE_SETS;
    assert_eq!(services(old), services(new));
    assert_eq!(nodes(old), nodes(new));
    assert_eq!(tasks(old), tasks(new));
    assert_eq!(containers(old), containers(new));
    for id in ["svc-agent", "svc-api", "svc-db"] {
        let name = format!("service-{id}.json");
        let converted = |set| service(decode(&fixture(set, &name))).unwrap();
        assert_eq!(converted(old), converted(new), "{name}");
        assert_eq!(converted(new).id, id);
    }
    for container in [
        "c-compose-web",
        "c-compose-worker",
        "c-plain",
        "c-swarm-api",
    ] {
        let name = format!("container-{container}.json");
        assert_eq!(inspect(old, &name), inspect(new, &name), "{name}");
    }
}

#[test]
fn services_keep_the_fields_the_exporter_reads() {
    let services = services("api-1.56");
    let ids: Vec<&str> = services.iter().map(|service| service.id.as_str()).collect();
    assert_eq!(ids, ["svc-api", "svc-agent", "svc-db", "svc-cron"]);

    let api = &services[0];
    assert_eq!(api.spec.name, "web_api");
    assert_eq!(
        api.spec.mode.replicated,
        Some(ReplicatedService { replicas: Some(3) })
    );
    assert_eq!(api.spec.mode.global, None);
    let placement = api.spec.task_template.placement.as_ref().unwrap();
    assert_eq!(placement.constraints, ["node.role==worker"]);
    assert!(placement.platforms.is_empty());
    assert_eq!(
        api.spec
            .task_template
            .restart_policy
            .as_ref()
            .unwrap()
            .condition,
        "any"
    );
    assert_eq!(api.update_status, None);

    let agent = &services[1];
    assert_eq!(agent.spec.mode.global, Some(Present {}));
    assert_eq!(agent.spec.mode.replicated, None);
    assert_eq!(
        agent
            .spec
            .task_template
            .placement
            .as_ref()
            .unwrap()
            .platforms,
        [Platform {
            architecture: "amd64".to_owned(),
            os: "linux".to_owned()
        }]
    );
    let update = agent.update_status.as_ref().unwrap();
    assert_eq!(update.state, "updating");
    assert_eq!(update.started_at, Some(at("2026-09-19T12:00:00Z")));
    assert_eq!(update.completed_at, None);

    let db = &services[2];
    assert_eq!(db.spec.task_template.restart_policy, None);
    assert_eq!(
        db.update_status.as_ref().unwrap().completed_at,
        Some(at("2026-09-18T08:00:00Z"))
    );

    let cron = &services[3];
    assert_eq!(
        cron.update_status.as_ref().unwrap().state,
        "rollback_paused"
    );
    assert_eq!(cron.spec.task_template.placement, None);
}

#[test]
fn nodes_tasks_and_containers_keep_the_fields_the_exporter_reads() {
    let nodes = nodes("api-1.56");
    let manager = &nodes[0];
    assert_eq!(manager.id, "node-mgr1");
    assert_eq!(manager.spec.role, "manager");
    assert_eq!(manager.spec.availability, "active");
    assert_eq!(
        manager.spec.labels.get("zone").map(String::as_str),
        Some("a")
    );
    assert_eq!(manager.status.state, "ready");
    assert_eq!(manager.status.addr, "10.0.0.1");
    assert_eq!(manager.description.platform.architecture, "x86_64");
    assert_eq!(
        manager
            .description
            .engine
            .labels
            .get("tier")
            .map(String::as_str),
        Some("core")
    );
    assert!(nodes[1].description.engine.labels.is_empty());
    assert_eq!(nodes[3].status.state, "down");

    let tasks = tasks("api-1.56");
    assert_eq!(tasks.len(), 9);
    let old = &tasks[0];
    assert_eq!(old.service_id, "svc-api");
    assert_eq!(old.node_id, "node-w1");
    assert_eq!(old.slot, 1);
    assert_eq!(old.desired_state, "shutdown");
    assert_eq!(old.status.state, "failed");
    assert_eq!(old.status.timestamp, at("2026-09-20T10:00:00Z"));
    assert_eq!(old.created_at, at("2026-09-20T10:00:00Z"));
    assert_eq!(old.version.index, 300);
    assert_eq!(old.job_iteration, None);
    assert!(tasks[1].created_at > old.created_at);

    let containers = containers("api-1.56");
    let states: Vec<(&str, &str)> = containers
        .iter()
        .map(|container| (container.id.as_str(), container.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [
            ("c-compose-web", "running"),
            ("c-compose-worker", "exited"),
            ("c-plain", "running"),
            ("c-paused", "paused"),
            ("c-swarm-api", "running"),
        ]
    );
    assert_eq!(containers[0].names, ["/shop-web-1"]);
    assert_eq!(
        containers[0]
            .labels
            .get("com.docker.compose.project")
            .map(String::as_str),
        Some("shop")
    );

    let web = inspect("api-1.56", "container-c-compose-web.json");
    assert_eq!(web.config.unwrap().healthcheck, Some(Present {}));
    assert_eq!(web.state.unwrap().health.unwrap().status, "healthy");
    let worker = inspect("api-1.56", "container-c-compose-worker.json");
    assert_eq!(worker.config.unwrap().healthcheck, None);
    let state = worker.state.unwrap();
    assert_eq!(state.exit_code, 137);
    assert_eq!(state.health, None);
}

#[test]
fn null_and_missing_fields_are_the_zero_value() {
    let converted = task(decode(
        br#"{"ServiceID":null,"Slot":null,"Status":null,"CreatedAt":null,"Version":{"Index":null}}"#,
    ))
    .unwrap();
    assert_eq!(converted, Task::default());
    assert!(converted.created_at.is_zero());
    assert!(converted.status.timestamp.is_zero());

    let converted = service(decode(br#"{"ID":"a","Spec":{"Labels":null,"Mode":null}}"#)).unwrap();
    assert_eq!(converted.id, "a");
    assert!(converted.spec.labels.is_empty());
    assert_eq!(converted.spec.mode, ServiceMode::default());

    // A null pointer stays absent; Go's zero time behind a pointer is present.
    let converted = service(decode(
        br#"{"UpdateStatus":{"StartedAt":null,"CompletedAt":"0001-01-01T00:00:00Z"}}"#,
    ))
    .unwrap();
    let update = converted.update_status.unwrap();
    assert_eq!(update.started_at, None);
    assert!(update.completed_at.unwrap().is_zero());
    assert_eq!(update.completed_at.unwrap().unix(), -62_135_596_800);
}

#[test]
fn a_timestamp_that_is_not_rfc_3339_is_an_error() {
    let error = task(decode(br#"{"CreatedAt":"yesterday"}"#)).unwrap_err();
    assert!(
        error.to_string().contains("parsing time \"yesterday\""),
        "{error}"
    );
}

/// What bollard decodes is the daemon's published set of values; one it does not know fails the
/// whole call instead of reaching the collectors' fallback rules as text (a change from the Go
/// exporter, which kept every state as a string).
#[test]
fn an_unknown_enum_value_is_refused_by_bollard() {
    assert!(serde_json::from_slice::<wire::Task>(br#"{"DesiredState":"hibernating"}"#).is_err());
    assert!(serde_json::from_slice::<wire::Node>(br#"{"Status":{"State":"lost"}}"#).is_err());
    // The values the daemon documents keep their wire text, the empty one included.
    let known = task(decode(
        br#"{"DesiredState":"shutdown","Status":{"State":"orphaned"}}"#,
    ))
    .unwrap();
    assert_eq!(known.desired_state, "shutdown");
    assert_eq!(known.status.state, "orphaned");
    let summary = container_summary(decode(br#"{"State":""}"#));
    assert_eq!(summary.state, "");
}

#[test]
fn an_event_keeps_its_type_action_actor_and_time() {
    let converted = event(decode(
        br#"{"Type":"service","Action":"update","Actor":{"ID":"svc-api"},"time":10,"timeNano":10000000123}"#,
    ));
    assert_eq!(
        (converted.kind.as_str(), converted.action.as_str()),
        ("service", "update")
    );
    assert_eq!(converted.actor.id, "svc-api");
    assert_eq!((converted.time, converted.time_nano), (10, 10_000_000_123));
}
