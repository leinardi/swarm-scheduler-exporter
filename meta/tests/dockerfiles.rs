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

//! The shipped image's invariants that docs/release.md and the Dockerfile's own comments state:
//! one Dockerfile, a pinned frontend, two stages from tag-pinned dhi.io bases, the Rust build
//! stage with every download checked and its version stamps, the runtime posture and labels, and
//! the platforms. The checks are `repo_meta::image`'s, with this repository's parameters; the
//! negative controls run them on altered copies of the real files.

use repo_meta::dockerfile::{self, Stage};
use repo_meta::image::{
    BasePolicy, BuildBase, BuildStage, Download, RuntimeStage, STAMP_FIELDS, StampSources, Stamps,
    build_stage_problems, dockerfiles, label_problems, only_dockerfile_problems, platform_problems,
    runtime_problems, stage_problems, syntax_problems,
};
use repo_meta::toolchain::rust_release;
use repo_meta::{Problem, pattern, report, strings};

const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const SETUP_ZIGBUILD: &str = ".github/actions/setup-zigbuild/action.yaml";
const WORKFLOWS_DIR: &str = ".github/workflows";
/// The workflow whose `PLATFORMS` the image is built for.
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yaml";
const WANT_PLATFORMS: &str = "linux/amd64,linux/arm64";
/// dhi.io/static has no arm/v7, and no 386.
const FORBIDDEN_PLATFORMS: &[&str] = &["linux/arm/v", "linux/386"];
const STAGES: &[&str] = &["build", "runtime"];
const RUNTIME_USER: &str = "65532:65532";
const BINARY: &str = "/usr/local/bin/swarm-scheduler-exporter";

const STAMPS: Stamps<'static> = Stamps {
    prefix: "SWARM_SCHEDULER_EXPORTER",
    fields: &STAMP_FIELDS,
    version_file: "src/version.rs",
    build_command: "cargo zigbuild",
};

/// The runtime stage's HEALTHCHECK, timings included, as the Dockerfile's comment above it
/// explains them.
fn healthcheck() -> String {
    format!(
        r#"--interval=30s --timeout=5s --start-period=90s --retries=3 CMD ["{BINARY}", "--healthcheck"]"#
    )
}

fn shipped() -> String {
    meta::repo().read(SHIPPED_DOCKERFILE)
}

/// The stage named `name` of `content`, which must be the only one.
fn stage(content: &str, name: &str) -> Stage {
    let stages =
        dockerfile::parse(content).unwrap_or_else(|error| panic!("{SHIPPED_DOCKERFILE}: {error}"));
    dockerfile::stage_named(SHIPPED_DOCKERFILE, &stages, name)
        .cloned()
        .unwrap_or_else(|problem| panic!("{problem}"))
}

