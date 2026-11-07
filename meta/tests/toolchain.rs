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

//! The repository's tooling pins: one exact Rust release across rust-toolchain.toml, the image and
//! every CI toolchain step, with Cargo.toml's `rust-version` on its line; the image's musl
//! standard libraries for that release; one zig and one cargo-zigbuild between the image and CI;
//! one ShellCheck between the hook and CI; one Trivy between the release and the scheduled scan;
//! and no flag that makes a gate pass on nothing. The checks are repo-meta's, with this
//! repository's files and pins; the negative controls run them on altered copies.

use repo_meta::toolchain::{
    RustVersion, ToolchainRef, ZigRecord, ci_toolchain_problems, image_rust_problems, rust_release,
    rust_version_problems, shellcheck_problems, trivy_problems, zigbuild_action_problems,
};
use repo_meta::vacuous::{AllowedFragment, VacuousFlags};
use repo_meta::{report, yaml};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const SETUP_ZIGBUILD: &str = ".github/actions/setup-zigbuild/action.yaml";
const SETUP_SHELLCHECK: &str = ".github/actions/setup-shellcheck/action.yaml";
const PRE_COMMIT: &str = ".pre-commit-config.yaml";
const CI: &str = ".github/workflows/ci.yaml";

/// The exact release rust-toolchain.toml pins.
fn release() -> String {
    rust_release(TOOLCHAIN, &meta::repo().read(TOOLCHAIN))
        .unwrap_or_else(|problem| panic!("{problem}"))
}

fn parse(file: &str, content: &str) -> yaml::Value {
    yaml::parse(file, content).unwrap_or_else(|problem| panic!("{problem}"))
}

/// Every YAML file under .github, the workflows and the composite actions, parsed.
fn github_documents() -> Vec<(String, yaml::Value)> {
    let repo = meta::repo();
    let files = repo.files_under(".github", &[".yaml", ".yml"]);
    assert!(
        files.iter().any(|file| file == CI),
        "the scan saw no CI workflow: {files:?}"
    );
    files
        .into_iter()
        .map(|file| {
            let document = parse(&file, &repo.read(&file));
            (file, document)
        })
        .collect()
}

/// Go: `TestGoLineIsOneValue`. The MSRV in Cargo.toml is the line (major.minor) of the exact
/// release rust-toolchain.toml pins, or a release of that line: it names no patch, so a consumer
/// on an earlier patch release still builds.
#[test]
fn rust_version_is_the_toolchain_line() {
    let problems = rust_version_problems(
        "Cargo.toml",
        &meta::repo().read("Cargo.toml"),
        &release(),
        RustVersion::Line,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestRustToolchainIsOneValue`. Every CI step that sets up Rust installs the exact release
/// rust-toolchain.toml pins, never a channel like stable or a release without its patch, so the
/// release binaries and the tested build come from the same compiler.
#[test]
fn ci_installs_the_toolchain_release() {
    let problems = ci_toolchain_problems(&github_documents(), &release(), ToolchainRef::Input);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The image's build stage builds with the exact release rust-toolchain.toml pins (its
/// `dhi.io/rust` tag), and its musl standard libraries are written for that toolchain: the
/// `RUST_VERSION` argument is the release in the tag.
#[test]
fn image_builds_with_the_toolchain_release() {
    let problems = image_rust_problems(
        DOCKERFILE,
        &meta::repo().read(DOCKERFILE),
        &release(),
        RustVersion::Exact,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestActionPinRules`, the negative control of the step rules: each unpinned form is a
/// problem, and so is another action's `toolchain` input naming another release (repo-meta holds
/// every CI toolchain input to the release, not only dtolnay/rust-toolchain's).
#[test]
fn rust_toolchain_pin_rules() {
    let sample = parse(
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
    let problems = ci_toolchain_problems(
        &[("sample.yaml".to_owned(), sample)],
        "1.98.0",
        ToolchainRef::Input,
    );
    assert_eq!(problems.len(), 4, "{problems:#?}");
}

/// The negative control of the image rule: a tag and a `RUST_VERSION` that differ, an image on
/// another release, and a `COPY --from=` of another release's image, are refused.
#[test]
fn image_rust_rules() {
    let dockerfile = meta::repo().read(DOCKERFILE);
    let release = release();
    let check = |content: &str| {
        image_rust_problems(DOCKERFILE, content, &release, RustVersion::Exact).is_empty()
    };
    assert!(check(&dockerfile));
    let argument = format!("ARG RUST_VERSION={release}\n");
    let tag = format!("dhi.io/rust:{release}-");
    for broken in [
        dockerfile.replacen(&argument, "ARG RUST_VERSION=1.0.0\n", 1),
        dockerfile.replacen(&tag, "dhi.io/rust:1.0.0-", 1),
        dockerfile
            .replacen(&argument, "ARG RUST_VERSION=1.0.0\n", 1)
            .replacen(&tag, "dhi.io/rust:1.0.0-", 1),
        format!(
            "{dockerfile}COPY --from=dhi.io/rust:1.97.0-debian13-dev /usr/local/rustup \
             /usr/local/rustup\n"
        ),
    ] {
        assert_ne!(broken, dockerfile, "a negative control changed nothing");
        assert!(!check(&broken), "{broken}");
    }
}

/// The release binaries and the image are built with the same zig and the same cargo-zigbuild,
/// from the same `x86_64` archives: both download those, so a bump that moves one copy fails here
/// rather than in the first release that runs it.
#[test]
fn zig_and_cargo_zigbuild_are_one_version() {
    let repo = meta::repo();
    let dockerfile = repo.read(DOCKERFILE);
    let problems = zigbuild_action_problems(
        SETUP_ZIGBUILD,
        &repo.read(SETUP_ZIGBUILD),
        ZigRecord::Dockerfile {
            file: DOCKERFILE,
            content: &dockerfile,
        },
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestShellCheckIsOneVersion`. droast's ShellCheck bridge runs the `shellcheck` on `PATH`,
/// which CI installs with setup-shellcheck; it must be the release the shellcheck-py hook wraps
/// (its rev adds a fourth component for the wrapper), which Dependabot moves.
#[test]
fn shellcheck_is_one_version() {
    let repo = meta::repo();
    let problems = shellcheck_problems(
        PRE_COMMIT,
        &repo.read(PRE_COMMIT),
        SETUP_SHELLCHECK,
        &repo.read(SETUP_SHELLCHECK),
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestTrivyIsOneVersion`. The release scan and the scheduled scan of the published image
/// install one exact Trivy, so a finding means the same in both (docs/release.md).
#[test]
fn trivy_is_one_version() {
    let repo = meta::repo();
    let workflows = repo.read_all(&[
        ".github/workflows/release.yaml".to_owned(),
        ".github/workflows/scheduled-scan.yaml".to_owned(),
    ]);
    let problems = trivy_problems(&workflows);
    assert!(problems.is_empty(), "{}", report(&problems));
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
