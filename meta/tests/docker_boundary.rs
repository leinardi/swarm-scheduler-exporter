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

//! The Docker SDK boundary, which keeps the exporter read-only against Docker in code (a `:ro`
//! socket bind does not make the API read-only). It replaces Go's `docker-sdk-boundary` depguard
//! rule for the Rust workspace:
//!
//! - bollard, the SDK the test harness creates and removes things with, is declared directly only
//!   by `testenv`, only in its `[dependencies]` and only as `optional = true` (behind the
//!   `integration` feature); the exporter reaches it only through `docker-client`;
//! - the exporter package names neither bollard nor testenv nor testenv-core (whose `bollard`
//!   feature brings bollard) in any dependency table, so none can reach the shipped binary;
//! - nothing under `src/` names testenv, and bollard is named only in `src/docker`, as
//!   `docker_client::bollard`;
//! - the exporter's Docker client is rust-common's `docker-client`, over bollard, as long as its
//!   `attach` feature (the container attach and wait calls) is not enabled: no workspace manifest
//!   enables it, and the exporter package takes `tcp` and `tls` only (`test-util` too, in its
//!   dev-dependencies);
//! - the exporter reaches the client through `src/docker` alone, builds no request of its own
//!   there, calls no bollard write, and makes only the read calls (and the version negotiation)
//!   whose API sections the shipped socket-proxy allowlist grants.
//!
//! Each rule is a function over the files' text, so each has a negative control. They are this
//! repository's own: repo-meta's `docker_drivers`, which `integration_targets.rs` runs, sees a
//! client only where code imports or calls it, and these rules see the name anywhere on a line, a
//! comment and `extern crate` included.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use repo_meta::workspace::{member_manifest, members};
use toml::{Table, Value};

const SDK: &str = "bollard";
const HARNESS: &str = "testenv";
/// The harness's shared half (rust-common), whose `bollard` feature brings the SDK.
const HARNESS_CORE: &str = "testenv-core";
const HARNESS_MANIFEST: &str = "testenv/Cargo.toml";
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];
/// The sanctioned Docker client, and the features that let it send requests of any method.
const CLIENT: &str = "docker-client";
const WRITE_FEATURES: &[&str] = &["attach"];
/// What the exporter package may enable: in its `[dependencies]`, and in its
/// `[dev-dependencies]`.
const CLIENT_FEATURES: &[&str] = &["tcp", "tls"];
const CLIENT_TEST_FEATURES: &[&str] = &["tcp", "tls", "test-util"];

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// One dependency entry: the table it sits in (`dependencies`, `target.'cfg(unix)'.dependencies`,
/// `workspace.dependencies`), the crate it names (its `package` if renamed), and the entry.
struct Dependency {
    table: String,
    crate_name: String,
    entry: Value,
}

