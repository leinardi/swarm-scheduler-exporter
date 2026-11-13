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

//! The repository's tooling pins: every version written in more than one file is one value (the
//! Rust line and the image's release, zig, cargo-zigbuild, their archive digests and `ShellCheck`),
//! Trivy is never pinned, and no flag makes a gate pass on nothing. The checks are
//! `repo_meta::toolchain::one_value` over this repository's pin sources and `repo_meta::vacuous`'s;
//! the negative controls run them on altered copies of the real files and on samples.
//!
//! The Makefile is the one record of the zig, cargo-zigbuild and `rust-std` pins and their archive
//! digests (make-common's `rust-release.mk` reads them): the Dockerfile's build stage carries
//! copies of the versions and digests, checked against it. CI and the release pass only the
//! versions to gh-composite-actions' setup-zigbuild, checked against it too; that action checks the
//! archives against its own default digests, which no test here compares.

use repo_meta::toolchain::{PinSource, one_value, rust_release, toolchain_targets};
use repo_meta::vacuous::{AllowedFragment, VacuousFlags};
use repo_meta::{Problem, report};

/// Runs make in the repository without the flags of a parent make or commit hook.
fn make() -> std::process::Command {
    let mut command = std::process::Command::new("make");
    command
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
        .arg("--no-print-directory")
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .env_remove("MAKEOVERRIDES");
    command
}

/// Every `RUST_RELEASE_*` setting of the Makefile is `:=`, so an environment variable of the same
/// name cannot change the shipped bytes, while a command-line assignment (the release's
/// `RUST_RELEASE_PLATFORMS`, say) still wins.
#[test]
fn release_settings_ignore_environment_but_keep_command_line_overrides() {
    let makefile = meta::repo().read(RELEASE_PINS);
    let names: Vec<&str> = makefile
        .lines()
        .filter(|line| line.starts_with("RUST_RELEASE_"))
        .filter_map(|line| line.split_once(" := ").map(|(name, _)| name))
        .collect();
    assert!(
        names.len() >= 13,
        "{RELEASE_PINS} sets only {names:?} with :="
    );
    for name in names {
        let print = format!("--eval=print-release-setting: ; @printf '%s' '$({name})'");
        let baseline = make()
            .arg(&print)
            .arg("print-release-setting")
            .output()
            .unwrap_or_else(|error| panic!("cannot run make: {error}"));
        assert!(baseline.status.success(), "{name}: {baseline:?}");
        assert!(!baseline.stdout.is_empty(), "{name} is empty");
        for (arguments, expected) in [
            (Vec::new(), baseline.stdout.as_slice()),
            (vec![format!("{name}=override")], b"override".as_slice()),
        ] {
            let output = make()
                .arg(&print)
                .arg("print-release-setting")
                .args(arguments)
                .env(name, "environment-override")
                .output()
                .unwrap_or_else(|error| panic!("cannot run make: {error}"));
            assert!(output.status.success(), "{name}: {output:?}");
            assert_eq!(output.stdout, expected, "{name}");
        }
    }
}

const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const RELEASE_PINS: &str = "Makefile";
const RUST_TOOLCHAIN: &str = "rust-toolchain.toml";
const PRE_COMMIT: &str = ".pre-commit-config.yaml";
const CI_WORKFLOW: &str = ".github/workflows/ci.yaml";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yaml";
const SCHEDULED_SCAN: &str = ".github/workflows/scheduled-scan.yaml";

