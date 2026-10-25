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
//! - the exporter package names neither bollard nor testenv in any dependency table, so neither
//!   can reach the shipped binary;
//! - nothing under `src/` names either;
//! - the exporter's own Docker client (`src/docker`) sends `GET` only, plus one `HEAD` in its
//!   ping, builds requests in one place, and asks only for paths the shipped socket-proxy
//!   allowlist grants.
//!
//! Each rule is a function over the files' text, so each has a negative control.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use meta::repo;
use regex::Regex;
use toml::{Table, Value};

const SDK: &str = "bollard";
const HARNESS: &str = "testenv";
const HARNESS_MANIFEST: &str = "testenv/Cargo.toml";
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

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
                && (dependency.crate_name == SDK || dependency.crate_name == HARNESS)
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

static SDK_OR_HARNESS: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(bollard|testenv)\b"));

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

static METHOD: LazyLock<Regex> = LazyLock::new(|| regex(r"\bMethod::([A-Za-z_]+)"));
static FUNCTION: LazyLock<Regex> = LazyLock::new(|| regex(r"\bfn ([A-Za-z_][A-Za-z0-9_]*)"));
static REQUEST_BUILT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b(Request::new|Request::builder)\s*\("));
/// The request paths: the API calls' path arguments and the ping's path.
static REQUEST_PATH: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?:\b(?:get_list|get_object|get_body|get)\(\s*(?:&format!\(\s*)?|join_path\(&\[[^\]]*,\s*)"(/[^"]*)""#,
    )
});

/// The function a byte offset of `text` sits in: the last `fn` declared before it.
fn enclosing_function(text: &str, offset: usize) -> &str {
    FUNCTION
        .captures_iter(&text[..offset])
        .last()
        .and_then(|captures| captures.get(1))
        .map_or("", |name| name.as_str())
}

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

/// The client rules over the non-test sources of `src/docker`, against the allowlist in `env`.
fn client_violations(sources: &[(&str, &str)], env: &str) -> Vec<String> {
    let granted = granted_segments(env);
    let mut violations = Vec::new();
    let mut heads = 0;
    let mut builders = 0;
    let mut paths = 0;
    for (path, text) in sources {
        for captures in METHOD.captures_iter(text) {
            let (Some(whole), Some(method)) = (captures.get(0), captures.get(1)) else {
                continue;
            };
            let function = enclosing_function(text, whole.start());
            match method.as_str() {
                "GET" => {}
                "HEAD" if function == "ping" => heads += 1,
                "HEAD" => {
                    violations.push(format!("{path}: HEAD outside the ping, in fn {function}"));
                }
                other => violations.push(format!("{path}: Method::{other} in fn {function}")),
            }
        }
        for found in REQUEST_BUILT.find_iter(text) {
            let function = enclosing_function(text, found.start());
            if function == "request" {
                builders += 1;
            } else {
                violations.push(format!(
                    "{path}: {} in fn {function}; requests are built in fn request only",
                    found.as_str()
                ));
            }
        }
        for captures in REQUEST_PATH.captures_iter(text) {
            let Some(request_path) = captures.get(1) else {
                continue;
            };
            paths += 1;
            let segment = request_path.as_str()[1..]
                .split(['/', '?'])
                .next()
                .unwrap_or_default();
            if !granted.contains(segment) {
                violations.push(format!(
                    "{path}: {} is not on the socket-proxy allowlist",
                    request_path.as_str()
                ));
            }
        }
    }
    if heads != 1 {
        violations.push(format!(
            "{heads} HEAD requests in fn ping, want exactly one"
        ));
    }
    if builders != 1 {
        violations.push(format!(
            "{builders} request builders in fn request, want exactly one"
        ));
    }
    if paths == 0 {
        violations.push("no request path found: the path pattern no longer matches".to_owned());
    }
    violations
}

/// The workspace manifests: the root, then each member's.
fn workspace_manifests() -> Vec<(String, String)> {
    let root = repo::read("Cargo.toml");
    let parsed: Table = root
        .parse()
        .unwrap_or_else(|error| panic!("parse Cargo.toml: {error}"));
    let members: Vec<String> = parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the root Cargo.toml lists no [workspace] members"))
        .iter()
        .filter_map(Value::as_str)
        .filter(|member| *member != ".")
        .map(|member| format!("{member}/Cargo.toml"))
        .collect();
    let mut manifests = vec![("Cargo.toml".to_owned(), root)];
    for member in members {
        let text = repo::read(&member);
        manifests.push((member, text));
    }
    manifests
}