/// Every dependency entry of a manifest, target-specific and workspace tables included.
fn dependencies(manifest: &Table) -> Vec<Dependency> {
    let mut tables: Vec<(String, &Table)> = Vec::new();
    for name in DEPENDENCY_TABLES {
        if let Some(table) = manifest.get(*name).and_then(Value::as_table) {
            tables.push(((*name).to_owned(), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (target, contents) in targets {
            for name in DEPENDENCY_TABLES {
                if let Some(table) = contents.get(*name).and_then(Value::as_table) {
                    tables.push((format!("target.{target}.{name}"), table));
                }
            }
        }
    }
    if let Some(table) = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table)
    {
        tables.push(("workspace.dependencies".to_owned(), table));
    }
    tables
        .into_iter()
        .flat_map(|(table, entries)| {
            entries.iter().map(move |(key, entry)| Dependency {
                table: table.clone(),
                crate_name: entry
                    .get("package")
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .to_owned(),
                entry: entry.clone(),
            })
        })
        .collect()
}

/// The manifest rules over `(path, text)` pairs, the root manifest at `Cargo.toml`: one message
/// per violation.
fn manifest_violations(manifests: &[(&str, &str)]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut confined = 0;
    for (path, text) in manifests {
        let manifest: Table = text
            .parse()
            .unwrap_or_else(|error| panic!("parse {path}: {error}"));
        for dependency in dependencies(&manifest) {
            let place = format!("{path} [{}] {}", dependency.table, dependency.crate_name);
            if *path == "Cargo.toml"
                && dependency.table != "workspace.dependencies"
                && [SDK, HARNESS, HARNESS_CORE].contains(&dependency.crate_name.as_str())
            {
                violations.push(format!(
                    "{place}: the exporter package must not depend on {}",
                    dependency.crate_name
                ));
                continue;
            }
            if dependency.crate_name != SDK {
                continue;
            }
            let optional = dependency
                .entry
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if *path != HARNESS_MANIFEST || dependency.table != "dependencies" {
                violations.push(format!(
                    "{place}: {SDK} is declared only in {HARNESS_MANIFEST} [dependencies]"
                ));
            } else if !optional {
                violations.push(format!(
                    "{place}: {SDK} must be optional = true, behind the integration feature"
                ));
            } else {
                confined += 1;
            }
        }
    }
    if confined != 1 {
        violations.push(format!(
            "{HARNESS_MANIFEST} declares {SDK} {confined} times as an optional dependency, want once"
        ));
    }
    violations
}

/// The client's manifest rules over `(path, text)` pairs, the root manifest at `Cargo.toml`: no
/// manifest enables a write feature of [`CLIENT`] (features unify across a workspace build), and
/// the exporter package declares it once in `[dependencies]` with [`CLIENT_FEATURES`] at most,
/// and in its dev-dependencies with [`CLIENT_TEST_FEATURES`] at most.
fn client_manifest_violations(manifests: &[(&str, &str)]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut declared = 0;
    for (path, text) in manifests {
        let manifest: Table = text
            .parse()
            .unwrap_or_else(|error| panic!("parse {path}: {error}"));
        for dependency in dependencies(&manifest) {
            if dependency.crate_name != CLIENT {
                continue;
            }
            let place = format!("{path} [{}] {CLIENT}", dependency.table);
            let features: Vec<&str> = dependency
                .entry
                .get("features")
                .and_then(Value::as_array)
                .map(|features| features.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for feature in &features {
                if WRITE_FEATURES.contains(feature) {
                    violations.push(format!(
                        "{place}: `{feature}` lets the client attach to and wait for containers"
                    ));
                }
            }
            if *path != "Cargo.toml" || dependency.table == "workspace.dependencies" {
                continue;
            }
            let allowed = match dependency.table.as_str() {
                "dependencies" => {
                    declared += 1;
                    CLIENT_FEATURES
                }
                "dev-dependencies" => CLIENT_TEST_FEATURES,
                table => {
                    violations.push(format!(
                        "{place}: the exporter declares {CLIENT} in [dependencies] and \
                         [dev-dependencies] only, not in [{table}]"
                    ));
                    continue;
                }
            };
            for feature in features {
                if !allowed.contains(&feature) && !WRITE_FEATURES.contains(&feature) {
                    violations.push(format!(
                        "{place}: feature `{feature}` is not one of {allowed:?}"
                    ));
                }
            }
        }
    }
    if declared != 1 {
        violations.push(format!(
            "Cargo.toml declares {CLIENT} {declared} times in [dependencies], want once"
        ));
    }
    violations
}

static SDK_OR_HARNESS: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(bollard|testenv(_core)?)\b"));

/// Files under `src/` that name the harness, or the SDK other than as `docker_client::bollard` in
/// `src/docker` (a comment there may name it), in code or in a comment.
fn source_violations(sources: &[(&str, &str)]) -> Vec<String> {
    sources
        .iter()
        .flat_map(|(path, text)| {
            let in_docker = path.starts_with("src/docker/");
            text.lines().enumerate().filter_map(move |(index, line)| {
                SDK_OR_HARNESS.find_iter(line).find_map(|found| {
                    let through_client = found.as_str() == SDK
                        && in_docker
                        && (line.contains("docker_client::bollard")
                            || line.trim_start().starts_with("//"));
                    (!through_client)
                        .then(|| format!("{path}:{}: names {}", index + 1, found.as_str()))
                })
            })
        })
        .collect()
}

static CLIENT_NAMED: LazyLock<Regex> = LazyLock::new(|| regex(r"\bdocker_client\b"));
/// The bollard calls that read: `list_*` and `inspect_*` of the objects the exporter lists.
static READ_CALL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\.\s*((?:list|inspect)_[a-z_]+)\s*\("));
/// The bollard calls that change, run, or leak content of a container, service or task.
static WRITE_CALL: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\.\s*((?:create|start|stop|restart|kill|remove|delete|update|pause|unpause|rename|wait|attach|exec|prune|build|commit|import|export|resize|upload|download|join|leave|init|unlock|push|pull|tag|connect|disconnect|prepare|top|logs|stats|changes)_[a-z_]+)\s*\(",
    )
});
/// The event stream and the version negotiation, the two calls that are not `list_*` or
/// `inspect_*`.
static EVENTS_CALL: LazyLock<Regex> = LazyLock::new(|| regex(r"\.\s*events\s*\("));
static NEGOTIATE_CALL: LazyLock<Regex> = LazyLock::new(|| regex(r"\bnegotiate(?:_version)?\s*\("));
static METHOD: LazyLock<Regex> = LazyLock::new(|| regex(r"\bMethod::[A-Za-z_]+"));
static REQUEST_BUILT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(Request::new|Request::builder)\s*\("));

/// The API section (the first path segment, as the allowlist names it) of each read call the
/// exporter may make.
const READ_SEGMENTS: &[(&str, &str)] = &[
    ("list_services", "services"),
    ("inspect_service", "services"),
    ("list_nodes", "nodes"),
    ("list_tasks", "tasks"),
    ("list_containers", "containers"),
    ("inspect_container", "containers"),
];

/// The first path segments the socket-proxy allowlist grants: each `KEY=1` of `env`, lowercased.
fn granted_segments(env: &str) -> BTreeSet<String> {
    env.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(_, value)| value.trim() == "1")
        .map(|(key, _)| key.trim().to_ascii_lowercase())
        .collect()
}

/// The client rules over the non-test sources of `src/` (paths as `src/…`), against the
/// allowlist in `env`: only `src/docker` names the client crate, nothing calls a write, and in
/// `src/docker` no request is built and no method named, and every call made is a known read
/// whose section is on the allowlist, the version negotiation's and the event stream's included.
fn client_violations(sources: &[(&str, &str)], env: &str) -> Vec<String> {
    let granted = granted_segments(env);
    let mut violations = Vec::new();
    let mut calls = 0;
    for (path, text) in sources {
        let in_docker = path.starts_with("src/docker/");
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            if !in_docker && CLIENT_NAMED.is_match(line) {
                violations.push(format!(
                    "{path}:{line_number}: names docker_client; the client is reached through \
                     src/docker"
                ));
            }
            if !in_docker {
                continue;
            }
            if let Some(found) = WRITE_CALL.captures(line) {
                violations.push(format!("{path}:{line_number}: {}( is a write", &found[1]));
            }
            for found in METHOD.find_iter(line).chain(REQUEST_BUILT.find_iter(line)) {
                violations.push(format!(
                    "{path}:{line_number}: {}; the client builds every request",
                    found.as_str()
                ));
            }
        }
        if !in_docker {
            continue;
        }
        // Over the whole text: a call's name may sit on the line after its receiver.
        let mut sections: Vec<(String, String)> = Vec::new();
        for captures in READ_CALL.captures_iter(text) {
            let call = captures[1].to_owned();
            match READ_SEGMENTS.iter().find(|(name, _)| *name == call) {
                Some((_, segment)) => sections.push((call, (*segment).to_owned())),
                None => {
                    violations.push(format!("{path}: {call}( is not a read the exporter makes"));
                }
            }
        }
        sections.extend(
            EVENTS_CALL
                .find_iter(text)
                .map(|_| ("events".to_owned(), "events".to_owned())),
        );
        sections.extend(
            NEGOTIATE_CALL
                .find_iter(text)
                .map(|_| ("negotiate".to_owned(), "version".to_owned())),
        );
        for (call, segment) in sections {
            calls += 1;
            if !granted.contains(&segment) {
                violations.push(format!(
                    "{path}: {call}( needs /{segment}, which is not on the socket-proxy allowlist"
                ));
            }
        }
    }
    if calls == 0 {
        violations.push("no client call found: the call patterns no longer match".to_owned());
    }
    violations
}

/// The workspace manifests: the root, then each member's.
fn workspace_manifests() -> Vec<(String, String)> {
    let repo = meta::repo();
    let root = repo.read("Cargo.toml");
    let parsed: Table = root
        .parse()
        .unwrap_or_else(|error| panic!("parse Cargo.toml: {error}"));
    let members: Vec<String> = members(&parsed)
        .unwrap_or_else(|problem| panic!("{problem}"))
        .iter()
        .filter(|member| *member != ".")
        .map(|member| member_manifest(member))
        .collect();
    let mut manifests = vec![("Cargo.toml".to_owned(), root)];
    manifests.extend(repo.read_all(&members));
    manifests
}

fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect()
}

