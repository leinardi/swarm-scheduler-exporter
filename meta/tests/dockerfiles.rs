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
//! one Dockerfile, a pinned frontend, two stages from tag-pinned bases, the Rust build stage and
//! its version stamps, the runtime posture and labels, and the platforms.

use std::collections::BTreeMap;
use std::fs;

use meta::dockerfile::{self, Stage};
use meta::repo;
use regex::Regex;

const SHIPPED_DIR: &str = "deployments/docker";
const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const WORKFLOWS_DIR: &str = ".github/workflows";
const WANT_PLATFORMS: &str = "linux/amd64,linux/arm64";
const RUNTIME_USER: &str = "65532:65532";
const BINARY: &str = "/usr/local/bin/swarm-scheduler-exporter";

/// The runtime stage's HEALTHCHECK, timings included, as the Dockerfile's comment above it
/// explains them.
fn healthcheck() -> String {
    format!(
        r#"--interval=30s --timeout=5s --start-period=90s --retries=3 CMD ["{BINARY}", "--healthcheck"]"#
    )
}

fn shipped_stages() -> Vec<Stage> {
    let stages = dockerfile::parse(&repo::read(SHIPPED_DOCKERFILE))
        .unwrap_or_else(|error| panic!("{SHIPPED_DOCKERFILE}: {error}"));
    assert_eq!(
        stages.len(),
        2,
        "{SHIPPED_DOCKERFILE} has {} stages, want build and runtime",
        stages.len()
    );
    stages
}

fn is_dockerfile(name: &str) -> bool {
    name == "Dockerfile" || name.ends_with(".Dockerfile")
}

/// Every Dockerfile in the repository, skipping installed and generated trees: hidden
/// directories (.git, the tool caches, a nested worktree under .claude/worktrees, whose
/// Dockerfile is not this tree's), dist, vendor and cargo's target. Walking rather than asking
/// git finds an untracked one too.
fn all_dockerfiles() -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(directory) = pending.pop() {
        let path = if directory.is_empty() {
            repo::root()
        } else {
            repo::path(&directory)
        };
        let entries =
            fs::read_dir(&path).unwrap_or_else(|error| panic!("list {directory:?}: {error}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("list {directory:?}: {error}"));
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("inspect {relative}: {error}"));
            if kind.is_dir() {
                if !name.starts_with('.') && !matches!(name.as_str(), "dist" | "vendor" | "target")
                {
                    pending.push(relative);
                }
            } else if is_dockerfile(&name) {
                found.push(relative);
            }
        }
    }
    assert!(
        found.iter().any(|path| path == SHIPPED_DOCKERFILE),
        "the walk did not find {SHIPPED_DOCKERFILE}, so it would find nothing else either"
    );
    found.sort();
    found
}

/// Go: `TestShippedDockerfileIsTheOnlyOne`. deployments/docker holds exactly the image that
/// ships, and the repository no other Dockerfile: a second one would look shipped to every
/// reader and be built by nothing.
#[test]
fn shipped_dockerfile_is_the_only_one() {
    let mut found: Vec<String> = fs::read_dir(repo::path(SHIPPED_DIR))
        .expect("list deployments/docker")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| is_dockerfile(name))
        .collect();
    found.sort();
    assert_eq!(found, ["Dockerfile"], "{SHIPPED_DIR} holds {found:?}");
    assert_eq!(
        all_dockerfiles(),
        [SHIPPED_DOCKERFILE],
        "the repository holds other Dockerfiles"
    );
}

/// Go: `TestSyntaxDirectiveIsPinned`. The `BuildKit` frontend that reads the Dockerfile is a
/// literal reference pinned by digest: a bare docker/dockerfile:1.7 is a tag that can be moved
/// under a build, and Dependabot never updates the line (docs/release.md).
#[test]
fn syntax_directive_is_pinned() {
    let syntax = dockerfile::syntax(&repo::read(SHIPPED_DOCKERFILE))
        .expect("the Dockerfile has a syntax parser directive on its first line");
    assert_eq!(
        dockerfile::repository(&syntax),
        "docker/dockerfile",
        "{syntax}"
    );
    assert!(
        dockerfile::digest_pinned(&syntax),
        "the frontend {syntax:?} is not pinned by digest"
    );
}