/// The `X.Y` line of the release in the `dhi.io/rust` tag of a `FROM`.
const IMAGE_TAG_LINE: &str = r"(?mi)^[ \t]*FROM[ \t]+(?:--\S+[ \t]+)*dhi\.io/rust:(\d+\.\d+)\.\d+-";
const ZIG_DOCKERFILE: &str = r"(?m)^ARG ZIG_VERSION=(\S+)$";
/// The `zig-version` input of gh-composite-actions' setup-zigbuild, in CI and the release.
const ZIG_ACTION: &str = r#"(?m)^\s*zig-version:\s*"?([\d.]+)"#;
const ZIGBUILD_DOCKERFILE: &str = r"(?m)^ARG CARGO_ZIGBUILD_VERSION=(\S+)$";
const ZIGBUILD_ACTION: &str = r#"(?m)^\s*cargo-zigbuild-version:\s*"?([\d.]+)"#;
/// shellcheck-py adds a fourth component of its own (v0.11.0.1), so only the first three count.
const SHELLCHECK_HOOK: &str =
    r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*v?(\d+\.\d+\.\d+)(?:\.\d+)?";
/// The `version` input of every gh-composite-actions setup-shellcheck step in CI.
const SHELLCHECK_ACTION: &str =
    r"setup-shellcheck@[0-9a-f]{40}[^\n]*\n\s*with:\s*\n\s*version:\s*v?(\d+\.\d+\.\d+)";
/// A Trivy version written down anywhere under .github: trivy-gate's `trivy-version` input, a
/// `TRIVY_VERSION`, or a setup-trivy step of its own.
const TRIVY_PIN: &str = r"(?m)trivy-version:|TRIVY_VERSION|aquasecurity/setup-trivy";

/// The cargo-zigbuild archives' build hosts in the Makefile and the Dockerfile, with the
/// Dockerfile's `case` arm that names each: (target, arm text before the digest).
const ZIGBUILD_ARMS: [(&str, &str); 2] = [
    (
        "x86_64-unknown-linux-musl",
        "zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "zigbuild_asset=aarch64-unknown-linux-gnu; zigbuild_sha256=",
    ),
];
/// The musl standard libraries' targets, likewise.
const STD_ARMS: [(&str, &str); 2] = [
    (
        "x86_64-unknown-linux-musl",
        "triple=x86_64-unknown-linux-musl; std_sha256=",
    ),
    (
        "aarch64-unknown-linux-musl",
        "triple=aarch64-unknown-linux-musl; std_sha256=",
    ),
];

/// Every YAML file under .github, with its text; none is a failure, since the scans would check
/// nothing.
fn github_yaml() -> Vec<(String, String)> {
    let repo = meta::repo();
    let files = repo.files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !files.is_empty(),
        "no YAML files under .github; the scan would check nothing"
    );
    repo.read_all(&files)
}

/// `content` with its first `from` replaced by `to`; `from` must be there, so an edit cannot
/// silently change nothing.
fn edited(content: &str, from: &str, to: &str) -> String {
    assert!(content.contains(from), "{from:?} is gone");
    content.replacen(from, to, 1)
}

/// What the pin checks read of a file: `edit` applied to its path and its content.
type Edit<'edit> = &'edit dyn Fn(&str, String) -> String;

/// A source of `pattern` in `file`, whose content `edit` has applied.
fn source(edit: Edit, file: &str, pattern: &str) -> PinSource {
    PinSource::new(file, &edit(file, meta::repo().read(file)), pattern)
}

/// The same file for a preset source.
fn read(edit: Edit, file: &str) -> String {
    edit(file, meta::repo().read(file))
}

/// A release setting of the Makefile: `NAME := value`.
fn release_pin(edit: Edit, name: &str) -> PinSource {
    source(
        edit,
        RELEASE_PINS,
        &format!(r"(?m)^{}\s*:=\s*(\S+)\s*$", name.replace('.', r"\.")),
    )
}

