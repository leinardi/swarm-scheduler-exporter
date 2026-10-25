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
//! reaches a daemon sits behind testenv's `integration` feature, and no other test target drives
//! the docker CLI.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use meta::repo;
use regex::Regex;
use toml::Table;

const HARNESS_LIB: &str = "testenv/src/lib.rs";
const HARNESS_MANIFEST: &str = "testenv/Cargo.toml";
const FEATURE: &str = "integration";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// What reaches a Docker daemon from a source file: the SDK imported or named in code (not in a
/// comment or a string, as the boundary test's own fixtures do), or the docker CLI as a child.
static DRIVES_DOCKER: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^\s*(?:pub )?use bollard\b|^[^\x22/\n]*\bbollard::|Command::new\(\s*\x22docker\x22")
});
/// A module declaration in lib.rs, with the attribute lines right above it.
static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^((?:#\[[^\n]*\]\n)*)(?:pub(?:\([a-z]+\))? )?mod ([a-z_][a-z0-9_]*);")
});

/// The modules lib.rs declares without the `integration` feature gate.
fn ungated_modules(lib: &str) -> BTreeSet<String> {
    let gate = format!("#[cfg(feature = \x22{FEATURE}\x22)]");
    MODULE
        .captures_iter(lib)
        .filter(|captures| !captures[1].contains(&gate))
        .map(|captures| captures[2].to_owned())
        .collect()
}

/// The files of `files` (path and text) that drive Docker.
fn docker_drivers(files: &[(String, String)]) -> Vec<&str> {
    files
        .iter()
        .filter(|(_, text)| DRIVES_DOCKER.is_match(text))
        .map(|(path, _)| path.as_str())
        .collect()
}

fn read_all(paths: &[String]) -> Vec<(String, String)> {
    paths
        .iter()
        .map(|path| (path.clone(), repo::read(path)))
        .collect()
}

/// Go: `TestIntegrationTreesAreTagged`. Every harness module that reaches Docker is compiled only
/// with the `integration` feature, and the suite itself is a test target that requires it.
#[test]
fn integration_tree_is_gated() {
    let ungated = ungated_modules(&repo::read(HARNESS_LIB));
    assert!(
        !ungated.is_empty(),
        "no ungated module in {HARNESS_LIB}; the check would pass on nothing"
    );
    let ungated_files: Vec<String> = repo::files_under("testenv/src", &[".rs"])
        .into_iter()
        .filter(|path| {
            let module = path
                .trim_start_matches("testenv/src/")
                .split(['/', '.'])
                .next()
                .unwrap_or_default();
            ungated.contains(module)
        })
        .collect();
    let sources = read_all(&ungated_files);
    let drivers = docker_drivers(&sources);
    assert!(
        drivers.is_empty(),
        "harness modules built without the {FEATURE} feature reach Docker: {drivers:?}"
    );

    let manifest: Table = repo::read(HARNESS_MANIFEST)
        .parse()
        .unwrap_or_else(|error| panic!("parse {HARNESS_MANIFEST}: {error}"));
    let suite = manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .and_then(|targets| {
            targets
                .iter()
                .find(|target| target.get("name").and_then(toml::Value::as_str) == Some(FEATURE))
        })
        .unwrap_or_else(|| panic!("{HARNESS_MANIFEST} declares no [[test]] named {FEATURE}"));
    let required: Vec<&str> = suite
        .get("required-features")
        .and_then(toml::Value::as_array)
        .map(|features| features.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        required,
        [FEATURE],
        "the {FEATURE} suite must require exactly the {FEATURE} feature"
    );
}

/// Go: `TestUntaggedTestsAvoidDocker`. No source of the exporter, its tests or the meta crate
/// reaches Docker: the exporter's own client talks to a fake Engine in its tests, and only the
/// harness behind the feature drives a daemon.
#[test]
fn plain_tests_avoid_docker() {
    let mut paths = Vec::new();
    for tree in ["src", "tests", "meta"] {
        paths.extend(repo::files_under(tree, &[".rs"]));
    }
    assert!(
        paths.len() > 10,
        "only {} sources found; the check would pass on nothing",
        paths.len()
    );
    let sources = read_all(&paths);
    let drivers = docker_drivers(&sources);
    assert!(
        drivers.is_empty(),
        "sources outside the gated harness reach Docker: {drivers:?}"
    );
}

/// Go: `TestDockerCheckRecognizesItsTargets`. The negative controls: a gated declaration is
/// recognised, an ungated one is reported, and both ways of reaching Docker are found.
#[test]
fn separation_rules_are_recognized() {
    let lib = "#[cfg(feature = \x22integration\x22)]\nmod dind;\n\
               #[cfg(feature = \x22integration\x22)]\npub mod context;\n\
               mod error;\npub mod redact;\n#[cfg(test)]\nmod tests;\n";
    assert_eq!(
        ungated_modules(lib),
        BTreeSet::from(["error".to_owned(), "redact".to_owned(), "tests".to_owned()])
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
    assert_eq!(docker_drivers(&files), ["a.rs", "b.rs", "e.rs"]);
}
