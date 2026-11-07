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

//! The repository's tooling pins, each one value everywhere it is written: the Rust release across
//! rust-toolchain.toml and the image (its `dhi.io/rust` tag and `RUST_VERSION`), with Cargo.toml's
//! `rust-version` on its line; one zig and one cargo-zigbuild between the image and CI; one
//! ShellCheck between the hook and CI; one Trivy between the release and the scheduled scan; and no
//! flag that makes a gate pass on nothing. The pins are `repo_meta::toolchain::one_value`'s, over
//! this repository's files; each has a negative control that alters one copy.

use repo_meta::report;
use repo_meta::toolchain::{PinSource, PinValue, one_value};
use repo_meta::vacuous::{AllowedFragment, VacuousFlags};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const CARGO: &str = "Cargo.toml";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const SETUP_ZIGBUILD: &str = ".github/actions/setup-zigbuild/action.yaml";
const SETUP_SHELLCHECK: &str = ".github/actions/setup-shellcheck/action.yaml";
const PRE_COMMIT: &str = ".pre-commit-config.yaml";
const RELEASE: &str = ".github/workflows/release.yaml";
const SCHEDULED_SCAN: &str = ".github/workflows/scheduled-scan.yaml";

/// The text of `file`.
fn read(file: &str) -> String {
    meta::repo().read(file)
}

/// The sources of one pin, read from the repository, `altered` swapping the text of one file for
/// the negative controls.
type Sources = fn(altered: Option<(&str, &str)>) -> Vec<PinSource>;

/// The text of `file`, or the altered copy when `altered` names it.
fn text(file: &str, altered: Option<(&str, &str)>) -> String {
    match altered {
        Some((name, content)) if name == file => content.to_owned(),
        _ => read(file),
    }
}

