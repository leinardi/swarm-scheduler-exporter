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
use std::path::PathBuf;

use super::*;
use crate::collector::metrics_ids::{SERVICE_MODE_GLOBAL, SERVICE_MODE_GLOBAL_JOB};
use crate::docker::types::{EngineDescription, NodeDescription, NodeSpec, NodeStatus};
use crate::test_support::schedulable_node;

fn platform(os: &str, architecture: &str) -> Platform {
    Platform {
        architecture: architecture.to_owned(),
        os: os.to_owned(),
    }
}

fn constraints(expressions: &[&str]) -> Vec<String> {
    expressions
        .iter()
        .map(|&expression| expression.to_owned())
        .collect()
}

/// Go: `TestIsNodeSchedulable`
#[test]
fn is_node_schedulable_needs_ready_and_active() {
    let ready = schedulable_node("n1", "host1");
    let with = |change: fn(&mut Node)| {
        let mut node = ready.clone();
        change(&mut node);
        node
    };
    let cases = [
        ("nil node", None, false),
        ("ready+active", Some(ready.clone()), true),
        (
            "not ready",
            Some(with(|node| node.status.state = "down".to_owned())),
            false,
        ),
        (
            "paused",
            Some(with(|node| node.spec.availability = "pause".to_owned())),
            false,
        ),
        (
            "drain",
            Some(with(|node| node.spec.availability = "drain".to_owned())),
            false,
        ),
    ];
    for (name, node, want) in cases {
        assert_eq!(is_node_schedulable(node.as_ref()), want, "{name}");
    }
}

/// Go: `TestPlatformMatches`
#[test]
fn platform_matches_any_required_platform() {
    let node = schedulable_node("n1", "host1");
    let cases = [
        (
            "exact match linux/amd64",
            vec![platform("linux", "amd64")],
            true,
        ),
        (
            "case-insensitive OS",
            vec![platform("Linux", "AMD64")],
            true,
        ),
        ("OS wildcard", vec![platform("", "amd64")], true),
        ("arch wildcard", vec![platform("linux", "")], true),
        ("both wildcards", vec![platform("", "")], true),
        ("OS mismatch", vec![platform("windows", "amd64")], false),
        ("arch mismatch", vec![platform("linux", "arm64")], false),
        (
            "multiple platforms one matches",
            vec![platform("windows", "amd64"), platform("linux", "amd64")],
            true,
        ),
    ];
    for (name, required, want) in cases {
        assert_eq!(platform_matches(&node, &required), want, "{name}");
    }
}

/// Go: `TestPlatformMatchesArchNormalization`: the kernel-style names nodes report match the
/// manifest names platforms use.
#[test]
fn platform_matches_normalizes_architectures() {
    for (name, node_arch, required_arch, want) in [
        (
            "x86_64 node matches amd64 platform",
            "x86_64",
            "amd64",
            true,
        ),
        (
            "amd64 node matches x86_64 platform",
            "amd64",
            "x86_64",
            true,
        ),
        (
            "aarch64 node matches arm64 platform",
            "aarch64",
            "arm64",
            true,
        ),
        (
            "arm64 node matches aarch64 platform",
            "arm64",
            "aarch64",
            true,
        ),
        ("i686 node matches 386 platform", "i686", "386", true),
        ("armv7l node matches arm platform", "armv7l", "arm", true),
        ("x86_64 node does NOT match arm64", "x86_64", "arm64", false),
        (
            "aarch64 node does NOT match amd64",
            "aarch64",
            "amd64",
            false,
        ),
    ] {
        let mut node = schedulable_node("n", "n");
        node.description.platform = platform("linux", node_arch);
        assert_eq!(
            platform_matches(&node, &[platform("linux", required_arch)]),
            want,
            "{name}"
        );
    }
}

fn constrained_node() -> Node {
    Node {
        id: "abc123".to_owned(),
        description: NodeDescription {
            hostname: "pollux".to_owned(),
            platform: platform("linux", "x86_64"),
            engine: EngineDescription {
                labels: BTreeMap::from([("engine_env".to_owned(), "prod".to_owned())]),
            },
        },
        status: NodeStatus {
            state: "ready".to_owned(),
            addr: "10.0.0.5".to_owned(),
        },
        spec: NodeSpec {
            labels: BTreeMap::from([("zone".to_owned(), "us-east-1".to_owned())]),
            role: "manager".to_owned(),
            availability: "active".to_owned(),
        },
    }
}

