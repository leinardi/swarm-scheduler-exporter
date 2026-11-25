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
//! The crates whose types cross rust-common's public API are one package each in this workspace's
//! graph, and the ones rust-common re-exports are taken through the re-export, not declared here:
//! prometheus-client and tokio-util as `service_kit::prometheus_client` and
//! `service_kit::tokio_util`, bollard as `testenv_core::bollard`. Two copies compile apart and fail
//! at the boundary with E0277 or E0308. One exception: the harness declares tokio-util for its `io`
//! feature, which service-kit's re-export does not enable; the check still holds it to
//! rust-common's one package. The check is `repo_meta::boundary`'s, on
//! `cargo metadata --locked --all-features` (CI runs `cargo fetch --locked` first); the negative
//! controls plant a split and a declaration into the real metadata.

use repo_meta::boundary::{Exception, TABLE, check, check_metadata, metadata};
use repo_meta::git_pins::RUST_COMMON_URL;
use repo_meta::yaml::Value;
use repo_meta::{report, strings};

const DAEMON: &str = "swarm-scheduler-exporter";

/// The harness's tokio-util, for `io` (the workload image archive's byte stream).
const EXCEPTIONS: &[Exception] = &[Exception {
    member: "testenv",
    package: "tokio-util",
    reason: "the harness needs tokio-util's io feature, which service-kit's re-export lacks",
}];

fn real_metadata() -> Value {
    metadata(meta::repo().root()).unwrap_or_else(|problem| panic!("{problem}"))
}

fn array<'value>(value: &'value mut Value, key: &str) -> &'value mut Vec<Value> {
    match value.get_mut(key) {
        Some(Value::Array(items)) => items,
        _ => panic!("cargo metadata has no {key} array"),
    }
}

/// The one package named `name`, cloned.
fn package(metadata: &Value, name: &str) -> Value {
    let found: Vec<&Value> = metadata["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("cargo metadata has no packages array"))
        .iter()
        .filter(|package| package["name"] == name)
        .collect();
    assert_eq!(found.len(), 1, "want one {name} package");
    found[0].clone()
}

fn node_mut<'value>(metadata: &'value mut Value, id: &Value) -> &'value mut Value {
    array(array_parent(metadata), "nodes")
        .iter_mut()
        .find(|node| node["id"] == *id)
        .unwrap_or_else(|| panic!("no resolve node {id}"))
}

fn array_parent(metadata: &mut Value) -> &mut Value {
    metadata
        .get_mut("resolve")
        .unwrap_or_else(|| panic!("cargo metadata has no resolve graph"))
}

#[test]
fn every_boundary_crate_is_one_package_and_none_is_declared_beside_its_reexport() {
    let problems = check(meta::repo().root(), RUST_COMMON_URL, EXCEPTIONS);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The exporter's own prometheus-client 0.25 beside service-kit's: named, with both copies and
/// the re-export to take it through.
#[test]
fn a_prometheus_client_split_is_named() {
    let mut metadata = real_metadata();
    let ours = package(&metadata, "prometheus-client");
    let mut theirs = ours.clone();
    let id = Value::String(
        "registry+https://github.com/rust-lang/crates.io-index#prometheus-client@0.25.2".into(),
    );
    theirs["id"] = id.clone();
    theirs["version"] = Value::String("0.25.2".into());
    array(&mut metadata, "packages").push(theirs);
    let mut node = node_mut(&mut metadata, &ours["id"]).clone();
    node["id"] = id.clone();
    array(array_parent(&mut metadata), "nodes").push(node);
    let daemon = package(&metadata, DAEMON);
    let daemon_node = node_mut(&mut metadata, &daemon["id"]);
    let mut edge = array(daemon_node, "deps")[0].clone();
    edge["name"] = Value::String("prometheus_client".into());
    edge["pkg"] = id;
    array(daemon_node, "deps").push(edge);

    let problems = strings(&check_metadata(
        &metadata,
        TABLE,
        RUST_COMMON_URL,
        EXCEPTIONS,
    ));
    assert_eq!(problems.len(), 1, "{problems:#?}");
    let problem = &problems[0];
    assert!(
        problem.starts_with(&format!(
            "2 copies of prometheus-client: 0.25.2 (crates.io) via {DAEMON} -> prometheus-client; \
             {} (crates.io), rust-common's, via {DAEMON} -> service-kit -> prometheus-client;",
            ours["version"].as_str().expect("version")
        )),
        "{problem}"
    );
    assert!(
        problem.ends_with("(E0277/E0308); take it through service_kit::prometheus_client"),
        "{problem}"
    );
}

/// prometheus-client declared by the exporter, at the very package rust-common uses: refused all the
/// same, as a direct declaration of a re-exported crate.
#[test]
fn a_direct_prometheus_client_declaration_is_refused() {
    let mut metadata = real_metadata();
    let packages = array(&mut metadata, "packages");
    let daemon = packages
        .iter_mut()
        .find(|package| package["name"] == DAEMON)
        .expect("the daemon package");
    let declared = array(daemon, "dependencies");
    let mut dependency = declared[0].clone();
    dependency["name"] = Value::String("prometheus-client".into());
    dependency["rename"] = Value::Null;
    dependency["kind"] = Value::Null;
    dependency["target"] = Value::Null;
    declared.push(dependency);

    assert_eq!(
        strings(&check_metadata(
            &metadata,
            TABLE,
            RUST_COMMON_URL,
            EXCEPTIONS
        )),
        [format!(
            "{DAEMON} declares prometheus-client directly (normal dependency); take it through \
             service_kit::prometheus_client, or pass an Exception with the reason"
        )]
    );
}

/// The harness's tokio-util passes only as the exception: without it, the declaration is refused.
#[test]
fn the_harness_tokio_util_needs_its_exception() {
    let metadata = real_metadata();
    assert_eq!(
        strings(&check_metadata(&metadata, TABLE, RUST_COMMON_URL, &[])),
        [
            "testenv declares tokio-util directly (normal dependency); take it through \
          service_kit::tokio_util, or pass an Exception with the reason"
        ]
    );
}