/// Every pin the repository writes in more than one file, each one value: the Rust line
/// (rust-toolchain.toml, Cargo's `rust-version`, the image's tag), the exact Rust release
/// (rust-toolchain.toml's channel, the image's tag and `ARG RUST_VERSION`, the release the musl
/// standard libraries are downloaded for in the Dockerfile and the Makefile), zig, cargo-zigbuild,
/// each archive digest (the Makefile's and the Dockerfile's) and `ShellCheck`. `edit` is applied to the content of each file read.
fn pin_problems(edit: Edit) -> Vec<Problem> {
    let dockerfile = read(edit, DOCKERFILE);
    let mut problems = one_value(
        "the Rust line",
        &[
            PinSource::rust_channel_line(RUST_TOOLCHAIN, &read(edit, RUST_TOOLCHAIN)),
            PinSource::rust_version_line("Cargo.toml", &read(edit, "Cargo.toml")),
            PinSource::new(DOCKERFILE, &dockerfile, IMAGE_TAG_LINE),
        ],
        &[],
    );
    problems.extend(one_value(
        "the Rust release",
        &[
            PinSource::rust_channel(RUST_TOOLCHAIN, &read(edit, RUST_TOOLCHAIN)),
            PinSource::rust_image_tag(DOCKERFILE, &dockerfile),
            PinSource::rust_version_arg(DOCKERFILE, &dockerfile),
            release_pin(edit, "RUST_RELEASE_RUST_STD_VERSION"),
        ],
        &[],
    ));
    problems.extend(one_value(
        "zig",
        &[
            release_pin(edit, "RUST_RELEASE_ZIG_VERSION"),
            source(edit, DOCKERFILE, ZIG_DOCKERFILE),
            source(edit, CI_WORKFLOW, ZIG_ACTION),
            source(edit, RELEASE_WORKFLOW, ZIG_ACTION),
        ],
        &[],
    ));
    problems.extend(one_value(
        "cargo-zigbuild",
        &[
            release_pin(edit, "RUST_RELEASE_CARGO_ZIGBUILD_VERSION"),
            source(edit, DOCKERFILE, ZIGBUILD_DOCKERFILE),
            source(edit, CI_WORKFLOW, ZIGBUILD_ACTION),
            source(edit, RELEASE_WORKFLOW, ZIGBUILD_ACTION),
        ],
        &[],
    ));
    for arch in ["x86_64", "aarch64"] {
        problems.extend(one_value(
            &format!("the zig {arch} digest"),
            &[
                release_pin(edit, &format!("RUST_RELEASE_ZIG_SHA256_{arch}")),
                PinSource::new(
                    DOCKERFILE,
                    &dockerfile,
                    &format!("zig_arch={arch}; zig_sha256=([0-9a-f]{{64}})"),
                ),
            ],
            &[],
        ));
    }
    for (target, arm) in ZIGBUILD_ARMS {
        problems.extend(one_value(
            &format!("the cargo-zigbuild digest for {target}"),
            &[
                release_pin(
                    edit,
                    &format!("RUST_RELEASE_CARGO_ZIGBUILD_SHA256_{target}"),
                ),
                PinSource::new(DOCKERFILE, &dockerfile, &format!("{arm}([0-9a-f]{{64}})")),
            ],
            &[],
        ));
    }
    for (target, arm) in STD_ARMS {
        problems.extend(one_value(
            &format!("the rust-std digest for {target}"),
            &[
                release_pin(edit, &format!("RUST_RELEASE_RUST_STD_SHA256_{target}")),
                PinSource::new(DOCKERFILE, &dockerfile, &format!("{arm}([0-9a-f]{{64}})")),
            ],
            &[],
        ));
    }
    problems.extend(one_value(
        "ShellCheck",
        &[
            source(edit, PRE_COMMIT, SHELLCHECK_HOOK),
            source(edit, CI_WORKFLOW, SHELLCHECK_ACTION),
        ],
        &[],
    ));
    problems
}

/// The pins hold: rust-toolchain.toml pins an exact release and the MSRV in Cargo.toml and the
/// image's tag are its line; that release, the image's tag, `ARG RUST_VERSION` and the Makefile's
/// `rust-std` are one release; CI's and the release's binaries and the image's build stage are
/// built with the same zig and cargo-zigbuild, from the same archives; and the `ShellCheck` CI installs is the release the shellcheck-py hook wraps.
#[test]
fn pins_are_one_value() {
    let problems = pin_problems(&|_, content| content);
    assert!(problems.is_empty(), "{}", report(&problems));
    rust_release(RUST_TOOLCHAIN, &meta::repo().read(RUST_TOOLCHAIN))
        .unwrap_or_else(|problem| panic!("{problem}"));
}