/// The non-test sources of the exporter.
fn client_sources() -> Vec<(String, String)> {
    let repo = meta::repo();
    let paths: Vec<String> = repo
        .files_under("src", &[".rs"])
        .into_iter()
        .filter(|path| !path.ends_with("/tests.rs") && !path.contains("/tests/"))
        .collect();
    repo.read_all(&paths)
}

#[test]
fn bollard_is_confined_to_the_harness() {
    let manifests = workspace_manifests();
    let violations = manifest_violations(&borrowed(&manifests));
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn the_docker_client_is_docker_client_without_attach() {
    let manifests = workspace_manifests();
    let violations = client_manifest_violations(&borrowed(&manifests));
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn src_names_neither_the_sdk_nor_the_harness() {
    let repo = meta::repo();
    let sources = repo.read_all(&repo.files_under("src", &[".rs"]));
    assert!(!sources.is_empty(), "no source under src");
    let violations = source_violations(&borrowed(&sources));
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn the_docker_client_only_reads_allowlisted_paths() {
    let sources = client_sources();
    let violations = client_violations(
        &borrowed(&sources),
        &meta::repo().read("deployments/docker/socket-proxy.env"),
    );
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

/// A negative control: its name, the manifests, and the violation it must raise.
type ManifestCase = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

const ROOT_OK: &str =
    "[package]\nname = \"swarm-scheduler-exporter\"\n[dependencies]\njiff = \"0.2\"\n";

const HARNESS_OK: &str = r#"
[package]
name = "testenv"
[dependencies]
bollard = { version = "=0.21.1", optional = true }
"#;

/// Negative controls for the manifest rules: each manifest below breaks one, and the clean set
/// passes.
#[test]
fn manifest_rules_reject_each_escape() {
    let clean = manifest_violations(&[("Cargo.toml", ROOT_OK), (HARNESS_MANIFEST, HARNESS_OK)]);
    assert!(clean.is_empty(), "{clean:?}");
    let cases: &[ManifestCase] = &[
        (
            "bollard in the exporter's dependencies",
            &[
                ("Cargo.toml", "[dependencies]\nbollard = \"0.21\"\n"),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on bollard",
        ),
        (
            "testenv as an exporter dev-dependency",
            &[
                (
                    "Cargo.toml",
                    "[dev-dependencies]\ntestenv = { path = \"testenv\" }\n",
                ),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on testenv",
        ),
        (
            "testenv as an exporter build-dependency for one target",
            &[
                (
                    "Cargo.toml",
                    "[target.'cfg(unix)'.build-dependencies]\ntestenv = { path = \"testenv\" }\n",
                ),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on testenv",
        ),
        (
            "bollard renamed in the exporter's dependencies",
            &[
                (
                    "Cargo.toml",
                    "[dependencies]\ndocker = { package = \"bollard\", version = \"0.21\" }\n",
                ),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on bollard",
        ),
        (
            "bollard in the meta crate",
            &[
                ("Cargo.toml", ROOT_OK),
                (HARNESS_MANIFEST, HARNESS_OK),
                (
                    "meta/Cargo.toml",
                    "[dev-dependencies]\nbollard = \"0.21\"\n",
                ),
            ],
            "declared only in testenv/Cargo.toml",
        ),
        (
            "bollard as a harness dev-dependency",
            &[
                ("Cargo.toml", ROOT_OK),
                (HARNESS_MANIFEST, "[dev-dependencies]\nbollard = \"0.21\"\n"),
            ],
            "declared only in testenv/Cargo.toml",
        ),
        (
            "bollard not optional",
            &[
                ("Cargo.toml", ROOT_OK),
                (HARNESS_MANIFEST, "[dependencies]\nbollard = \"0.21\"\n"),
            ],
            "must be optional = true",
        ),
        (
            "bollard missing altogether",
            &[
                ("Cargo.toml", ROOT_OK),
                (HARNESS_MANIFEST, "[dependencies]\n"),
            ],
            "0 times",
        ),
    ];
    for (name, manifests, want) in cases {
        let violations = manifest_violations(manifests);
        assert!(
            violations.iter().any(|violation| violation.contains(want)),
            "{name}: want a violation containing {want:?}, got {violations:?}"
        );
    }
}

/// Negative controls for testenv-core, whose `bollard` feature would bring the SDK into the
/// exporter: the workspace entry and the harness's own use pass, the exporter's does not.
#[test]
fn manifest_rules_keep_testenv_core_out_of_the_exporter() {
    let root = format!("{ROOT_OK}[workspace.dependencies]\ntestenv-core = {{ git = \"x\" }}\n");
    let harness =
        format!("{HARNESS_OK}testenv-core = {{ workspace = true, features = [\"bollard\"] }}\n");
    let shared = manifest_violations(&[("Cargo.toml", &root), (HARNESS_MANIFEST, &harness)]);
    assert!(shared.is_empty(), "{shared:?}");

    let cases: &[ManifestCase] = &[
        (
            "testenv-core in the exporter's dependencies",
            &[
                (
                    "Cargo.toml",
                    "[dependencies]\ntestenv-core = { workspace = true, features = [\"bollard\"] }\n",
                ),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on testenv-core",
        ),
        (
            "testenv-core renamed in the exporter's dev-dependencies",
            &[
                (
                    "Cargo.toml",
                    "[dev-dependencies]\nharness = { package = \"testenv-core\", workspace = true }\n",
                ),
                (HARNESS_MANIFEST, HARNESS_OK),
            ],
            "must not depend on testenv-core",
        ),
    ];
    for (name, manifests, want) in cases {
        let violations = manifest_violations(manifests);
        assert!(
            violations.iter().any(|violation| violation.contains(want)),
            "{name}: want a violation containing {want:?}, got {violations:?}"
        );
    }
}

/// Negative control for the source rule.
#[test]
fn source_rule_rejects_a_reference() {
    assert!(source_violations(&[("src/lib.rs", "pub mod docker;\n")]).is_empty());
    for (path, line) in [
        ("src/lib.rs", "use bollard::Docker;"),
        ("src/lib.rs", "// see testenv::up"),
        ("src/lib.rs", "extern crate bollard;"),
        (
            "src/collector/nodes.rs",
            "use docker_client::bollard::Docker;",
        ),
        ("src/docker/api.rs", "use bollard::Docker;"),
        ("src/docker/api.rs", "extern crate bollard;"),
        (
            "src/docker/api.rs",
            "use docker_client::bollard::Docker; // testenv",
        ),
    ] {
        let violations = source_violations(&[(path, line)]);
        assert_eq!(violations.len(), 1, "{path} {line:?}: {violations:?}");
    }
    // bollard through the client, in the client module, and a comment there, are the way in.
    for line in [
        "use docker_client::bollard::Docker;",
        "use docker_client::bollard::query_parameters::EventsOptionsBuilder;",
        "//! made over bollard",
    ] {
        let violations = source_violations(&[("src/docker/api.rs", line)]);
        assert!(violations.is_empty(), "{line:?}: {violations:?}");
    }
    // A word that merely contains one is not a reference.
    assert!(source_violations(&[("src/lib.rs", "let testenvironment = 1;")]).is_empty());
}

/// The clean exporter: the client in `src/docker` with the read calls, `run` through the module.
const SOURCES_OK: &[(&str, &str)] = &[
    (
        "src/docker/mod.rs",
        "pub use docker_client::{Config, ConnectError};\n",
    ),
    (
        "src/docker/api.rs",
        r"
async fn service_list(&self) { self.docker.list_services(None).await }
async fn container_inspect(&self) { self.docker.inspect_container(id, None).await }
async fn events(&self) { self.docker.events(Some(options)) }
async fn version(&self) { negotiate(docker, &self.config).await }
",
    ),
    ("src/run.rs", "use crate::docker::{self, Client};\n"),
];

const ENV: &str =
    "# the allowlist\nVERSION=1\nSERVICES=1\nCONTAINERS=1\nEVENTS=1\nPOST=0\nNETWORKS=0\n";

/// Negative controls for the client source rules: each edit below breaks one, and the clean
/// exporter passes.
#[test]
fn client_rules_reject_each_escape() {
    let clean = client_violations(SOURCES_OK, ENV);
    assert!(clean.is_empty(), "{clean:?}");

    let cases: [(&str, (&str, &str), &str); 8] = [
        (
            "a method named in the client module",
            (
                "src/docker/api.rs",
                "fn update(&self) { *request.method_mut() = Method::POST; }",
            ),
            "Method::POST; the client builds every request",
        ),
        (
            "a request built in the client module",
            (
                "src/docker/api.rs",
                "fn raw(&self) { let request = Request::builder().body(()); }",
            ),
            "Request::builder(; the client builds every request",
        ),
        (
            "a write call",
            (
                "src/docker/api.rs",
                "async fn kill(&self) { self.docker.kill_container(id, None).await }",
            ),
            "kill_container( is a write",
        ),
        (
            "a log read, which leaks what the containers print",
            (
                "src/docker/api.rs",
                "fn logs(&self) { self.docker.logs_container(id, None) }",
            ),
            "logs_container( is a write",
        ),
        (
            "the client named outside src/docker",
            ("src/collector/nodes.rs", "use docker_client::Config;"),
            "src/collector/nodes.rs:1: names docker_client",
        ),
        (
            "a read the allowlist denies",
            (
                "src/docker/api.rs",
                "async fn networks(&self) { self.docker.list_networks(None).await }",
            ),
            "list_networks( is not a read the exporter makes",
        ),
        (
            "an inspect of an object the exporter does not read",
            (
                "src/docker/api.rs",
                "async fn secret(&self) { self.docker.inspect_secret(id).await }",
            ),
            "inspect_secret( is not a read the exporter makes",
        ),
        (
            "a read whose section is off",
            (
                "src/docker/api.rs",
                "async fn tasks(&self) { self.docker.list_tasks(None).await }",
            ),
            "list_tasks( needs /tasks, which is not on the socket-proxy allowlist",
        ),
    ];
    for (name, extra, want) in cases {
        let mut sources = SOURCES_OK.to_vec();
        sources.push(extra);
        let violations = client_violations(&sources, ENV);
        assert!(
            violations.iter().any(|violation| violation.contains(want)),
            "{name}: want a violation containing {want:?}, got {violations:?}"
        );
    }

    // The events stream needs its key, the version negotiation needs VERSION, and a call pattern
    // that matches nothing is a failure too.
    let violations = client_violations(SOURCES_OK, &ENV.replace("EVENTS=1", "EVENTS=0"));
    assert!(
        violations.iter().any(|violation| violation
            .contains("events( needs /events, which is not on the socket-proxy allowlist")),
        "{violations:?}"
    );
    let violations = client_violations(SOURCES_OK, &ENV.replace("VERSION=1", "VERSION=0"));
    assert!(
        violations.iter().any(|violation| violation
            .contains("negotiate( needs /version, which is not on the socket-proxy allowlist")),
        "{violations:?}"
    );
    let pathless = client_violations(&[("src/docker/api.rs", "fn ping() {}\n")], ENV);
    assert!(
        pathless
            .iter()
            .any(|violation| violation.contains("no client call")),
        "{pathless:?}"
    );
}

/// Negative controls for the client's manifest rules.
#[test]
fn client_manifest_rules_reject_each_escape() {
    const ROOT: &str = "[dependencies]\ndocker-client = { workspace = true, features = [\"tcp\", \"tls\"] }\n[dev-dependencies]\ndocker-client = { workspace = true, features = [\"tcp\", \"tls\", \"test-util\"] }\n";
    let clean = client_manifest_violations(&[("Cargo.toml", ROOT), (HARNESS_MANIFEST, HARNESS_OK)]);
    assert!(clean.is_empty(), "{clean:?}");

    let cases: &[ManifestCase] = &[
        (
            "attach in the exporter's dependencies",
            &[(
                "Cargo.toml",
                "[dependencies]\ndocker-client = { workspace = true, features = [\"tcp\", \"attach\"] }\n",
            )],
            "`attach` lets the client attach to and wait for containers",
        ),
        (
            "attach on the workspace entry",
            &[(
                "Cargo.toml",
                "[workspace.dependencies]\ndocker-client = { git = \"x\", features = [\"attach\"] }\n[dependencies]\ndocker-client.workspace = true\n",
            )],
            "[workspace.dependencies] docker-client: `attach`",
        ),
        (
            "attach in another member, which feature unification would hand the exporter",
            &[
                ("Cargo.toml", ROOT),
                (
                    HARNESS_MANIFEST,
                    "[dev-dependencies]\ndocker-client = { workspace = true, features = [\"attach\"] }\n",
                ),
            ],
            "testenv/Cargo.toml [dev-dependencies] docker-client: `attach`",
        ),
        (
            "test-util in the shipped binary",
            &[(
                "Cargo.toml",
                "[dependencies]\ndocker-client = { workspace = true, features = [\"test-util\"] }\n",
            )],
            "feature `test-util` is not one of",
        ),
        (
            "the client renamed for one target",
            &[
                ("Cargo.toml", ROOT),
                (
                    "Cargo.toml",
                    "[target.'cfg(unix)'.dependencies]\nengine = { package = \"docker-client\", version = \"0.1\" }\n",
                ),
            ],
            "not in [target.cfg(unix).dependencies]",
        ),
        (
            "no client",
            &[("Cargo.toml", "[dependencies]\njiff = \"0.2\"\n")],
            "declares docker-client 0 times",
        ),
    ];
    for (name, manifests, want) in cases {
        let violations = client_manifest_violations(manifests);
        assert!(
            violations.iter().any(|violation| violation.contains(want)),
            "{name}: want a violation containing {want:?}, got {violations:?}"
        );
    }
}