/// Go: `TestConstraintsMatch`
#[test]
fn constraints_match_with_swarmkit_semantics() {
    let node = constrained_node();
    let cases: &[(&str, &[&str], bool)] = &[
        ("empty constraints", &[], true),
        ("role == manager", &["node.role == manager"], true),
        ("role == worker", &["node.role == worker"], false),
        ("role != worker", &["node.role != worker"], true),
        ("role != manager", &["node.role != manager"], false),
        ("hostname match", &["node.hostname == pollux"], true),
        ("hostname mismatch", &["node.hostname == castor"], false),
        ("node.id match", &["node.id == abc123"], true),
        ("platform.os", &["node.platform.os == linux"], true),
        // Swarm compares the raw arch the node reports, unlike platform_matches.
        ("platform.arch raw", &["node.platform.arch == x86_64"], true),
        (
            "platform.arch not normalized",
            &["node.platform.arch == amd64"],
            false,
        ),
        ("node label", &["node.labels.zone == us-east-1"], true),
        (
            "node label mismatch",
            &["node.labels.zone == eu-west-1"],
            false,
        ),
        (
            "missing node label",
            &["node.labels.nonexistent == x"],
            false,
        ),
        ("engine label", &["engine.labels.engine_env == prod"], true),
        (
            "engine label mismatch",
            &["engine.labels.engine_env == dev"],
            false,
        ),
        (
            "multiple constraints all match",
            &["node.role == manager", "node.hostname == pollux"],
            true,
        ),
        (
            "multiple constraints one fails",
            &["node.role == manager", "node.hostname == castor"],
            false,
        ),
        ("malformed no operator", &["node.role"], false),
        ("unsupported operator >", &["node.labels.count > 5"], false),
        ("empty value", &["node.role =="], false),
        ("empty key", &["== manager"], false),
        (
            "case-insensitive role value",
            &["node.role == Manager"],
            true,
        ),
        (
            "case-insensitive label value",
            &["node.labels.zone == US-EAST-1"],
            true,
        ),
        ("case-insensitive key", &["Node.Role == manager"], true),
        (
            "case-insensitive label prefix",
            &["Node.Labels.zone == us-east-1"],
            true,
        ),
        ("missing node label !=", &["node.labels.gpu != true"], true),
        ("missing node label ==", &["node.labels.gpu == true"], false),
        (
            "missing engine label !=",
            &["engine.labels.gpu != true"],
            true,
        ),
        ("node.ip exact", &["node.ip == 10.0.0.5"], true),
        ("node.ip exact mismatch", &["node.ip == 10.0.0.6"], false),
        ("node.ip cidr", &["node.ip == 10.0.0.0/24"], true),
        ("node.ip != cidr", &["node.ip != 10.0.0.0/24"], false),
        (
            "node.ip != other cidr",
            &["node.ip != 192.168.0.0/16"],
            true,
        ),
        ("node.ip invalid value ==", &["node.ip == not-an-ip"], false),
        ("node.ip invalid value !=", &["node.ip != not-an-ip"], false),
        ("unknown key", &["node.unknown == x"], false),
        ("unknown key !=", &["node.unknown != x"], false),
        ("exact node.labels. prefix", &["node.labels. != x"], false),
        (
            "exact engine.labels. prefix",
            &["engine.labels. != x"],
            false,
        ),
    ];
    for (name, expressions, want) in cases {
        assert_eq!(
            constraints_match(&node, &constraints(expressions)),
            *want,
            "{name}"
        );
    }
}

/// Beyond the Go table: Go's Unicode folding and white space, which an ASCII-only port gets
/// wrong, and a key cut inside a character.
#[test]
fn constraints_fold_and_trim_as_go() {
    let mut node = constrained_node();
    node.description.hostname = "straße".to_owned();
    node.spec
        .labels
        .insert("café".to_owned(), "Ünïcode".to_owned());
    let cases: &[(&str, bool)] = &[
        // U+212A KELVIN SIGN folds to k, U+017F LONG S to s.
        ("node.hostname == STRAẞE", true),
        ("node.hoſtname == straße", true),
        ("node.labels.café == üNÏCODE", true),
        ("node.labels.CAFÉ == ünïcode", false),
        ("\u{a0}node.role\u{2003}==\u{3000}manager\u{85}", true),
        ("node.labelſ.zone == us-east-1", false),
        ("node.labels\u{e9}zone == us-east-1", false),
        ("node.role == manager != worker", false),
        ("node.role != worker == x", false),
    ];
    for (expression, want) in cases {
        assert_eq!(
            constraints_match(&node, &constraints(&[expression])),
            *want,
            "{expression:?}"
        );
    }
}