/// Every target rust-toolchain.toml installs has a `rust-std` digest in the Makefile.
#[test]
fn every_toolchain_target_has_a_rust_std_digest() {
    let pins = meta::repo().read(RELEASE_PINS);
    let targets = toolchain_targets(&meta::repo().read(RUST_TOOLCHAIN));
    assert!(!targets.is_empty(), "{RUST_TOOLCHAIN} names no target");
    for target in targets {
        assert!(
            pins.contains(&format!("RUST_RELEASE_RUST_STD_SHA256_{target} :=")),
            "{RELEASE_PINS} records no rust-std digest for {target}"
        );
    }
}

/// One negative control: `from` replaced by `to` in `file` breaks `want` problems.
type Refusal = (&'static str, &'static str, String, String, usize);

/// Each edit of the real files is refused with the problems it breaks.
fn assert_refused(refusals: Vec<Refusal>) {
    for (name, file, from, to, want) in refusals {
        let problems = pin_problems(&|path, content| {
            if path == file {
                edited(&content, &from, &to)
            } else {
                content
            }
        });
        assert_eq!(problems.len(), want, "{name}: {}", report(&problems));
    }
}

/// The negative control of the Rust pins, on the real files: another line or release, or a source
/// that no longer writes the pin.
#[test]
fn rust_pin_rules_refuse_each_edit() {
    let release = rust_release(RUST_TOOLCHAIN, &meta::repo().read(RUST_TOOLCHAIN))
        .unwrap_or_else(|problem| panic!("{problem}"));
    let line = release
        .rsplit_once('.')
        .map_or(release.as_str(), |(line, _)| line);
    let other_line = format!("{line}0");
    let channel = format!("channel = \"{release}\"");
    let msrv = format!("rust-version = \"{line}\"");
    let image_tag = format!("dhi.io/rust:{release}-");
    let argument = format!("ARG RUST_VERSION={release}");

    assert_refused(vec![
        (
            "a toolchain of another line",
            RUST_TOOLCHAIN,
            channel.clone(),
            format!("channel = \"{other_line}.1\""),
            2,
        ),
        (
            "a toolchain of another patch",
            RUST_TOOLCHAIN,
            channel.clone(),
            format!("channel = \"{line}.99\""),
            1,
        ),
        (
            "a channel that is not a release",
            RUST_TOOLCHAIN,
            channel,
            "channel = \"stable\"".to_owned(),
            2,
        ),
        (
            "an MSRV of another line",
            "Cargo.toml",
            msrv,
            format!("rust-version = \"{other_line}\""),
            1,
        ),
        (
            "an image of another line",
            DOCKERFILE,
            image_tag,
            format!("dhi.io/rust:{other_line}.1-"),
            2,
        ),
        (
            "standard libraries of another patch",
            DOCKERFILE,
            argument,
            format!("ARG RUST_VERSION={line}.99"),
            1,
        ),
        (
            "the Makefile's rust-std of another patch",
            RELEASE_PINS,
            format!("RUST_RELEASE_RUST_STD_VERSION := {release}"),
            format!("RUST_RELEASE_RUST_STD_VERSION := {line}.99"),
            1,
        ),
    ]);
}