/// Go: `TestShippedStages`. The two stages, in order, each from a literal base pinned by tag and
/// not by digest: dhi.io republishes its tags with security fixes, and a rebuild should pick
/// those up (docs/release.md). A variable or latest would let a build change base unseen.
#[test]
fn shipped_stages_are_build_and_runtime() {
    let stages = shipped_stages();
    for stage in &stages {
        assert!(
            dockerfile::tag_pinned(&stage.image),
            "stage {:?} is FROM {:?}, which is not a literal reference pinned by tag alone",
            stage.name,
            stage.image
        );
    }
    let names: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    assert_eq!(names, ["build", "runtime"]);
}

/// The stage named `name`, which must be the only one.
fn stage(name: &str) -> Stage {
    let found: Vec<Stage> = shipped_stages()
        .into_iter()
        .filter(|candidate| candidate.name == name)
        .collect();
    match <[Stage; 1]>::try_from(found) {
        Ok([stage]) => stage,
        Err(found) => panic!(
            "{SHIPPED_DOCKERFILE} has {} stages named {name}, want one",
            found.len()
        ),
    }
}

/// Go: `TestBuildStage`, for the Rust build: on the build platform, cross-compiled by
/// cargo-zigbuild to the target platform's static musl triple, from the locked dependency graph,
/// with every toolchain download checked, and stamped with the version strings the binary reads.
#[test]
fn build_stage() {
    let build = stage("build");
    assert_eq!(build.platform, "$BUILDPLATFORM");
    assert_eq!(dockerfile::repository(&build.image), "dhi.io/rust");

    let instructions = build.joined();
    let missing: Vec<&str> = [
        "ARG VERSION",
        "ARG COMMIT",
        "ARG DATE",
        "ARG RUST_VERSION=",
        "ARG ZIG_VERSION=",
        "ARG CARGO_ZIGBUILD_VERSION=",
        "ARG BUILDARCH",
        "ARG TARGETARCH",
        "amd64) triple=x86_64-unknown-linux-musl;",
        "arm64) triple=aarch64-unknown-linux-musl;",
        "sha256sum -c -",
        "/releases/download/v${CARGO_ZIGBUILD_VERSION}/cargo-zigbuild-${zigbuild_asset}.tar.xz",
        r#"cargo zigbuild --release --locked --target "${triple}" --bin swarm-scheduler-exporter"#,
        r#"cp "target/${triple}/release/swarm-scheduler-exporter" /out/swarm-scheduler-exporter"#,
    ]
    .into_iter()
    .filter(|want| !instructions.contains(want))
    .collect();
    assert!(
        missing.is_empty(),
        "the build stage lacks {missing:?}:\n{instructions}"
    );

    // Every download is checked against its own digest: each curl in the stage is followed by a
    // sha256sum check of the file it wrote.
    let downloads = instructions.matches("curl -fsSL").count();
    let checks = instructions.matches("| sha256sum -c -").count();
    assert!(downloads > 0, "the build stage downloads nothing");
    assert_eq!(
        downloads, checks,
        "a download in the build stage is not checked"
    );
}

