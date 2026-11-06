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
//! - bollard, the SDK the test harness creates and removes things with, is declared only by
//!   `testenv`, only in its `[dependencies]` and only as `optional = true` (behind the
//!   `integration` feature);
//! - the exporter package names neither bollard nor testenv nor testenv-core (whose `bollard`
//!   feature brings bollard) in any dependency table, so none can reach the shipped binary;
//! - nothing under `src/` names either;
//! - the exporter's Docker client is rust-common's `docker-engine`, which sends `GET` only, plus
//!   one `HEAD` in its ping, as long as neither `write` nor `attach` (its requests of any method)
//!   is enabled: no workspace manifest enables them, and the exporter package takes `tcp` and
//!   `tls` only (`test-util` too, in its dev-dependencies);
//! - the exporter reaches the client through `src/docker` alone, builds no request of its own
//!   there, calls no write, and asks only for paths the shipped socket-proxy allowlist grants,
//!   `/_ping` included.
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
const CLIENT: &str = "docker-engine";
const WRITE_FEATURES: &[&str] = &["write", "attach"];
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
                        "{place}: `{feature}` lets the client send requests of any method"
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

/// Files under `src/` that name the SDK or the harness, in code or in a comment.
fn source_violations(sources: &[(&str, &str)]) -> Vec<String> {
    sources
        .iter()
        .flat_map(|(path, text)| {
            text.lines().enumerate().filter_map(move |(index, line)| {
                SDK_OR_HARNESS
                    .find(line)
                    .map(|found| format!("{path}:{}: names {}", index + 1, found.as_str()))
            })
        })
        .collect()
}

static METHOD: LazyLock<Regex> = LazyLock::new(|| regex(r"\bMethod::[A-Za-z_]+"));
static REQUEST_BUILT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(Request::new|Request::builder)\s*\("));
static WRITE_CALL: LazyLock<Regex> = LazyLock::new(|| regex(r"\bexchange(_upgrade)?\s*\("));
static ENGINE: LazyLock<Regex> = LazyLock::new(|| regex(r"\bdocker_engine\b"));
/// The request paths: the client's calls' path arguments.
static REQUEST_PATH: LazyLock<Regex> = LazyLock::new(|| {
    regex(r#"\b(?:get_list|get_object|get_body|get)\(\s*(?:&format!\(\s*)?"(/[^"]*)""#)
});
/// The client's `events` call, whose path (`/events`) is the crate's.
static EVENTS_CALL: LazyLock<Regex> = LazyLock::new(|| regex(r"\.events\s*\("));

/// The first path segments the socket-proxy allowlist grants: each `KEY=1` of `env`, lowercased,
/// with `PING` standing for `_ping`.
fn granted_segments(env: &str) -> BTreeSet<String> {
    env.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(_, value)| value.trim() == "1")
        .map(|(key, _)| match key.trim() {
            "PING" => "_ping".to_owned(),
            key => key.to_ascii_lowercase(),
        })
        .collect()
}

/// The client rules over the non-test sources of `src/` (paths as `src/…`), against the
/// allowlist in `env`: only `src/docker` names the client crate, nothing calls a write, and in
/// `src/docker` no request is built and no method named, and every path the client is asked for
/// is on the allowlist, as is the ping's.
fn client_violations(sources: &[(&str, &str)], env: &str) -> Vec<String> {
    let granted = granted_segments(env);
    let mut violations = Vec::new();
    let mut paths = 0;
    if !granted.contains("_ping") {
        violations.push(
            "/_ping, the client's version negotiation, is not on the socket-proxy allowlist"
                .to_owned(),
        );
    }
    for (path, text) in sources {
        let in_docker = path.starts_with("src/docker/");
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            if !in_docker && ENGINE.is_match(line) {
                violations.push(format!(
                    "{path}:{line_number}: names docker_engine; the client is reached through \
                     src/docker"
                ));
            }
            if let Some(found) = WRITE_CALL.find(line) {
                violations.push(format!(
                    "{path}:{line_number}: {} is a write",
                    found.as_str()
                ));
            }
            if !in_docker {
                continue;
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
        // Over the whole text: a call's path argument may sit on the line after it.
        let calls = REQUEST_PATH
            .captures_iter(text)
            .filter_map(|captures| captures.get(1).map(|found| found.as_str().to_owned()))
            .chain(EVENTS_CALL.find_iter(text).map(|_| "/events".to_owned()));
        for request_path in calls {
            paths += 1;
            let segment = request_path[1..]
                .split(['/', '?'])
                .next()
                .unwrap_or_default();
            if !granted.contains(segment) {
                violations.push(format!(
                    "{path}: {request_path} is not on the socket-proxy allowlist"
                ));
            }
        }
    }
    if paths == 0 {
        violations.push("no request path found: the path pattern no longer matches".to_owned());
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
fn the_docker_client_is_docker_engine_without_writes() {
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
    for line in [
        "use bollard::Docker;",
        "// see testenv::up",
        "extern crate bollard;",
    ] {
        let violations = source_violations(&[("src/lib.rs", line)]);
        assert_eq!(violations.len(), 1, "{line:?}: {violations:?}");
    }
    // A word that merely contains one is not a reference.
    assert!(source_violations(&[("src/lib.rs", "let testenvironment = 1;")]).is_empty());
}

/// The clean exporter: the client in `src/docker` with the read calls, `run` through the module.
const SOURCES_OK: &[(&str, &str)] = &[
    (
        "src/docker/mod.rs",
        "pub use docker_engine::{Settings, with_bare_timeout};\n",
    ),
    (
        "src/docker/api.rs",
        r#"
async fn service_list(&self) { self.inner.get_list("/services", "").await }
async fn container_inspect(&self) { self.inner.get_object(&format!("/containers/{id}/json")).await }
async fn events(&self) { self.inner.events(&query).await }
"#,
    ),
    ("src/run.rs", "use crate::docker::{self, Client};\n"),
];

const ENV: &str =
    "# the allowlist\nPING=1\nSERVICES=1\nCONTAINERS=1\nEVENTS=1\nPOST=0\nNETWORKS=0\n";

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
                "async fn kill(&self) { self.inner.exchange(\"/containers/x/kill\", \"\", |_| {}) }",
            ),
            "exchange( is a write",
        ),
        (
            "the client named outside src/docker",
            ("src/collector/nodes.rs", "use docker_engine::Client;"),
            "src/collector/nodes.rs:1: names docker_engine",
        ),
        (
            "a write outside src/docker",
            (
                "src/run.rs",
                "client.exchange_upgrade(\"/containers/x/attach\", \"\", |_| {})",
            ),
            "exchange_upgrade( is a write",
        ),
        (
            "a path the allowlist denies",
            (
                "src/docker/api.rs",
                "async fn network_list(&self) { self.inner.get_list(\"/networks\", \"\").await }",
            ),
            "/networks is not on the socket-proxy allowlist",
        ),
        (
            "a path whose key is 0",
            (
                "src/docker/api.rs",
                "async fn pull(&self) { self.inner.get(\"/post\", \"\", &[]).await }",
            ),
            "/post is not on the socket-proxy allowlist",
        ),
        (
            "an inspect the allowlist denies",
            (
                "src/docker/api.rs",
                "self.inner.get_object(&format!(\"/secrets/{id}\")).await",
            ),
            "/secrets/{id} is not on the socket-proxy allowlist",
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

    // The events stream needs its key, the ping needs PING, and a path pattern that matches
    // nothing is a failure too.
    let violations = client_violations(SOURCES_OK, &ENV.replace("EVENTS=1", "EVENTS=0"));
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains("/events is not on the socket-proxy allowlist")),
        "{violations:?}"
    );
    let violations = client_violations(SOURCES_OK, &ENV.replace("PING=1", "PING=0"));
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains("/_ping, the client's version negotiation")),
        "{violations:?}"
    );
    let pathless = client_violations(&[("src/docker/api.rs", "fn ping() {}\n")], ENV);
    assert!(
        pathless
            .iter()
            .any(|violation| violation.contains("no request path")),
        "{pathless:?}"
    );
}