/// The negative control of the tool pins, on the real files: a second version, or a source that no
/// longer writes the pin.
#[test]
fn tool_pin_rules_refuse_each_edit() {
    let makefile = meta::repo().read(RELEASE_PINS);
    let digest = makefile
        .lines()
        .find_map(|line| {
            line.strip_prefix("RUST_RELEASE_RUST_STD_SHA256_x86_64-unknown-linux-musl := ")
        })
        .unwrap_or_else(|| panic!("{RELEASE_PINS} records no x86_64 rust-std digest"));
    assert_refused(vec![
        (
            "zig in the Makefile",
            RELEASE_PINS,
            "RUST_RELEASE_ZIG_VERSION := ".to_owned(),
            "RUST_RELEASE_ZIG_VERSION := 9".to_owned(),
            1,
        ),
        (
            "zig",
            DOCKERFILE,
            "ARG ZIG_VERSION=".to_owned(),
            "ARG ZIG_VERSION=9".to_owned(),
            1,
        ),
        (
            "zig in CI",
            CI_WORKFLOW,
            "zig-version:".to_owned(),
            "zig-version-gone:".to_owned(),
            1,
        ),
        (
            "zig in the release",
            RELEASE_WORKFLOW,
            "zig-version: ".to_owned(),
            "zig-version: 9".to_owned(),
            1,
        ),
        (
            "cargo-zigbuild in the Makefile",
            RELEASE_PINS,
            "RUST_RELEASE_CARGO_ZIGBUILD_VERSION := ".to_owned(),
            "RUST_RELEASE_CARGO_ZIGBUILD_VERSION := 9".to_owned(),
            1,
        ),
        (
            "cargo-zigbuild in CI",
            CI_WORKFLOW,
            "cargo-zigbuild-version: ".to_owned(),
            "cargo-zigbuild-version: 9".to_owned(),
            1,
        ),
        (
            "cargo-zigbuild in the release",
            RELEASE_WORKFLOW,
            "cargo-zigbuild-version:".to_owned(),
            "cargo-zigbuild-version-gone:".to_owned(),
            1,
        ),
        (
            "a zig digest in the Makefile",
            RELEASE_PINS,
            "RUST_RELEASE_ZIG_SHA256_aarch64 := ".to_owned(),
            "RUST_RELEASE_ZIG_SHA256_aarch64 := 0".to_owned(),
            1,
        ),
        (
            "a cargo-zigbuild digest the Dockerfile no longer writes",
            DOCKERFILE,
            "zigbuild_sha256=".to_owned(),
            "zigbuild_digest=".to_owned(),
            1,
        ),
        (
            "a rust-std digest in the Makefile",
            RELEASE_PINS,
            digest.to_owned(),
            format!("{}0", &digest[..63]),
            1,
        ),
        (
            "a rust-std digest the Dockerfile no longer writes",
            DOCKERFILE,
            "std_sha256=".to_owned(),
            "std_digest=".to_owned(),
            1,
        ),
        (
            "ShellCheck",
            CI_WORKFLOW,
            "version: v0.".to_owned(),
            "version: v9.0.".to_owned(),
            1,
        ),
    ]);
}

/// The Trivy pins under .github, one problem per file that writes one.
fn trivy_pin_problems(files: &[(String, String)]) -> Vec<Problem> {
    let pin = repo_meta::pattern(TRIVY_PIN);
    files
        .iter()
        .filter(|(_, content)| pin.is_match(content))
        .map(|(file, _)| {
            Problem::at(
                file,
                "pins Trivy: trivy-gate installs its latest release, so every scan runs the newest \
                 checks",
            )
        })
        .collect()
}