/// The value the setup-zigbuild action pins for `key`, such as `ZIG_VERSION`: CI's copy of the
/// release the build stage must download, so the build stage's rule does not read its own.
fn action_pin(key: &str) -> String {
    let action = meta::repo().read(SETUP_ZIGBUILD);
    pattern(&format!(r#"(?m)^\s*{key}:\s*"?([\d.]+)"?\s*$"#))
        .captures(&action)
        .map_or_else(
            || panic!("{SETUP_ZIGBUILD} pins no {key}"),
            |captures| captures[1].to_owned(),
        )
}

/// The arms a download must have, each `(platform, asset)` with the sha256 the Dockerfile's arm
/// for exactly that platform and asset writes. The pairs are this repository's; a digest that is
/// not a sha256, or an arm with another asset, leaves the arm unread, so the rule refuses it.
fn arms(
    dockerfile: &str,
    asset_var: &str,
    sha_var: &str,
    pairs: &[(&str, &str)],
) -> Vec<(String, String, String)> {
    pairs
        .iter()
        .map(|(platform, asset)| {
            let digest = pattern(&format!(
                r"{}\)\s*{asset_var}={}; {sha_var}=([0-9a-f]{{64}}) ;;",
                regex::escape(platform),
                regex::escape(asset)
            ))
            .captures(dockerfile)
            .map_or_else(
                || "<no sha256 arm>".to_owned(),
                |captures| captures[1].to_owned(),
            );
            ((*platform).to_owned(), (*asset).to_owned(), digest)
        })
        .collect()
}

/// The build stage's three downloads: zig and cargo-zigbuild for the build platform at the
/// releases CI pins too, and the target platform's musl standard library for the exact release
/// of rust-toolchain.toml; each in its own RUN, chosen by its platform argument's `case` with
/// exactly these arms, and checked with `sha256sum -c -`.
fn downloads(dockerfile: &str, release: &str) -> Vec<Download<'static>> {
    vec![
        Download {
            tool: "zig",
            url: "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz",
            platform_arg: "BUILDARCH",
            version_arg: "ZIG_VERSION",
            version: action_pin("ZIG_VERSION"),
            asset_var: "zig_arch",
            sha_var: "zig_sha256",
            arms: arms(
                dockerfile,
                "zig_arch",
                "zig_sha256",
                &[("amd64", "x86_64"), ("arm64", "aarch64")],
            ),
        },
        Download {
            tool: "cargo-zigbuild",
            url: "https://github.com/rust-cross/cargo-zigbuild/releases/download/v${CARGO_ZIGBUILD_VERSION}/cargo-zigbuild-${zigbuild_asset}.tar.xz",
            platform_arg: "BUILDARCH",
            version_arg: "CARGO_ZIGBUILD_VERSION",
            version: action_pin("CARGO_ZIGBUILD_VERSION"),
            asset_var: "zigbuild_asset",
            sha_var: "zigbuild_sha256",
            arms: arms(
                dockerfile,
                "zigbuild_asset",
                "zigbuild_sha256",
                &[
                    ("amd64", "x86_64-unknown-linux-musl"),
                    ("arm64", "aarch64-unknown-linux-gnu"),
                ],
            ),
        },
        Download {
            tool: "the musl rust-std",
            url: "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz",
            platform_arg: "TARGETARCH",
            version_arg: "RUST_VERSION",
            version: release.to_owned(),
            asset_var: "triple",
            sha_var: "std_sha256",
            arms: arms(
                dockerfile,
                "triple",
                "std_sha256",
                &[
                    ("amd64", "x86_64-unknown-linux-musl"),
                    ("arm64", "aarch64-unknown-linux-musl"),
                ],
            ),
        },
    ]
}

/// The exact release rust-toolchain.toml pins.
fn release() -> String {
    rust_release(
        "rust-toolchain.toml",
        &meta::repo().read("rust-toolchain.toml"),
    )
    .unwrap_or_else(|problem| panic!("{problem}"))
}

/// The build stage's rules against `dockerfile`: on the build platform, from the debian variant of
/// the exact toolchain release (whose toolchain takes the upstream musl standard libraries),
/// cross-compiled by cargo-zigbuild to the target platform's static musl triple from the locked
/// dependency graph, with every download a rule's and checked.
fn build_problems(dockerfile: &str) -> Vec<Problem> {
    let release = release();
    let base = format!("dhi.io/rust:{release}-debian13-dev");
    let downloads = downloads(dockerfile, &release);
    let rules = BuildStage {
        platform: "$BUILDPLATFORM",
        base: BuildBase::Exact(&base),
        required: &[
            r#"cargo zigbuild --release --locked --target "${triple}" --bin swarm-scheduler-exporter"#,
            r#"cp "target/${triple}/release/swarm-scheduler-exporter" /out/swarm-scheduler-exporter"#,
        ],
        downloads: &downloads,
    };
    build_stage_problems(&stage(dockerfile, "build"), &rules)
}

/// The runtime stage's rules: no shell to run anything in, nothing to write to, an unprivileged
/// numeric user, the healthcheck in exec form, the port, the entrypoint and the binary.
fn runtime_stage_problems(dockerfile: &str) -> Vec<Problem> {
    let entrypoint = format!(r#"["{BINARY}"]"#);
    let copy = format!("--from=build /out/swarm-scheduler-exporter {BINARY}");
    let healthcheck = healthcheck();
    let rules = RuntimeStage {
        repository: "dhi.io/static",
        forbidden: &["RUN", "SHELL", "VOLUME", "WORKDIR", "ADD", "ENV"],
        exactly: &[
            ("USER", RUNTIME_USER),
            ("EXPOSE", "8888"),
            ("HEALTHCHECK", &healthcheck),
            ("ENTRYPOINT", &entrypoint),
            ("COPY", &copy),
        ],
    };
    let runtime = stage(dockerfile, "runtime");
    let mut problems = runtime_problems(&runtime, &rules);
    problems.extend(label_problems(
        &runtime,
        &[
            (
                "org.opencontainers.image.title",
                Some("swarm-scheduler-exporter"),
            ),
            ("org.opencontainers.image.description", None),
            (
                "org.opencontainers.image.source",
                Some("https://github.com/leinardi/swarm-scheduler-exporter"),
            ),
            ("org.opencontainers.image.version", Some("${VERSION}")),
            ("org.opencontainers.image.revision", Some("${COMMIT}")),
            ("org.opencontainers.image.created", Some("${DATE}")),
            ("org.opencontainers.image.licenses", Some("MIT")),
        ],
    ));
    problems
}

/// The stamps of `dockerfile` and `makefile`, against src/version.rs and make-common's rust.mk.
fn stamp_problems(dockerfile: &str, makefile: &str) -> Vec<Problem> {
    let repo = meta::repo();
    let build = stage(dockerfile, "build");
    STAMPS.problems(&StampSources {
        version_rs: &repo.read("src/version.rs"),
        build: &build,
        makefile,
        rust_mk: &repo.read(".mk/rust.mk"),
        release: None,
    })
}

/// Every workflow, with its text.
fn workflows() -> Vec<(String, String)> {
    let repo = meta::repo();
    repo.read_all(&repo.files_under(WORKFLOWS_DIR, &[".yaml", ".yml"]))
}

/// Go: `TestShippedDockerfileIsTheOnlyOne`. deployments/docker holds exactly the image that
/// ships, and the repository no other Dockerfile (tracked or new, `Dockerfile.<name>` included):
/// a second one would look shipped to every reader and be built by nothing.
#[test]
fn shipped_dockerfile_is_the_only_one() {
    let problems = only_dockerfile_problems(&dockerfiles(&meta::repo()), SHIPPED_DOCKERFILE);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestSyntaxDirectiveIsPinned`. The `BuildKit` frontend that reads the Dockerfile is a
/// literal docker/dockerfile reference pinned by tag and digest: a bare docker/dockerfile:1.7 is
/// a tag that can be moved under a build, and Dependabot never updates the line
/// (docs/release.md).
#[test]
fn syntax_directive_is_pinned() {
    let problems = syntax_problems(SHIPPED_DOCKERFILE, &shipped(), true);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestShippedStages`. The two stages, in order, each from a literal dhi.io base pinned by
/// tag and not by digest: dhi.io republishes its tags with security fixes, and a rebuild should
/// pick those up (docs/release.md). A variable, latest or another registry would let a build
/// change base unseen.
#[test]
fn shipped_stages_are_build_and_runtime() {
    let problems = stage_problems(SHIPPED_DOCKERFILE, &shipped(), STAGES, BasePolicy::DhiOnly);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestBuildStage`, for the Rust build: on the build platform, cross-compiled by
/// cargo-zigbuild to the target platform's static musl triple, from the locked dependency graph,
/// with every toolchain download checked against its own digest in the RUN that fetches it.
#[test]
fn build_stage() {
    let problems = build_problems(&shipped());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestVersionIsStamped`. The version, commit and date the binary reports come from the
/// build: the image's build stage (in the RUN that runs cargo zigbuild) and the make targets set
/// the three variables src/version.rs reads at compile time, from the same build arguments and
/// git values. A path that stamped nothing would ship a binary reporting "dev".
#[test]
fn version_is_stamped() {
    let problems = stamp_problems(&shipped(), &meta::repo().read("Makefile"));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestRuntimeStage`. The posture of the image that ships: no shell to run anything in,
/// nothing to write to, an unprivileged numeric user, the healthcheck in exec form, and exactly
/// the seven OCI labels.
#[test]
fn runtime_stage() {
    let problems = runtime_stage_problems(&shipped());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestPlatforms`. The two platforms wherever a workflow names platforms, the release
/// workflow among them: dhi.io/static has no arm/v7.
#[test]
fn platforms() {
    let problems = platform_problems(
        &workflows(),
        RELEASE_WORKFLOW,
        WANT_PLATFORMS,
        FORBIDDEN_PLATFORMS,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// `content` with its first `from` replaced by `to`; `from` must be there, so an edit cannot
/// silently change nothing.
fn edit(content: &str, from: &str, to: &str) -> String {
    assert!(content.contains(from), "{from:?} is gone");
    content.replacen(from, to, 1)
}

/// A negative control of this repository's build stage rules: each edit of the real Dockerfile
/// below breaks one, and the rules refuse it.
#[test]
fn build_rules_refuse_each_edit() {
    let content = shipped();
    let edit = |from: &str, to: &str| edit(&content, from, to);
    let zig_check = "\t&& echo \"${zig_sha256}  /tmp/zig.tar.xz\" | sha256sum -c - \\\n";
    for (name, broken) in [
        (
            "an extra download beside a listed one",
            edit(
                zig_check,
                &format!("{zig_check}\t&& curl -fsSL -o /tmp/x https://example.invalid/x \\\n"),
            ),
        ),
        ("an unchecked download", edit(zig_check, "")),
        (
            "an arm for another platform",
            edit(
                "\t\t*) echo \"no zig for",
                "\t\tarm) zig_arch=arm; zig_sha256=0000000000000000000000000000000000000000000000000000000000000000 ;; \\\n\t\t*) echo \"no zig for",
            ),
        ),
        (
            "the alpine variant",
            edit("-debian13-dev AS build", "-alpine3.22-dev AS build"),
        ),
        (
            "an unlocked build",
            edit("--release --locked --target", "--release --target"),
        ),
    ] {
        assert!(!build_problems(&broken).is_empty(), "{name} passes");
    }
}

/// A negative control of the stamp rules: a stamp moved out of the building RUN, an undeclared
/// argument, and a Makefile whose winning prefix is another.
#[test]
fn stamp_rules_refuse_each_edit() {
    let content = shipped();
    let makefile = meta::repo().read("Makefile");
    let edit = |from: &str, to: &str| edit(&content, from, to);
    for (name, broken) in [
        (
            "a stamp outside the building RUN",
            edit("SWARM_SCHEDULER_EXPORTER_DATE=\"${DATE}\" \\\n", "\\\n").replacen(
                "WORKDIR /src\n",
                "WORKDIR /src\nENV SWARM_SCHEDULER_EXPORTER_DATE=\"${DATE}\"\n",
                1,
            ),
        ),
        (
            "an undeclared argument",
            edit("ARG DATE=\"unknown\"\n\n# The image's", "\n# The image's"),
        ),
    ] {
        assert!(
            !stamp_problems(&broken, &makefile).is_empty(),
            "{name} passes"
        );
    }
    let prefix = "RUST_STAMP_PREFIX := SWARM_SCHEDULER_EXPORTER";
    assert!(makefile.contains(prefix), "{prefix:?} is gone");
    for broken in [
        makefile.replacen(prefix, "RUST_STAMP_PREFIX := OTHER", 1),
        format!("{makefile}\nRUST_STAMP_PREFIX += X\n"),
    ] {
        assert!(!stamp_problems(&content, &broken).is_empty(), "{broken}");
    }
}

/// A negative control of the runtime rules: a shell, an extra label, the root user.
#[test]
fn runtime_rules_refuse_each_edit() {
    let content = shipped();
    let edit = |from: &str, to: &str| edit(&content, from, to);
    for (name, broken) in [
        (
            "a shell in the runtime stage",
            edit(
                "# Copy the binary\n",
                "SHELL [\"/bin/sh\", \"-c\"]\n# Copy the binary\n",
            ),
        ),
        (
            "an extra label",
            edit(
                "org.opencontainers.image.licenses=\"MIT\"",
                "org.opencontainers.image.licenses=\"MIT\" extra.label=\"x\"",
            ),
        ),
        (
            "root",
            content.replacen(&format!("USER {RUNTIME_USER}"), "USER 0:0", 1),
        ),
    ] {
        assert_ne!(broken, content, "{name} changed nothing");
        assert!(!runtime_stage_problems(&broken).is_empty(), "{name} passes");
    }
}

/// A negative control of the stage and platform rules: a base from another registry, and a third
/// platform in the release workflow.
#[test]
fn stage_and_platform_rules_refuse_each_edit() {
    assert!(
        !stage_problems(
            SHIPPED_DOCKERFILE,
            &edit(
                &shipped(),
                "FROM dhi.io/static:",
                "FROM docker.io/library/alpine:"
            ),
            STAGES,
            BasePolicy::DhiOnly,
        )
        .is_empty(),
        "a base from another registry passes"
    );

    let workflows = workflows();
    let release = workflows
        .iter()
        .position(|(file, _)| file == RELEASE_WORKFLOW)
        .unwrap_or_else(|| panic!("no {RELEASE_WORKFLOW}"));
    let mut broken = workflows.clone();
    broken[release].1 = broken[release].1.replacen(
        &format!("PLATFORMS: {WANT_PLATFORMS}"),
        "PLATFORMS: linux/amd64,linux/arm64,linux/arm/v7",
        1,
    );
    assert_ne!(broken, workflows, "the platform edit changed nothing");
    assert_eq!(
        strings(&platform_problems(
            &broken,
            RELEASE_WORKFLOW,
            WANT_PLATFORMS,
            FORBIDDEN_PLATFORMS,
        ))
        .len(),
        2,
        "a third platform"
    );
}

/// Go: `TestParseRejectsWhatTheAssertionsMustNotMiss`, the negative control for the reader: each
/// of these inputs has to come out as something the assertions above refuse.
#[test]
fn parse_rejects_what_the_assertions_must_not_miss() {
    let digest = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    for (name, image) in [
        ("a tag only", "dhi.io/static:20250419".to_owned()),
        (
            "a variable tag",
            format!("dhi.io/rust:${{RUST_LINE}}-debian13-dev{digest}"),
        ),
        ("a variable image", format!("$BASE{digest}")),
        (
            "a short digest",
            "dhi.io/static:20250419@sha256:0123".to_owned(),
        ),
        ("a digest with no tag", format!("dhi.io/static{digest}")),
        ("a stage name, not a ref", "build".to_owned()),
    ] {
        assert!(
            !dockerfile::digest_pinned(&image),
            "{name} ({image:?}) passes as digest-pinned"
        );
    }
    assert!(
        dockerfile::digest_pinned(&format!("dhi.io/static:20250419{digest}")),
        "a literal tag plus digest does not pass as digest-pinned"
    );

    for (name, image) in [
        ("no tag", "dhi.io/static".to_owned()),
        ("latest", "dhi.io/static:latest".to_owned()),
        (
            "a variable tag",
            "dhi.io/rust:${RUST_LINE}-debian13-dev".to_owned(),
        ),
        ("a variable image", "$BASE".to_owned()),
        (
            "a tag and a digest",
            format!("dhi.io/static:20250419{digest}"),
        ),
        ("a stage name, not a ref", "build".to_owned()),
    ] {
        assert!(
            !dockerfile::tag_pinned(&image),
            "{name} ({image:?}) passes as pinned by tag alone"
        );
    }
    assert!(
        dockerfile::tag_pinned("dhi.io/rust:1.98.1-debian13-dev"),
        "a literal tag does not pass as pinned by tag"
    );

    let stages =
        dockerfile::parse("FROM a:1 AS one\nRUN echo \\\n  two\n# note\nFROM b:2\nUSER 1:1\n")
            .expect("parse");
    assert!(
        stages.len() == 2
            && stages[0].name == "one"
            && stages[1].name.is_empty()
            && stages[0].joined() == "RUN echo two"
            && stages[1].joined() == "USER 1:1",
        "parsed {stages:?}"
    );

    for (name, content) in [
        ("no directive", "FROM a:1\n".to_owned()),
        (
            "after a comment",
            format!("# note\n# syntax=docker/dockerfile:1.7{digest}\n"),
        ),
        (
            "after a blank line",
            format!("\n# syntax=docker/dockerfile:1.7{digest}\n"),
        ),
        (
            "after an instruction",
            format!("FROM a:1\n# syntax=docker/dockerfile:1.7{digest}\n"),
        ),
        ("a directive with no value", "# syntax=\n".to_owned()),
    ] {
        let syntax = dockerfile::syntax(&content);
        assert!(
            syntax.is_none(),
            "{name}: read the syntax directive {syntax:?}, which BuildKit ignores"
        );
    }
    let syntax = dockerfile::syntax(&format!(
        "# check=error=true\n#Syntax = docker/dockerfile:1.7{digest}\n"
    ));
    assert_eq!(
        syntax,
        Some(format!("docker/dockerfile:1.7{digest}")),
        "a directive after another directive"
    );

    assert!(
        dockerfile::parse("FROM a:1\nRUN echo \\\n").is_err(),
        "a file ending inside a continuation parsed"
    );
}