/// Negative controls for the client's manifest rules.
#[test]
fn client_manifest_rules_reject_each_escape() {
    const ROOT: &str = "[dependencies]\ndocker-engine = { workspace = true, features = [\"tcp\", \"tls\"] }\n[dev-dependencies]\ndocker-engine = { workspace = true, features = [\"tcp\", \"tls\", \"test-util\"] }\n";
    let clean = client_manifest_violations(&[("Cargo.toml", ROOT), (HARNESS_MANIFEST, HARNESS_OK)]);
    assert!(clean.is_empty(), "{clean:?}");

    let cases: &[ManifestCase] = &[
        (
            "write in the exporter's dependencies",
            &[(
                "Cargo.toml",
                "[dependencies]\ndocker-engine = { workspace = true, features = [\"tcp\", \"write\"] }\n",
            )],
            "`write` lets the client send requests of any method",
        ),
        (
            "attach on the workspace entry",
            &[(
                "Cargo.toml",
                "[workspace.dependencies]\ndocker-engine = { git = \"x\", features = [\"attach\"] }\n[dependencies]\ndocker-engine.workspace = true\n",
            )],
            "[workspace.dependencies] docker-engine: `attach`",
        ),
        (
            "write in another member, which feature unification would hand the exporter",
            &[
                ("Cargo.toml", ROOT),
                (
                    HARNESS_MANIFEST,
                    "[dev-dependencies]\ndocker-engine = { workspace = true, features = [\"write\"] }\n",
                ),
            ],
            "testenv/Cargo.toml [dev-dependencies] docker-engine: `write`",
        ),
        (
            "test-util in the shipped binary",
            &[(
                "Cargo.toml",
                "[dependencies]\ndocker-engine = { workspace = true, features = [\"test-util\"] }\n",
            )],
            "feature `test-util` is not one of",
        ),
        (
            "the client renamed for one target",
            &[
                ("Cargo.toml", ROOT),
                (
                    "Cargo.toml",
                    "[target.'cfg(unix)'.dependencies]\nengine = { package = \"docker-engine\", version = \"0.1\" }\n",
                ),
            ],
            "not in [target.cfg(unix).dependencies]",
        ),
        (
            "no client",
            &[("Cargo.toml", "[dependencies]\njiff = \"0.2\"\n")],
            "declares docker-engine 0 times",
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