/// Trivy is never pinned: every scan is trivy-gate's, at its default `latest`, and no workflow
/// installs a Trivy of its own.
#[test]
fn trivy_is_never_pinned() {
    let problems = trivy_pin_problems(&github_yaml());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control: each way of pinning Trivy is found, in the file that writes it.
#[test]
fn trivy_pins_are_recognized() {
    for sample in [
        "        with:\n          trivy-version: 0.76.2\n",
        "env:\n  TRIVY_VERSION: v0.76.2\n",
        "      - uses: aquasecurity/setup-trivy@0000000000000000000000000000000000000000 # v0.3.1\n",
    ] {
        let problems = trivy_pin_problems(&[(SCHEDULED_SCAN.to_owned(), sample.to_owned())]);
        assert_eq!(problems.len(), 1, "{sample:?}: {}", report(&problems));
    }
}

/// The flags no gate may carry: the core set, the Rust set with the rule that clippy denies
/// warnings.
fn vacuous_flags() -> VacuousFlags {
    VacuousFlags::core().with_rust()
}

/// The reviewed exceptions, each an exact fragment of one file. Each must still match at least
/// one line, so an entry cannot outlive the code it excuses and quietly cover a new line later.
const ALLOWED: &[AllowedFragment<'static>] = &[
    // grep exits 1 when it selects nothing, and "no release tag yet" is an answer here, not a
    // failure; the brace group confines the || true to that one grep under pipefail.
    AllowedFragment {
        file: RELEASE_WORKFLOW,
        fragment: r#"{ grep -E "$RELEASE_TAG_REGEX" || true; }"#,
        reason: "a tag filter that may select nothing",
    },
    // pre-commit-all restores the rust job's Rust cache and never saves it, so one job writes it.
    AllowedFragment {
        file: CI_WORKFLOW,
        fragment: "save-if: false",
        reason: "a restore-only Rust cache",
    },
    // A recovery that reuses a published image scans it again, but cannot change it: its findings
    // are reported as a warning by the step after, not a failure of the publish.
    AllowedFragment {
        file: RELEASE_WORKFLOW,
        fragment: "continue-on-error: true # findings in an image already published are warnings",
        reason: "the scan of a reused, already published image",
    },
];

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and every make
/// fragment, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let repo = meta::repo();
    let fragments = repo.files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let mut files = repo.read_all(&["Makefile".to_owned(), PRE_COMMIT.to_owned()]);
    files.extend(repo.read_all(&fragments));
    files.extend(github_yaml());
    let problems = vacuous_flags().problems(&files, ALLOWED);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found, and an allowed fragment excuses only itself, in its own file.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    let hits = |file: &str, line: &str| {
        let mut used = vec![false; ALLOWED.len()];
        flags.hits(file, line, ALLOWED, &mut used).len()
    };
    for sample in [
        "\tcargo clippy -- -D warnings || true",
        "\tcargo clippy -- -D warnings ||:",
        "\tcargo clippy -- -D warnings || exit 0",
        "\t-cargo test",
        "\t@-cargo test",
        ".IGNORE: rust-clippy",
        "\t$(MAKE) -i rust-test",
        "\t$(MAKE) --ignore-errors rust-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
        "\tcargo clippy --workspace",
        "\tcargo test --no-run",
        "\t$(CARGO) test -- --skip broker",
    ] {
        assert_eq!(hits("Makefile", sample), 1, "{sample:?}");
    }

    let release_filter = r#"{ grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V | tail -n1"#;
    assert_eq!(
        hits(RELEASE_WORKFLOW, release_filter),
        0,
        "the release tag filter is not allowed in release.yaml"
    );
    assert_eq!(
        hits(CI_WORKFLOW, release_filter),
        1,
        "the release tag filter is allowed outside release.yaml"
    );
    assert_eq!(
        hits(RELEASE_WORKFLOW, &format!("{release_filter} || true")),
        1,
        "a second || true beside the allowed one is allowed in release.yaml"
    );
    assert_eq!(
        hits(RELEASE_WORKFLOW, "make rust-test || true"),
        1,
        "an unrelated || true is allowed in release.yaml"
    );

    let reuse_scan =
        "        continue-on-error: true # findings in an image already published are warnings";
    assert_eq!(
        hits(RELEASE_WORKFLOW, reuse_scan),
        0,
        "the reused image's scan is not allowed in release.yaml"
    );
    assert_eq!(
        hits(CI_WORKFLOW, reuse_scan),
        1,
        "the reused image's scan is allowed outside release.yaml"
    );
    assert_eq!(
        hits(RELEASE_WORKFLOW, "        continue-on-error: true"),
        1,
        "another continue-on-error is allowed in release.yaml"
    );
}