fn testdata(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests/testdata", name]
        .iter()
        .collect();
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn unhex(hex: &str) -> String {
    if hex == "-" {
        return String::new();
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

/// The `node.ip` constraint against Go's `net.ParseIP`, `ParseCIDR`, `Equal` and `Contains`, for
/// every text of the Go corpus and six node addresses, with both operators.
#[test]
fn node_ip_constraints_match_go() {
    let mut checked = 0;
    for line in testdata("net/go-ip.tsv").lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields[0] != "match" {
            continue;
        }
        let (expected, addr) = (unhex(fields[1]), unhex(fields[2]));
        for (operator, want) in [("==", fields[3]), ("!=", fields[4])] {
            assert_eq!(
                match_node_ip_constraint(operator, &expected, &addr).to_string(),
                want,
                "node.ip {operator} {expected:?} on {addr:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 672);
}

/// Go: `TestCountEligibleNodes`
#[test]
fn count_eligible_nodes_applies_the_placement() {
    let mut nodes = vec![
        schedulable_node("n1", "host1"),
        schedulable_node("n2", "host2"),
        schedulable_node("n3", "host3"),
    ];
    nodes[2].spec.availability = "drain".to_owned();
    assert_eq!(
        count_eligible_nodes(&nodes, None),
        2,
        "no placement: n1 and n2"
    );
    let hostname = Placement {
        constraints: constraints(&["node.hostname == host1"]),
        ..Placement::default()
    };
    assert_eq!(count_eligible_nodes(&nodes, Some(&hostname)), 1);
    let windows = Placement {
        platforms: vec![platform("windows", "amd64")],
        ..Placement::default()
    };
    assert_eq!(
        count_eligible_nodes(&[schedulable_node("n1", "h1")], Some(&windows)),
        0
    );
    // Issue #35: a stack-deployed global service carries the image platform (amd64) and a !=
    // constraint on a label no node has; every x86_64 node still counts.
    let six: Vec<Node> = (0..6)
        .map(|index| {
            let mut node = schedulable_node(&format!("n{index}"), &format!("host{index}"));
            node.description.platform = platform("linux", "x86_64");
            node
        })
        .collect();
    let global = Placement {
        constraints: constraints(&["node.labels.x != y"]),
        platforms: vec![platform("linux", "amd64")],
    };
    assert_eq!(count_eligible_nodes(&six, Some(&global)), 6);
}

/// Go: `TestNodeEligible`
#[test]
fn node_eligible_combines_state_and_placement() {
    let ready = schedulable_node("n1", "host1");
    let mut drained = schedulable_node("n2", "host2");
    drained.spec.availability = "drain".to_owned();
    let constrained = |expression: &str| Placement {
        constraints: constraints(&[expression]),
        ..Placement::default()
    };
    let windows = Placement {
        platforms: vec![platform("windows", "")],
        ..Placement::default()
    };
    let cases = [
        ("nil placement, schedulable", &ready, None, true),
        ("nil placement, drained", &drained, None, false),
        (
            "constraint matches",
            &ready,
            Some(constrained("node.hostname == host1")),
            true,
        ),
        (
            "constraint does not match",
            &ready,
            Some(constrained("node.hostname == other")),
            false,
        ),
        ("platform does not match", &ready, Some(windows), false),
        (
            "constraint matches but drained",
            &drained,
            Some(constrained("node.hostname == host2")),
            false,
        ),
    ];
    for (name, node, placement, want) in cases {
        assert_eq!(
            node_eligible(Some(node), placement.as_ref()),
            want,
            "{name}"
        );
    }
    assert!(!node_eligible(None, None));
}

/// Go: `TestPlacementFromMetadata`
#[test]
fn placement_from_metadata_rebuilds_the_cached_placement() {
    assert_eq!(placement_from_metadata(&ServiceMetadata::default()), None);
    let metadata = ServiceMetadata {
        constraints: constraints(&["node.role == manager"]),
        platforms: vec![platform("linux", "")],
        ..ServiceMetadata::default()
    };
    let placement = placement_from_metadata(&metadata).unwrap();
    assert_eq!(placement.constraints.len(), 1);
    assert_eq!(placement.platforms.len(), 1);
}

fn metadata(mode: &str, configured: f64) -> ServiceMetadata {
    ServiceMetadata {
        stack: "shop".to_owned(),
        service: "web".to_owned(),
        service_mode: mode.to_owned(),
        configured_replicas: configured,
        ..ServiceMetadata::default()
    }
}

#[test]
fn replica_counts_per_mode() {
    let mut nodes = vec![
        schedulable_node("n1", "h1"),
        schedulable_node("n2", "h2"),
        schedulable_node("n3", "h3"),
    ];
    nodes[2].status.state = "down".to_owned();
    let counts = |mode, configured| replica_counts(&metadata(mode, configured), &nodes);
    assert_eq!(
        counts(SERVICE_MODE_REPLICATED, 5.0),
        (5.0, 2.0),
        "min(configured, eligible)"
    );
    assert_eq!(counts(SERVICE_MODE_REPLICATED, 1.0), (1.0, 1.0));
    assert_eq!(counts(SERVICE_MODE_REPLICATED_JOB, 4.0), (4.0, 4.0));
    assert_eq!(counts(SERVICE_MODE_GLOBAL, 0.0), (2.0, 2.0));
    assert_eq!(counts(SERVICE_MODE_GLOBAL_JOB, 0.0), (2.0, 2.0));
    let mut pinned = metadata(SERVICE_MODE_GLOBAL, 0.0);
    pinned.constraints = constraints(&["node.hostname == h1"]);
    assert_eq!(replica_counts(&pinned, &nodes), (1.0, 1.0));
}

#[test]
fn replica_gauges_carry_go_names_help_and_labels() {
    let custom = CustomLabels::new(
        &["com.example.team".to_owned()],
        &["com_example_team".to_owned()],
    );
    let gauges = ReplicaGauges::new(&custom);
    for (family, name, help) in [
        (
            gauges.desired(),
            "swarm_service_desired_replicas",
            DESIRED_REPLICAS_HELP,
        ),
        (
            gauges.schedulable(),
            "swarm_service_schedulable_replicas",
            SCHEDULABLE_REPLICAS_HELP,
        ),
    ] {
        let desc = &family.descs()[0];
        assert_eq!(desc.name(), name);
        assert_eq!(desc.help(), help);
        assert_eq!(
            desc.label_names(),
            [
                "stack",
                "service",
                "service_mode",
                "display_name",
                "com_example_team"
            ]
        );
    }
    assert_eq!(
        DESIRED_REPLICAS_HELP,
        "Number of desired replicas for a Swarm service (replicated: configured replicas; \
         replicated-job: total completions; global and global-job: eligible nodes)."
    );
    assert_eq!(
        SCHEDULABLE_REPLICAS_HELP,
        "Number of replicas that can currently be scheduled given node availability and \
         placement constraints (replicated: min(configured, eligible_nodes); global: eligible \
         nodes; jobs and restart-condition none services: 0)."
    );
    let registry = Registry::new();
    gauges.register(&registry).unwrap();
    assert!(gauges.register(&registry).is_err(), "registered once");
}

#[test]
fn schedulable_is_forced_to_zero_for_jobs_and_one_shots() {
    let gauges = ReplicaGauges::new(&CustomLabels::default());
    let labels = |metadata: &ServiceMetadata| labels_for_metadata(metadata);
    let replicated = metadata(SERVICE_MODE_REPLICATED, 3.0);
    gauges.set_desired(&replicated, 3.0).unwrap();
    gauges.set_schedulable(&replicated, 2.0).unwrap();
    assert_eq!(gauges.desired().get_with(&labels(&replicated)), Some(3.0));
    assert_eq!(
        gauges.schedulable().get_with(&labels(&replicated)),
        Some(2.0)
    );
    let mut one_shot = metadata(SERVICE_MODE_REPLICATED, 1.0);
    one_shot.service = "cron".to_owned();
    one_shot.restart_condition_none = true;
    gauges.set_schedulable(&one_shot, 1.0).unwrap();
    assert_eq!(gauges.schedulable().get_with(&labels(&one_shot)), Some(0.0));
    for mode in [SERVICE_MODE_REPLICATED_JOB, SERVICE_MODE_GLOBAL_JOB] {
        let job = metadata(mode, 4.0);
        gauges.set_schedulable(&job, 4.0).unwrap();
        assert_eq!(
            gauges.schedulable().get_with(&labels(&job)),
            Some(0.0),
            "{mode}"
        );
    }
}

#[test]
fn labels_for_metadata_names_every_label() {
    let mut service = metadata(SERVICE_MODE_GLOBAL, 0.0);
    service
        .custom_labels
        .insert("team".to_owned(), "platform".to_owned());
    assert_eq!(
        labels_for_metadata(&service),
        BTreeMap::from([
            ("display_name".to_owned(), "shop web".to_owned()),
            ("service".to_owned(), "web".to_owned()),
            ("service_mode".to_owned(), "global".to_owned()),
            ("stack".to_owned(), "shop".to_owned()),
            ("team".to_owned(), "platform".to_owned()),
        ])
    );
    // Custom labels the family does not carry are refused, as client_golang's With refuses
    // them.
    let gauges = ReplicaGauges::new(&CustomLabels::default());
    let error = gauges.set_desired(&service, 1.0).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("expected 4 label values but got 5"),
        "{error}"
    );
}