fn read_all(paths: &[String]) -> Vec<(String, String)> {
    paths
        .iter()
        .map(|path| (path.clone(), repo::read(path)))
        .collect()
}

fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect()
}

/// The non-test sources of the exporter's Docker client.
fn client_sources() -> Vec<(String, String)> {
    let paths: Vec<String> = repo::files_under("src/docker", &[".rs"])
        .into_iter()
        .filter(|path| !path.ends_with("/tests.rs") && !path.contains("/tests/"))
        .collect();
    read_all(&paths)
}

#[test]
fn bollard_is_confined_to_the_harness() {
    let manifests = workspace_manifests();
    let violations = manifest_violations(&borrowed(&manifests));
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn src_names_neither_the_sdk_nor_the_harness() {
    let sources = read_all(&repo::files_under("src", &[".rs"]));
    assert!(!sources.is_empty(), "no source under src");
    let violations = source_violations(&borrowed(&sources));
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn the_docker_client_only_reads_allowlisted_paths() {
    let sources = client_sources();
    let violations = client_violations(
        &borrowed(&sources),
        &repo::read("deployments/docker/socket-proxy.env"),
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

const CLIENT_OK: &str = r#"
fn request(&self) -> Request<Empty<Bytes>> {
    let mut request = Request::new(Empty::new());
    *request.method_mut() = Method::GET;
    request
}
async fn ping(&self) {
    let target = escape_path(&join_path(&[&self.base, "/_ping"]));
    let head = || { *request.method_mut() = Method::HEAD; };
}
async fn service_list(&self) { self.get_list("/services", "").await }
async fn container_inspect(&self) { self.get_object(&format!("/containers/{id}/json")).await }
"#;

const ENV: &str = "# the allowlist\nPING=1\nSERVICES=1\nCONTAINERS=1\nPOST=0\nNETWORKS=0\n";

/// Negative controls for the client rules: each edit below breaks one, and the clean client
/// passes.
#[test]
fn client_rules_reject_each_escape() {
    let clean = client_violations(&[("src/docker/client.rs", CLIENT_OK)], ENV);
    assert!(clean.is_empty(), "{clean:?}");

    let extra = |code: &str| format!("{CLIENT_OK}{code}");
    let cases = [
        (
            "a POST",
            extra("fn update(&self) { *request.method_mut() = Method::POST; }\n"),
            "Method::POST in fn update",
        ),
        (
            "a HEAD outside the ping",
            extra("fn probe(&self) { *request.method_mut() = Method::HEAD; }\n"),
            "HEAD outside the ping, in fn probe",
        ),
        (
            "a second HEAD in the ping",
            CLIENT_OK.replace("let head = ||", "let again = Method::HEAD; let head = ||"),
            "2 HEAD requests",
        ),
        (
            "a second request builder",
            extra("fn raw(&self) { let request = Request::builder().body(()); }\n"),
            "Request::builder( in fn raw",
        ),
        (
            "a path the allowlist denies",
            extra("async fn network_list(&self) { self.get_list(\"/networks\", \"\").await }\n"),
            "/networks is not on the socket-proxy allowlist",
        ),
        (
            "a path whose key is 0",
            extra("async fn pull(&self) { self.get(\"/post\", \"\", &[]).await }\n"),
            "/post is not on the socket-proxy allowlist",
        ),
    ];
    for (name, client, want) in cases {
        let violations = client_violations(&[("src/docker/client.rs", &client)], ENV);
        assert!(
            violations.iter().any(|violation| violation.contains(want)),
            "{name}: want a violation containing {want:?}, got {violations:?}"
        );
    }

    // No HEAD in the ping at all, and a path pattern that matches nothing, are failures too.
    let headless = CLIENT_OK.replace("Method::HEAD", "Method::GET");
    let violations = client_violations(&[("src/docker/client.rs", &headless)], ENV);
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains("0 HEAD")),
        "{violations:?}"
    );
    let pathless = client_violations(
        &[(
            "src/docker/client.rs",
            "fn request() { Request::new(()); Method::GET }\nfn ping() { Method::HEAD }\n",
        )],
        ENV,
    );
    assert!(
        pathless
            .iter()
            .any(|violation| violation.contains("no request path")),
        "{pathless:?}"
    );
}
