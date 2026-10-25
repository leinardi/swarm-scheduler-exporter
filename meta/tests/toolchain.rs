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

//! The repository's tooling pins: one Rust line across rust-toolchain.toml, Cargo.toml, the image
//! and every CI toolchain step; the image's toolchain patch and its musl standard libraries; one
//! zig and one cargo-zigbuild between the image and CI; one ShellCheck between the hook and CI; one
//! Trivy between the release and the scheduled scan; the always-run hooks running once; no flag
//! that makes a gate pass on nothing; and no meta-test that skips on a missing tool.

use std::collections::BTreeMap;

use meta::repo;
use regex::Regex;
use serde_yaml_ng::Value;

const RUST_TOOLCHAIN_ACTION: &str = "dtolnay/rust-toolchain";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const SETUP_ZIGBUILD: &str = ".github/actions/setup-zigbuild/action.yaml";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// One place a version is written: a file and the pattern whose first group is the version there.
struct PinSource {
    file: String,
    pattern: Regex,
}

fn source(file: &str, pattern: &str) -> PinSource {
    PinSource {
        file: file.to_owned(),
        pattern: re(pattern),
    }
}

/// Fails unless every source writes the pin and all of them agree.
fn expect_one_value(what: &str, sources: &[PinSource]) {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut problems = Vec::new();
    for pin in sources {
        let content = repo::read(&pin.file);
        let mut matched = false;
        for captures in pin.pattern.captures_iter(&content) {
            matched = true;
            found
                .entry(captures[1].to_owned())
                .or_default()
                .push(pin.file.clone());
        }
        if !matched {
            problems.push(format!(
                "{what}: {} does not write it ({})",
                pin.file, pin.pattern
            ));
        }
    }
    if found.len() > 1 {
        let values: Vec<String> = found
            .iter()
            .map(|(value, files)| {
                let mut files = files.clone();
                files.dedup();
                format!("{value} in {}", files.join(", "))
            })
            .collect();
        problems.push(format!("{what} is not one value: {}", values.join("; ")));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// One step, in a workflow or a composite action, that uses an action.
#[derive(Debug)]
struct ActionStep {
    file: String,
    /// The `uses:` value without its `@ref`.
    action: String,
    with: BTreeMap<String, String>,
}

/// Every mapping with a `uses:` key anywhere in the document: job steps and composite action
/// steps alike.
fn parse_action_steps(file: &str, content: &str) -> Vec<ActionStep> {
    fn walk(file: &str, node: &Value, steps: &mut Vec<ActionStep>) {
        match node {
            Value::Mapping(mapping) => {
                if let Some(Value::String(uses)) = mapping.get("uses") {
                    let action = uses
                        .split_once('@')
                        .map_or(uses.as_str(), |(action, _)| action);
                    let with = mapping
                        .get("with")
                        .and_then(Value::as_mapping)
                        .map(|inputs| {
                            inputs
                                .iter()
                                .filter_map(|(key, value)| {
                                    let value = match value {
                                        Value::String(text) => text.clone(),
                                        Value::Number(number) => number.to_string(),
                                        Value::Bool(flag) => flag.to_string(),
                                        Value::Null
                                        | Value::Sequence(_)
                                        | Value::Mapping(_)
                                        | Value::Tagged(_) => return None,
                                    };
                                    Some((key.as_str()?.to_owned(), value))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    steps.push(ActionStep {
                        file: file.to_owned(),
                        action: action.to_owned(),
                        with,
                    });
                }
                for value in mapping.values() {
                    walk(file, value, steps);
                }
            }
            Value::Sequence(sequence) => {
                for value in sequence {
                    walk(file, value, steps);
                }
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Tagged(_) => {}
        }
    }

    let document: Value =
        serde_yaml_ng::from_str(content).unwrap_or_else(|error| panic!("parse {file}: {error}"));
    let mut steps = Vec::new();
    walk(file, &document, &mut steps);
    steps
}

/// The Rust step rules: a toolchain step names an exact version, never a channel like stable,
/// which the Rust line test would not see move.
fn rust_pin_problems(steps: &[ActionStep]) -> Vec<String> {
    let exact = re(r"^\d+\.\d+\.\d+$");
    steps
        .iter()
        .filter(|step| step.action == RUST_TOOLCHAIN_ACTION)
        .filter_map(|step| match step.with.get("toolchain") {
            Some(toolchain) if exact.is_match(toolchain) => None,
            Some(toolchain) => Some(format!(
                "{}: {} installs {toolchain:?}; name the exact version of rust-toolchain.toml",
                step.file, step.action
            )),
            None => Some(format!(
                "{}: {} names no toolchain; name the exact version of rust-toolchain.toml",
                step.file, step.action
            )),
        })
        .collect()
}

fn github_yaml() -> Vec<String> {
    let files = repo::files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !files.is_empty(),
        "no YAML files under .github; the scan would check nothing"
    );
    files
}

fn github_steps() -> Vec<ActionStep> {
    github_yaml()
        .iter()
        .flat_map(|file| parse_action_steps(file, &repo::read(file)))
        .collect()
}

/// Go: `TestGoLineIsOneValue` and `TestRustToolchainIsOneValue`. The Rust line (major.minor) is
/// one value across rust-toolchain.toml, the MSRV in Cargo.toml, the build stage's image tag and
/// every CI toolchain step. The MSRV names no patch, so the exact release is checked by
/// `rust_toolchain_is_one_release` below.
#[test]
fn rust_line_is_one_value() {
    let mut sources = vec![
        source(
            "rust-toolchain.toml",
            r#"(?m)^channel = "(\d+\.\d+)(?:\.\d+)?"$"#,
        ),
        source(
            "Cargo.toml",
            r#"(?m)^rust-version = "(\d+\.\d+)(?:\.\d+)?"$"#,
        ),
        source(DOCKERFILE, r"dhi\.io/rust:(\d+\.\d+)[.-]"),
    ];
    let mut steps = 0;
    for file in github_yaml() {
        if parse_action_steps(&file, &repo::read(&file))
            .iter()
            .any(|step| step.action == RUST_TOOLCHAIN_ACTION)
        {
            steps += 1;
            sources.push(source(
                &file,
                r"(?m)^\s*toolchain:\s*(\d+\.\d+)(?:\.\d+)?\s*$",
            ));
        }
    }
    assert!(
        steps > 0,
        "no {RUST_TOOLCHAIN_ACTION} step found under .github"
    );
    expect_one_value("Rust line", &sources);
}

/// The exact Rust release, patch included, is one value across rust-toolchain.toml, the build
/// stage's image tag and every CI toolchain step, so the release binaries, the image's binary and
/// the tested build come from the same compiler.
#[test]
fn rust_toolchain_is_one_release() {
    let mut sources = vec![
        source(
            "rust-toolchain.toml",
            r#"(?m)^channel = "(\d+\.\d+\.\d+)"$"#,
        ),
        source(DOCKERFILE, r"dhi\.io/rust:(\d+\.\d+\.\d+)-"),
    ];
    for file in github_yaml() {
        if parse_action_steps(&file, &repo::read(&file))
            .iter()
            .any(|step| step.action == RUST_TOOLCHAIN_ACTION)
        {
            sources.push(source(&file, r"(?m)^\s*toolchain:\s*(\d+\.\d+\.\d+)\s*$"));
        }
    }
    expect_one_value("Rust release", &sources);
}

/// The build stage's standard libraries are written for the image's exact toolchain: the
/// `RUST_VERSION` argument is the version in the image tag.
#[test]
fn image_toolchain_patch_is_the_standard_library_version() {
    let dockerfile = repo::read(DOCKERFILE);
    let tag = re(r"dhi\.io/rust:(\d+\.\d+\.\d+)-")
        .captures(&dockerfile)
        .map(|captures| captures[1].to_owned());
    let argument = re(r"(?m)^ARG RUST_VERSION=(\S+)$")
        .captures(&dockerfile)
        .map(|captures| captures[1].to_owned());
    assert!(
        tag.is_some(),
        "the build stage's tag names no exact Rust version"
    );
    assert_eq!(
        tag, argument,
        "the image's Rust and the RUST_VERSION of its musl standard libraries differ"
    );
}

/// Go: `TestActionStepsArePinned`. Every Rust toolchain step under .github names an exact version.
#[test]
fn rust_toolchain_steps_are_pinned() {
    let steps = github_steps();
    let problems = rust_pin_problems(&steps);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        steps
            .iter()
            .any(|step| step.action == RUST_TOOLCHAIN_ACTION),
        "no {RUST_TOOLCHAIN_ACTION} step found under .github; the rule would check nothing"
    );
}

/// Go: `TestActionPinRules`, the negative control of the step rules: each unpinned form is a
/// problem.
#[test]
fn rust_toolchain_pin_rules() {
    let steps = parse_action_steps(
        "sample.yaml",
        r"jobs:
  test:
    steps:
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567 # master
        with:
          toolchain: 1.98.0
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: stable
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: 1.98
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
      - uses: some/other-action@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: nightly
",
    );
    assert_eq!(steps.len(), 5, "{steps:?}");
    let problems = rust_pin_problems(&steps);
    assert_eq!(problems.len(), 3, "{problems:#?}");
}

/// The release binaries and the image are built with the same zig, from the same archive.
#[test]
fn zig_is_one_version() {
    expect_one_value(
        "zig",
        &[
            source(DOCKERFILE, r"(?m)^ARG ZIG_VERSION=(\S+)$"),
            source(SETUP_ZIGBUILD, r#"ZIG_VERSION:\s*"?([\d.]+)"#),
        ],
    );
    // The x86_64 archive is downloaded in both places, so its digest is one value too: a bump
    // that moves one copy fails here rather than in the first release that runs it.
    expect_one_value(
        "zig x86_64 sha256",
        &[
            source(
                DOCKERFILE,
                r"amd64\) zig_arch=x86_64; zig_sha256=([0-9a-f]{64}) ;;",
            ),
            source(SETUP_ZIGBUILD, r"ZIG_SHA256:\s*([0-9a-f]{64})"),
        ],
    );
}

/// The release binaries and the image are built with the same cargo-zigbuild, from the same
/// archive.
#[test]
fn cargo_zigbuild_is_one_version() {
    expect_one_value(
        "cargo-zigbuild",
        &[
            source(DOCKERFILE, r"(?m)^ARG CARGO_ZIGBUILD_VERSION=(\S+)$"),
            source(SETUP_ZIGBUILD, r#"CARGO_ZIGBUILD_VERSION:\s*"?([\d.]+)"#),
        ],
    );
    expect_one_value(
        "cargo-zigbuild x86_64 sha256",
        &[
            source(
                DOCKERFILE,
                r"amd64\) zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=([0-9a-f]{64}) ;;",
            ),
            source(SETUP_ZIGBUILD, r"CARGO_ZIGBUILD_SHA256:\s*([0-9a-f]{64})"),
        ],
    );
}

/// Go: `TestShellCheckIsOneVersion`. droast's ShellCheck bridge runs the `shellcheck` on `PATH`,
/// which CI installs with setup-shellcheck; it must be the release the shellcheck-py hook wraps
/// (its rev adds a fourth component for the wrapper), which Dependabot moves.
#[test]
fn shellcheck_is_one_version() {
    expect_one_value(
        "ShellCheck",
        &[
            source(
                ".pre-commit-config.yaml",
                r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*(v\d+\.\d+\.\d+)",
            ),
            source(
                ".github/actions/setup-shellcheck/action.yaml",
                r#"SHELLCHECK_VERSION:\s*["']?(v[\d.]+)"#,
            ),
        ],
    );
}

/// Go: `TestTrivyIsOneVersion`. The release scan and the scheduled scan of the published image
/// use one Trivy, so a finding means the same in both (docs/release.md).
#[test]
fn trivy_is_one_version() {
    let trivy = r#"(?m)^\s*TRIVY_VERSION:\s*["']?(v\d+\.\d+\.\d+)"#;
    expect_one_value(
        "Trivy",
        &[
            source(".github/workflows/release.yaml", trivy),
            source(".github/workflows/scheduled-scan.yaml", trivy),
        ],
    );
}

/// The hooks of .pre-commit-config.yaml by id, as written; an id with aliases (the actionlint,
/// shellcheck and yamllint variants) appears more than once.
fn pre_commit_hooks() -> Vec<(String, Value)> {
    let config: Value = serde_yaml_ng::from_str(&repo::read(".pre-commit-config.yaml"))
        .unwrap_or_else(|error| panic!("parse .pre-commit-config.yaml: {error}"));
    let repos = config
        .get("repos")
        .and_then(Value::as_sequence)
        .unwrap_or_else(|| panic!(".pre-commit-config.yaml has no repos list"));
    repos
        .iter()
        .filter_map(|entry| entry.get("hooks").and_then(Value::as_sequence))
        .flatten()
        .map(|hook| {
            let id = hook.get("id").and_then(Value::as_str).unwrap_or_default();
            (id.to_owned(), hook.clone())
        })
        .collect()
}

/// The hooks that read the whole tree rather than the files pre-commit passes, so each one runs
/// on every commit and must not run twice per commit.
const ALWAYS_RUN_HOOKS: [&str; 3] = ["check-license-headers", "check-suppressions", "docs-links"];

/// Go: `TestAlwaysRunHooksRunOnce`. A whole-tree hook takes no file list and always runs, because
/// a commit that only deletes a file passes no file to it; and it is pinned to the pre-commit
/// stage, because `always_run` makes a hook eligible at every installed stage, commit-msg
/// included.
#[test]
fn always_run_hooks_run_once() {
    let hooks = pre_commit_hooks();
    let mut problems = Vec::new();
    for id in ALWAYS_RUN_HOOKS {
        let found: Vec<&Value> = hooks
            .iter()
            .filter(|(hook_id, _)| hook_id == id)
            .map(|(_, hook)| hook)
            .collect();
        let [hook] = found.as_slice() else {
            problems.push(format!(
                "hook {id} appears {} times, want once",
                found.len()
            ));
            continue;
        };
        if hook.get("always_run").and_then(Value::as_bool) != Some(true) {
            problems.push(format!("hook {id} does not set always_run: true"));
        }
        if hook.get("pass_filenames").and_then(Value::as_bool) != Some(false) {
            problems.push(format!("hook {id} does not set pass_filenames: false"));
        }
        let stages: Option<Vec<&str>> = hook
            .get("stages")
            .and_then(Value::as_sequence)
            .map(|stages| stages.iter().filter_map(Value::as_str).collect());
        if stages.as_deref() != Some(["pre-commit"].as_slice()) {
            problems.push(format!(
                "hook {id} runs at stages {stages:?}, want exactly [pre-commit]"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A flag that turns a gate with nothing to check, or a failing one, into a pass.
struct VacuousFlag {
    name: &'static str,
    pattern: Regex,
}

/// The flags none of the files that define gates may carry, outside [`ALLOWED_FRAGMENTS`].
fn vacuous_flags() -> Vec<VacuousFlag> {
    [
        ("|| true", r"\|\|\s*(true\b|:(\s|;|$)|exit\s+0\b)"),
        ("make's - recipe prefix", r"^\t[@+]*-"),
        (".IGNORE", r"^\.IGNORE\b"),
        (
            "make --ignore-errors",
            r"\$\(MAKE\).*\s(-[a-zA-Z]*i[a-zA-Z]*|--ignore-errors)\b",
        ),
        ("continue-on-error", r"continue-on-error"),
        (
            "if: false",
            r#"\bif:\s*(["']?false["']?\s*$|["']?\$\{\{\s*false\s*\}\}["']?)"#,
        ),
    ]
    .into_iter()
    .map(|(name, pattern)| VacuousFlag {
        name,
        pattern: re(pattern),
    })
    .collect()
}

/// One exact piece of shell, in one file, whose `|| true` is not a gate passing on nothing; the
/// reason says why. A line is allowed only when the flag is gone once the fragment is removed, so
/// a second `|| true` on the same line is still caught.
struct AllowedFragment {
    file: &'static str,
    fragment: &'static str,
    reason: &'static str,
}

const ALLOWED_FRAGMENTS: [AllowedFragment; 2] = [
    AllowedFragment {
        file: ".github/workflows/release.yaml",
        fragment: r#"{ grep -E "$RELEASE_TAG_REGEX" || true; }"#,
        reason: "a tag list with no release tag in it is an empty answer, not an error; grep \
                 exits 1 on no match and pipefail would turn that into a failed lookup",
    },
    AllowedFragment {
        file: ".github/workflows/release.yaml",
        fragment: r#"{ git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        reason: "HEAD^ does not exist on a root commit, which has no previous release",
    },
];

/// The flags on `content`'s lines, after removing the fragments allowed in `file`, as
/// `<line>: <flag>: <text>`; `used` records which allowlist entries matched.
fn vacuous_hits(
    flags: &[VacuousFlag],
    file: &str,
    content: &str,
    used: &mut [bool; ALLOWED_FRAGMENTS.len()],
) -> Vec<String> {
    let mut hits = Vec::new();
    for (index, line) in content.split('\n').enumerate() {
        let mut line = line.to_owned();
        for (allowed_index, allowed) in ALLOWED_FRAGMENTS.iter().enumerate() {
            if allowed.file == file && line.contains(allowed.fragment) {
                line = line.replace(allowed.fragment, "");
                used[allowed_index] = true;
            }
        }
        for flag in flags {
            if flag.pattern.is_match(&line) {
                hits.push(format!("{}: {}: {}", index + 1, flag.name, line.trim()));
            }
        }
    }
    hits
}

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and the make
/// fragments, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let flags = vacuous_flags();
    let fragments = repo::files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let workflows = repo::files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !workflows.is_empty(),
        "no YAML found under .github; the checks would see nothing"
    );
    let mut files = vec!["Makefile".to_owned(), ".pre-commit-config.yaml".to_owned()];
    files.extend(fragments);
    files.extend(workflows);

    let mut used = [false; ALLOWED_FRAGMENTS.len()];
    let mut problems = Vec::new();
    for file in &files {
        for hit in vacuous_hits(&flags, file, &repo::read(file), &mut used) {
            problems.push(format!("{file}:{hit}"));
        }
    }
    for (allowed, used) in ALLOWED_FRAGMENTS.iter().zip(used) {
        if !used {
            problems.push(format!(
                "the allowlist entry {:?} in {} ({}) matches nothing; remove it",
                allowed.fragment, allowed.file, allowed.reason
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found, and the allowlist covers only its own fragment in its own file.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    let mut used = [false; ALLOWED_FRAGMENTS.len()];
    for sample in [
        "\tcargo test --workspace || true",
        "\tcargo test --workspace ||:",
        "\tcargo test --workspace || exit 0",
        "\t-cargo test --workspace",
        "\t@-cargo test --workspace",
        ".IGNORE: go-vet",
        "\t$(MAKE) -i go-test",
        "\t$(MAKE) --ignore-errors go-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
    ] {
        assert!(
            !vacuous_hits(&flags, "Makefile", sample, &mut used).is_empty(),
            "no vacuous flag found in {sample:?}"
        );
    }

    let release = ".github/workflows/release.yaml";
    let allowed = r#"  { grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V"#;
    let hits = vacuous_hits(&flags, release, allowed, &mut used);
    assert!(
        hits.is_empty(),
        "the allowed fragment was flagged: {hits:?}"
    );

    let extended = format!("{allowed} || true");
    for (file, sample) in [
        (".github/workflows/ci.yaml", allowed),
        (release, extended.as_str()),
        (
            ".github/workflows/x.yaml",
            r#"  { git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        ),
    ] {
        assert!(
            !vacuous_hits(&flags, file, sample, &mut used).is_empty(),
            "{file}: {sample:?} passed through the allowlist"
        );
    }
}

/// Go: `TestToolGateListIsClosed`. No meta-test may skip on a missing tool: none looks a tool up
/// on `PATH`, since a skip on a missing tool is a pass CI cannot turn into a failure. (The Go list
/// held only the Go linter; the Dockerfile linter, droast, runs only as a pre-commit hook.) A test
/// that needs an external tool fails without it.
#[test]
fn no_meta_test_skips_on_a_missing_tool() {
    let own_lookup =
        re(r"\bsplit_paths\(|\bvar_os\(\s*\x22PATH\x22|Command::new\(\s*\x22which\x22");
    let sources = repo::files_under("meta", &[".rs"]);
    assert!(
        sources.iter().any(|path| path == "meta/tests/toolchain.rs"),
        "the walk did not reach meta/tests: {sources:?}"
    );
    for path in &sources {
        assert!(
            !own_lookup.is_match(&repo::read(path)),
            "{path} looks a tool up on PATH; a meta-test needing a tool must fail without it"
        );
    }

    // The negative control. The samples are split so this file does not match its own check.
    assert!(own_lookup.is_match(concat!("let found = env::split", "_paths(&paths);")));
    assert!(own_lookup.is_match(concat!("std::env::var", "_os(\x22PATH\x22)")));
    assert!(own_lookup.is_match(concat!("Command::new(\x22wh", "ich\x22)")));
}