/// Go: `TestVersionIsStamped`. The version, commit and date the binary reports come from the
/// build: the image's build stage and the make targets set the three variables src/version.rs
/// reads at compile time, from the same build arguments and git values. A path that stamped
/// nothing would ship a binary reporting "dev".
#[test]
fn version_is_stamped() {
    let fields = ["VERSION", "COMMIT", "DATE"];
    let version_rs = repo::read("src/version.rs");
    let build = stage("build").joined();
    let make = repo::read(".mk/rust.mk");
    let mut problems = Vec::new();
    for field in fields {
        let variable = format!("SWARM_SCHEDULER_EXPORTER_{field}");
        if !version_rs.contains(&format!("option_env!(\x22{variable}\x22)")) {
            problems.push(format!("src/version.rs does not read {variable}"));
        }
        if !build.contains(&format!("{variable}=\x22${{{field}}}\x22")) {
            problems.push(format!(
                "the build stage does not stamp {variable} from ${field}"
            ));
        }
        if !make.contains(&format!("{variable}=")) {
            problems.push(format!(".mk/rust.mk does not stamp {variable}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Go: `TestRuntimeStage`. The posture of the image that ships: no shell to run anything in,
/// nothing to write to, an unprivileged numeric user, the healthcheck in exec form, and the seven
/// OCI labels.
#[test]
fn runtime_stage() {
    let runtime = shipped_stages().swap_remove(1);
    assert_eq!(
        dockerfile::repository(&runtime.image),
        "dhi.io/static",
        "the runtime stage's base"
    );
    assert!(
        runtime.platform.is_empty(),
        "the runtime stage pins --platform={}; it has to follow the target platform",
        runtime.platform
    );

    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for instruction in &runtime.instructions {
        assert!(
            !matches!(
                instruction.keyword.as_str(),
                "RUN" | "SHELL" | "VOLUME" | "WORKDIR" | "ADD" | "ENV"
            ),
            "the runtime stage carries {} {}; it has no shell and writes nothing",
            instruction.keyword,
            instruction.args
        );
        seen.entry(instruction.keyword.as_str())
            .or_default()
            .push(instruction.args.as_str());
    }
    let entrypoint = format!(r#"["{BINARY}"]"#);
    let copy = format!("--from=build /out/swarm-scheduler-exporter {BINARY}");
    let healthcheck = healthcheck();
    for (keyword, args) in [
        ("USER", RUNTIME_USER),
        ("EXPOSE", "8888"),
        ("HEALTHCHECK", healthcheck.as_str()),
        ("ENTRYPOINT", entrypoint.as_str()),
        ("COPY", copy.as_str()),
    ] {
        assert_eq!(
            seen.get(keyword).map(Vec::as_slice),
            Some([args].as_slice()),
            "the runtime stage's {keyword}, want exactly {keyword} {args}"
        );
    }

    let labels = dockerfile::labels(&runtime);
    let description = labels
        .get("org.opencontainers.image.description")
        .cloned()
        .unwrap_or_default();
    assert!(
        !description.is_empty(),
        "the runtime stage has no org.opencontainers.image.description"
    );
    let want: BTreeMap<String, String> = [
        ("org.opencontainers.image.title", "swarm-scheduler-exporter"),
        ("org.opencontainers.image.description", description.as_str()),
        (
            "org.opencontainers.image.source",
            "https://github.com/leinardi/swarm-scheduler-exporter",
        ),
        ("org.opencontainers.image.version", "${VERSION}"),
        ("org.opencontainers.image.revision", "${COMMIT}"),
        ("org.opencontainers.image.created", "${DATE}"),
        ("org.opencontainers.image.licenses", "MIT"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(labels, want, "want exactly the seven OCI labels");
}

/// Go: `TestPlatforms`. The two platforms wherever a workflow names platforms: dhi.io/static has
/// no arm/v7.
#[test]
fn platforms() {
    let declaration =
        Regex::new(r"(?mi)^\s*(?:-\s*)?(platforms?|PLATFORMS)\s*:\s*(.+)$").expect("valid pattern");
    let mut declared = 0;
    let mut problems = Vec::new();
    for workflow in repo::files_under(WORKFLOWS_DIR, &[".yaml", ".yml"]) {
        let content = repo::read(&workflow);
        for captures in declaration.captures_iter(&content) {
            let value = captures[2].trim().trim_matches(['"', '\'']);
            if value.starts_with("${{") {
                continue;
            }
            declared += 1;
            if value != WANT_PLATFORMS {
                problems.push(format!(
                    "{workflow} declares {} {value:?}, want {WANT_PLATFORMS:?}",
                    &captures[1]
                ));
            }
        }
        if content.contains("linux/arm/v") || content.contains("linux/386") {
            problems.push(format!(
                "{workflow} names a platform other than {WANT_PLATFORMS}"
            ));
        }
    }
    assert!(
        declared > 0,
        "no workflow declares its platforms; the check would pass on nothing"
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
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
