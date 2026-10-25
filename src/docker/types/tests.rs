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

use super::*;

const FIXTURE_SETS: [&str; 2] = ["api-1.51", "api-1.56"];

fn fixture(set: &str, name: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "internal/collector/testdata/engine_wire",
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

/// Both fixture sets describe the same cluster in two API versions' shapes, so what the
/// exporter reads from them must be identical.
#[test]
fn both_fixture_sets_decode_to_the_same_objects() {
    let [old, new] = FIXTURE_SETS;
    let lists = |set| {
        (
            decode_list::<Service>(&fixture(set, "services.json")).unwrap(),
            decode_list::<Node>(&fixture(set, "nodes.json")).unwrap(),
            decode_list::<Task>(&fixture(set, "tasks.json")).unwrap(),
            decode_list::<ContainerSummary>(&fixture(set, "containers.json")).unwrap(),
        )
    };
    assert_eq!(lists(old), lists(new));
    for service in ["svc-agent", "svc-api", "svc-db"] {
        let name = format!("service-{service}.json");
        let decoded = |set| decode_first::<Service>(&fixture(set, &name)).unwrap();
        assert_eq!(decoded(old), decoded(new), "{name}");
        assert_eq!(decoded(new).id, service);
    }
    for container in [
        "c-compose-web",
        "c-compose-worker",
        "c-plain",
        "c-swarm-api",
    ] {
        let name = format!("container-{container}.json");
        let decoded = |set| decode_first::<ContainerInspect>(&fixture(set, &name)).unwrap();
        assert_eq!(decoded(old), decoded(new), "{name}");
    }
}

#[test]
fn services_decode_the_fields_the_exporter_reads() {
    let services = decode_list::<Service>(&fixture("api-1.56", "services.json")).unwrap();
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
fn nodes_tasks_and_containers_decode_the_fields_the_exporter_reads() {
    let nodes = decode_list::<Node>(&fixture("api-1.56", "nodes.json")).unwrap();
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

    let tasks = decode_list::<Task>(&fixture("api-1.56", "tasks.json")).unwrap();
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

    let containers =
        decode_list::<ContainerSummary>(&fixture("api-1.56", "containers.json")).unwrap();
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

    let web =
        decode_first::<ContainerInspect>(&fixture("api-1.56", "container-c-compose-web.json"))
            .unwrap();
    assert_eq!(web.config.unwrap().healthcheck, Some(Present {}));
    assert_eq!(web.state.unwrap().health.unwrap().status, "healthy");
    let worker =
        decode_first::<ContainerInspect>(&fixture("api-1.56", "container-c-compose-worker.json"))
            .unwrap();
    assert_eq!(worker.config.unwrap().healthcheck, None);
    let state = worker.state.unwrap();
    assert_eq!(state.exit_code, 137);
    assert_eq!(state.health, None);
}

#[test]
fn null_and_missing_fields_are_the_zero_value() {
    let task: Task = decode_first(
        br#"{"ServiceID":null,"Slot":null,"Status":null,"CreatedAt":null,"Version":{"Index":null}}"#,
    )
    .unwrap();
    assert_eq!(task, Task::default());
    assert!(task.created_at.is_zero());
    assert!(task.status.timestamp.is_zero());

    let service: Service =
        decode_first(br#"{"ID":"a","Spec":{"Labels":null,"Mode":null}}"#).unwrap();
    assert_eq!(service.id, "a");
    assert!(service.spec.labels.is_empty());
    assert_eq!(service.spec.mode, ServiceMode::default());

    // A null pointer stays absent; Go's zero time behind a pointer is present.
    let update: UpdateStatus =
        decode_first(br#"{"StartedAt":null,"CompletedAt":"0001-01-01T00:00:00Z"}"#).unwrap();
    assert_eq!(update.started_at, None);
    assert!(update.completed_at.unwrap().is_zero());
    assert_eq!(update.completed_at.unwrap().unix(), -62_135_596_800);
}

#[test]
fn null_lists_and_elements_are_empty_and_zero() {
    assert_eq!(decode_list::<Node>(b"null").unwrap(), []);
    assert_eq!(decode_list::<Node>(b" []").unwrap(), []);
    let nodes = decode_list::<Node>(br#"[null,{"ID":"n"}]"#).unwrap();
    assert_eq!(nodes[0], Node::default());
    assert_eq!(nodes[1].id, "n");
}

/// Unknown values of state fields are data, not decode errors: the collectors apply their
/// fallback rules to them.
#[test]
fn unknown_enum_values_decode_as_strings() {
    let task: Task =
        decode_first(br#"{"DesiredState":"hibernating","Status":{"State":"teleporting"}}"#)
            .unwrap();
    assert_eq!(task.desired_state, "hibernating");
    assert_eq!(task.status.state, "teleporting");
    let node: Node = decode_first(
        br#"{"Spec":{"Role":"observer","Availability":"asleep"},"Status":{"State":"lost"}}"#,
    )
    .unwrap();
    assert_eq!(
        (
            node.spec.role.as_str(),
            node.spec.availability.as_str(),
            node.status.state.as_str()
        ),
        ("observer", "asleep", "lost")
    );
    let update: UpdateStatus = decode_first(br#"{"State":"rolling_sideways"}"#).unwrap();
    assert_eq!(update.state, "rolling_sideways");
    let container: ContainerSummary = decode_first(br#"{"State":"frozen"}"#).unwrap();
    assert_eq!(container.state, "frozen");
    let inspect: ContainerInspect =
        decode_first(br#"{"State":{"Health":{"Status":"meh"}}}"#).unwrap();
    assert_eq!(inspect.state.unwrap().health.unwrap().status, "meh");
    let event: Event = decode_first(br#"{"Type":"plugin","Action":"brand-new"}"#).unwrap();
    assert_eq!(
        (event.kind.as_str(), event.action.as_str()),
        ("plugin", "brand-new")
    );
}

/// Go's `json.NewDecoder(body).Decode` reads the first value and ignores the rest.
#[test]
fn decode_first_reads_like_a_go_decoder() {
    assert_eq!(
        decode_first::<Vec<Node>>(b"  [] trailing garbage").unwrap(),
        []
    );
    assert_eq!(decode_first::<Node>(b"").unwrap_err(), "EOF");
    assert_eq!(decode_first::<Node>(b" \n\t").unwrap_err(), "EOF");
    assert_eq!(
        decode_first::<Node>(br#"{"ID":"n"#).unwrap_err(),
        "unexpected EOF"
    );
    assert!(decode_first::<Node>(br#"{"ID":5}"#).is_err());
    assert!(
        decode_first::<Task>(br#"{"CreatedAt":"yesterday"}"#)
            .unwrap_err()
            .contains("parsing time \"yesterday\"")
    );
}

#[test]
fn go_time_compares_instants_and_rounds_unix_down() {
    let utc = at("2026-09-20T10:00:00.5Z");
    let offset = at("2026-09-20T12:00:00.5+02:00");
    assert_eq!(utc, offset);
    assert_eq!(utc.unix(), 1_789_898_400);
    assert_eq!(at("1969-12-31T23:59:59.5Z").unix(), -1);
    assert!(GoTime::default().is_zero());
    assert!(!utc.is_zero());
}