/// `one_value` over the real files: nothing is wrong.
fn assert_one_value(what: &str, sources: Sources) {
    let problems = one_value(what, &sources(None), &[]);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control: `file` with its first `from` replaced by `to` is refused. `from` must be
/// there, so an edit cannot silently change nothing.
fn assert_refused(what: &str, sources: Sources, file: &str, from: &str, to: &str) {
    let original = read(file);
    assert!(original.contains(from), "{file}: {from:?} is gone");
    let broken = original.replacen(from, to, 1);
    let problems = one_value(what, &sources(Some((file, &broken))), &[]);
    assert!(!problems.is_empty(), "{what}: {file} with {to:?} passes");
}

/// The exact Rust release, in rust-toolchain.toml, the image's `dhi.io/rust` tag and its
/// `RUST_VERSION` argument, which names the musl standard libraries the build stage downloads.
fn rust_release_sources(altered: Option<(&str, &str)>) -> Vec<PinSource> {
    let toolchain = text(TOOLCHAIN, altered);
    let dockerfile = text(DOCKERFILE, altered);
    vec![
        PinSource::rust_channel(TOOLCHAIN, &toolchain),
        PinSource::rust_image_tag(DOCKERFILE, &dockerfile),
        PinSource::rust_version_arg(DOCKERFILE, &dockerfile),
    ]
}

#[test]
fn rust_release_is_one_value() {
    assert_one_value("the Rust release", rust_release_sources);
}

#[test]
fn rust_release_control_fails_on_another_release() {
    let release = read(TOOLCHAIN);
    let channel = release
        .lines()
        .find(|line| line.starts_with("channel"))
        .expect("rust-toolchain.toml has a channel")
        .to_owned();
    assert_refused(
        "the Rust release",
        rust_release_sources,
        TOOLCHAIN,
        &channel,
        "channel = \"1.0.0\"",
    );
    assert_refused(
        "the Rust release",
        rust_release_sources,
        DOCKERFILE,
        "dhi.io/rust:1.98.1-",
        "dhi.io/rust:1.0.0-",
    );
    assert_refused(
        "the Rust release",
        rust_release_sources,
        DOCKERFILE,
        "ARG RUST_VERSION=1.98.1\n",
        "ARG RUST_VERSION=1.0.0\n",
    );
}

/// The MSRV in Cargo.toml is the line (major.minor) of the release rust-toolchain.toml pins: it
/// names no patch, so a consumer on an earlier patch release still builds.
fn rust_line_sources(altered: Option<(&str, &str)>) -> Vec<PinSource> {
    vec![
        PinSource::rust_channel_line(TOOLCHAIN, &text(TOOLCHAIN, altered)),
        PinSource::rust_version_line(CARGO, &text(CARGO, altered)),
    ]
}

#[test]
fn rust_version_is_the_toolchain_line() {
    assert_one_value("the Rust line", rust_line_sources);
}

#[test]
fn rust_version_control_fails_on_another_line() {
    assert_refused(
        "the Rust line",
        rust_line_sources,
        CARGO,
        "rust-version = \"1.98\"",
        "rust-version = \"1.97\"",
    );
}

/// The release binaries and the image are built with the same zig and the same cargo-zigbuild,
/// from the same `x86_64` archives: both download those, so a bump that moves one copy fails here
/// rather than in the first release that runs it.
const ZIG_PINS: [(&str, &str, &str); 4] = [
    (
        "zig",
        r#"(?m)^\s*ZIG_VERSION:\s*"?(\d+\.\d+\.\d+)"?\s*(?:#.*)?$"#,
        r"(?m)^ARG ZIG_VERSION=(\S+)\s*$",
    ),
    (
        "zig x86_64 sha256",
        r"(?m)^\s*ZIG_SHA256:\s*([0-9a-f]{64})\s*(?:#.*)?$",
        r"amd64\) zig_arch=x86_64; zig_sha256=([0-9a-f]{64}) ;;",
    ),
    (
        "cargo-zigbuild",
        r#"(?m)^\s*CARGO_ZIGBUILD_VERSION:\s*"?(\d+\.\d+\.\d+)"?\s*(?:#.*)?$"#,
        r"(?m)^ARG CARGO_ZIGBUILD_VERSION=(\S+)\s*$",
    ),
    (
        "cargo-zigbuild x86_64 sha256",
        r"(?m)^\s*CARGO_ZIGBUILD_SHA256:\s*([0-9a-f]{64})\s*(?:#.*)?$",
        r"amd64\) zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=([0-9a-f]{64}) ;;",
    ),
];

fn zig_sources(index: usize, altered: Option<(&str, &str)>) -> Vec<PinSource> {
    let (_, action, dockerfile) = ZIG_PINS[index];
    vec![
        PinSource::new(SETUP_ZIGBUILD, &text(SETUP_ZIGBUILD, altered), action),
        PinSource::new(DOCKERFILE, &text(DOCKERFILE, altered), dockerfile),
    ]
}

#[test]
fn zig_and_cargo_zigbuild_are_one_version() {
    for (index, (what, _, _)) in ZIG_PINS.iter().enumerate() {
        let problems = one_value(what, &zig_sources(index, None), &[]);
        assert!(problems.is_empty(), "{}", report(&problems));
    }
}

#[test]
fn zig_control_fails_on_another_version() {
    let action = read(SETUP_ZIGBUILD);
    for (index, (what, _, _)) in ZIG_PINS.iter().enumerate() {
        let changed = [
            ("ZIG_VERSION: \"0.17.0\"", "ZIG_VERSION: \"0.16.0\""),
            (
                "CARGO_ZIGBUILD_VERSION: \"0.24.0\"",
                "CARGO_ZIGBUILD_VERSION: \"0.23.0\"",
            ),
        ];
        // Each version pin is refused when the action's copy moves; each digest when it changes.
        let (from, to) = match index {
            0 => changed[0],
            2 => changed[1],
            1 => ("ZIG_SHA256: 1cbe", "ZIG_SHA256: 0000"),
            _ => ("CARGO_ZIGBUILD_SHA256: 52be", "CARGO_ZIGBUILD_SHA256: 0000"),
        };
        assert!(action.contains(from), "{from:?} is gone");
        let broken = action.replacen(from, to, 1);
        let sources = zig_sources(index, Some((SETUP_ZIGBUILD, &broken)));
        assert!(
            !one_value(what, &sources, &[]).is_empty(),
            "{what}: {to:?} passes"
        );
    }
}

/// droast's ShellCheck bridge runs the `shellcheck` on `PATH`, which CI installs with
/// setup-shellcheck; it must be the release the shellcheck-py hook wraps (its rev adds a fourth
/// component for the wrapper), which Dependabot moves.
fn shellcheck_sources(altered: Option<(&str, &str)>) -> Vec<PinSource> {
    vec![
        PinSource::new(
            PRE_COMMIT,
            &text(PRE_COMMIT, altered),
            r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*(v\d+\.\d+\.\d+)",
        ),
        PinSource::new(
            SETUP_SHELLCHECK,
            &text(SETUP_SHELLCHECK, altered),
            r#"SHELLCHECK_VERSION:\s*["']?(v[\d.]+)"#,
        ),
    ]
}

#[test]
fn shellcheck_is_one_version() {
    assert_one_value("ShellCheck", shellcheck_sources);
}

#[test]
fn shellcheck_control_fails_on_another_version() {
    assert_refused(
        "ShellCheck",
        shellcheck_sources,
        SETUP_SHELLCHECK,
        "SHELLCHECK_VERSION: v0.11.0",
        "SHELLCHECK_VERSION: v0.10.0",
    );
}

/// The release scan and the scheduled scan of the published image install one exact Trivy, so a
/// finding means the same in both (docs/release.md).
fn trivy_sources(altered: Option<(&str, &str)>) -> Vec<PinSource> {
    [RELEASE, SCHEDULED_SCAN]
        .into_iter()
        .map(|file| {
            PinSource::new(
                file,
                &text(file, altered),
                r"(?m)^\s*TRIVY_VERSION:\s*(v\d+\.\d+\.\d+)\s*$",
            )
        })
        .collect()
}

#[test]
fn trivy_is_one_version() {
    assert_one_value("Trivy", trivy_sources);
}

#[test]
fn trivy_control_fails_on_another_version() {
    assert_refused(
        "Trivy",
        trivy_sources,
        SCHEDULED_SCAN,
        "TRIVY_VERSION: v0.74.0",
        "TRIVY_VERSION: v0.73.0",
    );
    // A source that writes no pin at all is refused as well, not read as agreeing.
    assert!(
        !one_value(
            "Trivy",
            &[],
            &[PinValue::new("v1.0.0", "a"), PinValue::new("v2.0.0", "b")]
        )
        .is_empty()
    );
}

/// The flags none of the files that define gates may carry: the core set and the Rust set (a test
/// run that only compiles, capped or allowed lints, tests skipped by name, a member excluded, a
/// clippy run without `-D warnings`), outside [`ALLOWED_FRAGMENTS`].
fn vacuous_flags() -> VacuousFlags {
    VacuousFlags::core().with_rust()
}

/// The exact pieces of shell, each in one file, whose `|| true` is not a gate passing on nothing;
/// the reason says why. A line is allowed only when the flag is gone once the fragment is
/// removed, so a second `|| true` on the same line is still caught, and an entry that matches
/// nothing is a problem.
const ALLOWED_FRAGMENTS: [AllowedFragment<'static>; 2] = [
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

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and the make
/// fragments, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let repo = meta::repo();
    let fragments = repo.files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let workflows = repo.files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !workflows.is_empty(),
        "no YAML found under .github; the checks would see nothing"
    );
    let mut files = vec!["Makefile".to_owned(), PRE_COMMIT.to_owned()];
    files.extend(fragments);
    files.extend(workflows);

    let problems = vacuous_flags().problems(&repo.read_all(&files), &ALLOWED_FRAGMENTS);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found, and the allowlist covers only its own fragment in its own file.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    let mut used = [false; ALLOWED_FRAGMENTS.len()];
    let mut hits = |file: &str, sample: &str| {
        flags
            .hits(file, sample, &ALLOWED_FRAGMENTS, &mut used)
            .len()
    };
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
        "\t$(CARGO) test --workspace --locked --no-run",
        "        entry: cargo test --workspace --locked -- --skip slow",
        "\t$(CARGO) clippy --workspace --all-targets --all-features --locked",
    ] {
        assert!(
            hits("Makefile", sample) > 0,
            "no vacuous flag found in {sample:?}"
        );
    }

    let release = ".github/workflows/release.yaml";
    let allowed = r#"  { grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V"#;
    assert_eq!(
        hits(release, allowed),
        0,
        "the allowed fragment was flagged"
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
            hits(file, sample) > 0,
            "{file}: {sample:?} passed through the allowlist"
        );
    }

    // An entry that matches nothing is a problem, so it cannot outlive the code it excuses.
    let problems = flags.problems(
        &[(release.to_owned(), "jobs: {}\n".to_owned())],
        &ALLOWED_FRAGMENTS,
    );
    assert_eq!(problems.len(), ALLOWED_FRAGMENTS.len(), "{problems:?}");
}
