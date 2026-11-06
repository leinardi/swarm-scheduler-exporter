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

//! The line between the plain `cargo test` and the Docker-driving integration suite, which
//! `cargo test --workspace` in CI and the cargo-test hook run without Docker: everything that
//! reaches a daemon sits behind testenv's `integration` feature, every testenv test target
//! requires it, no plain test command turns it on, and no other test target drives the docker
//! CLI. The checks are `repo_meta::integration`'s.

use std::collections::BTreeSet;

use repo_meta::integration::{IntegrationGate, cargo_test_commands, docker_drivers};
use repo_meta::{Problem, report};

const GATE: IntegrationGate<'static> = IntegrationGate {
    feature: "integration",
    src_dir: "testenv/src",
    manifest: "testenv/Cargo.toml",
    suite: "integration",
    integration_files: &[
        ".mk/integration-test.mk",
        ".github/workflows/integration.yaml",
    ],
};

/// The crate whose use drives a daemon: bollard, the SDK the harness creates and removes things
/// with. The exporter's own client is held to the read-only rules of `docker_boundary.rs`.
const CLIENTS: &[&str] = &["bollard"];

const HARNESS_LIB: &str = "testenv/src/lib.rs";

/// Go: `TestIntegrationTreesAreTagged`. Every harness source built without the `integration`
/// feature (lib.rs and each module it declares without the gate) stays off Docker; the feature
/// exists and is off by default, and every testenv test target, the suite among them, requires
/// exactly it.
#[test]
fn integration_tree_is_gated() {
    let repo = meta::repo();
    let lib = repo.read(HARNESS_LIB);
    assert!(
        !GATE.gated_modules(&lib).is_empty(),
        "no module of {HARNESS_LIB} is behind the integration feature; the gate checks nothing"
    );
    let ungated = GATE.ungated_sources(&lib, &repo.files_under(GATE.src_dir, &[".rs"]));
    assert!(
        ungated.iter().any(|path| path != HARNESS_LIB),
        "no ungated module in {HARNESS_LIB}; the check would pass on nothing"
    );
    let mut problems = docker_drivers(&repo.read_all(&ungated), CLIENTS);
    problems.extend(GATE.manifest_problems(&repo.read(GATE.manifest)));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// No plain test command (the Makefile and its fragments, the pre-commit configuration, every
/// workflow and action) turns the `integration` feature on: only the integration fragment and
/// workflow may.
#[test]
fn plain_test_commands_leave_the_feature_off() {
    let repo = meta::repo();
    let mut files = vec!["Makefile".to_owned(), ".pre-commit-config.yaml".to_owned()];
    files.extend(repo.files_under(".mk", &[".mk"]));
    files.extend(repo.files_under(".github", &[".yaml", ".yml"]));
    let sources = repo.read_all(&files);
    let commands: usize = sources
        .iter()
        .map(|(_, content)| cargo_test_commands(content))
        .sum();
    assert!(
        commands > 0,
        "no cargo test command found in {files:?}; the check would pass on nothing"
    );
    let problems: Vec<Problem> = sources
        .iter()
        .flat_map(|(file, content)| GATE.plain_test_problems(file, content))
        .collect();
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestUntaggedTestsAvoidDocker`. No source of the exporter, its tests or the meta crate
/// reaches Docker: the exporter's own client talks to a fake Engine in its tests, and only the
/// harness behind the feature drives a daemon.
#[test]
fn plain_tests_avoid_docker() {
    let repo = meta::repo();
    let mut paths = Vec::new();
    for tree in ["src", "tests", "meta"] {
        paths.extend(repo.files_under(tree, &[".rs"]));
    }
    assert!(
        paths.len() > 10,
        "only {} sources found; the check would pass on nothing",
        paths.len()
    );
    let problems = docker_drivers(&repo.read_all(&paths), CLIENTS);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestDockerCheckRecognizesItsTargets`. The negative controls: a gated declaration is
/// recognised, an ungated one is scanned, both ways of reaching Docker are found, and a feature
/// turned on in a plain test command is refused.
#[test]
fn separation_rules_are_recognized() {
    let lib = "#[cfg(feature = \x22integration\x22)]\nmod dind;\n\
               #[cfg(feature = \x22integration\x22)]\npub mod context;\n\
               mod error;\npub mod redact;\n#[cfg(test)]\nmod tests;\n";
    assert_eq!(
        GATE.gated_modules(lib),
        BTreeSet::from(["context".to_owned(), "dind".to_owned()])
    );
    let paths: Vec<String> = [
        "testenv/src/lib.rs",
        "testenv/src/dind.rs",
        "testenv/src/context/mod.rs",
        "testenv/src/error.rs",
        "testenv/src/redact.rs",
        "testenv/src/tests.rs",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(
        GATE.ungated_sources(lib, &paths),
        [
            "testenv/src/lib.rs",
            "testenv/src/error.rs",
            "testenv/src/redact.rs",
            "testenv/src/tests.rs"
        ]
    );
    // A module also declared without the gate (a stub for the plain build) is built there, so
    // it is not gated and its files are scanned.
    let doubled = "#[cfg(feature = \x22integration\x22)]\nmod dind;\n\
                   #[cfg(not(feature = \x22integration\x22))]\nmod dind;\n";
    assert_eq!(GATE.gated_modules(doubled), BTreeSet::new());
    assert_eq!(
        GATE.ungated_sources(doubled, &["testenv/src/dind.rs".to_owned()]),
        ["testenv/src/dind.rs"]
    );

    let files = [
        ("a.rs".to_owned(), "use bollard::Docker;".to_owned()),
        (
            "b.rs".to_owned(),
            "let out = Command::new(\x22docker\x22).arg(\x22ps\x22);".to_owned(),
        ),
        (
            "c.rs".to_owned(),
            "let out = Command::new(\x22git\x22); // docker".to_owned(),
        ),
        (
            "d.rs".to_owned(),
            "let fixture = \x22use bollard::Docker;\x22; // bollard::Docker".to_owned(),
        ),
        (
            "e.rs".to_owned(),
            "    let client = bollard::Docker::connect();".to_owned(),
        ),
    ];
    let found: Vec<String> = docker_drivers(&files, CLIENTS)
        .into_iter()
        .map(|problem| problem.location)
        .collect();
    assert_eq!(found, ["a.rs", "b.rs", "e.rs"]);

    let enabled = "\t$(CARGO) test --workspace --locked --features integration";
    assert_eq!(GATE.plain_test_problems(".mk/rust.mk", enabled).len(), 1);
    assert_eq!(
        GATE.plain_test_problems(".mk/integration-test.mk", enabled),
        []
    );
    let without = "[features]\nintegration = []\n\n[[test]]\nname = \x22integration\x22\n";
    assert_eq!(
        GATE.manifest_problems(without).len(),
        1,
        "a suite without the feature"
    );
}
