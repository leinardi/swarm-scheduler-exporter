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

//! The shipped image: the pinned frontend and the five stages. The frontend rule is
//! `repo_meta::image`'s; the stage rules are this repository's own, since repo-meta's `BasePolicy`
//! knows no internal stages, while two targets here share one base stage. Their negative controls
//! run them on altered copies of the real file. The `droast` hook enforces the label schema.

use repo_meta::dockerfile::{self, Stage};
use repo_meta::image::syntax_problems;
use repo_meta::{Problem, report};

const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
/// The stages, in order. The last one is the default target, so `runtime` must stay last.
const STAGES: &[&str] = &["build", "dist", "base", "prebuilt", "runtime"];
/// The external bases: each stage's dhi.io repository, pinned by an explicit tag alone: deliberately
/// not by digest, since dhi.io rebuilds its tags too often to follow by digest.
const EXTERNAL: &[(&str, &str)] = &[("build", "dhi.io/rust"), ("base", "dhi.io/static")];
/// The internal ones: the `dist` context's placeholder, and the two targets on the one base.
const INTERNAL: &[(&str, &str)] = &[
    ("dist", "scratch"),
    ("prebuilt", "base"),
    ("runtime", "base"),
];
/// What each target puts on the base: the release asset of the target platform, from the `dist`
/// context, or the build stage's binary, both where the entrypoint, the image tests and the
/// release read it.
const BINARIES: &[(&str, &str)] = &[
    (
        "prebuilt",
        "COPY --from=dist --chmod=0755 swarm-scheduler-exporter-${TARGETOS}-${TARGETARCH} /usr/local/bin/swarm-scheduler-exporter",
    ),
    (
        "runtime",
        "COPY --from=build /out/swarm-scheduler-exporter /usr/local/bin/swarm-scheduler-exporter",
    ),
];
/// A well-formed digest, for the negative controls that pin a base by one.
const ANY_DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ENTRYPOINT: &str = r#"ENTRYPOINT ["/usr/local/bin/swarm-scheduler-exporter"]"#;
/// The fixed non-root user, set in the base so both targets run as it.
const USER: &str = "USER 65532:65532";
/// The command of the health check, set in the base so both targets probe their own `/healthz`.
const HEALTHCHECK_CMD: &str = r#"CMD ["/usr/local/bin/swarm-scheduler-exporter", "--healthcheck"]"#;

fn shipped() -> String {
    meta::repo().read(SHIPPED_DOCKERFILE)
}

/// Go: `TestSyntaxDirectiveIsPinned`. The `BuildKit` frontend that reads the Dockerfile is a
/// literal docker/dockerfile reference pinned by tag and digest: a bare docker/dockerfile:1.7 is a
/// tag that can be moved under a build, and Dependabot never updates the line (docs/release.md).
#[test]
fn syntax_directive_is_pinned() {
    let problems = syntax_problems(SHIPPED_DOCKERFILE, &shipped(), true);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The one stage named `name`, or a problem.
fn named<'stages>(stages: &'stages [Stage], name: &str) -> Result<&'stages Stage, Problem> {
    dockerfile::stage_named(SHIPPED_DOCKERFILE, stages, name)
}

/// Whether `stage` has the instruction `line`, as [`Stage::joined`] writes it.
fn runs(stage: &Stage, line: &str) -> bool {
    stage.joined().lines().any(|joined| joined == line)
}

/// What is wrong with the stages of `content`: they must be exactly [`STAGES`], in order; each
/// external base a literal reference to its dhi.io repository by an explicit tag; each internal
/// stage from its stage; each target's binary copied where the entrypoint reads it; both targets
/// labelled alike (droast checks the default target's labels only); and the user, the health check
/// and the entrypoint set in the base, so both targets run them.
fn stage_problems(content: &str) -> Vec<Problem> {
    let stages = match dockerfile::parse(content) {
        Ok(stages) => stages,
        Err(error) => return vec![Problem::at(SHIPPED_DOCKERFILE, error.to_string())],
    };
    let mut problems = Vec::new();
    let found: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    if found != STAGES {
        problems.push(Problem::at(
            SHIPPED_DOCKERFILE,
            format!("has the stages {found:?}, want {STAGES:?}"),
        ));
    }
    for &(name, repository) in EXTERNAL {
        match named(&stages, name) {
            Ok(stage)
                if dockerfile::tag_pinned(&stage.image)
                    && dockerfile::repository(&stage.image) == repository => {}
            Ok(stage) => problems.push(Problem::at(
                SHIPPED_DOCKERFILE,
                format!(
                    "stage {name:?} is FROM {:?}, want a literal {repository}:<tag>, with no digest",
                    stage.image
                ),
            )),
            Err(problem) => problems.push(problem),
        }
    }
    for &(name, from) in INTERNAL {
        match named(&stages, name) {
            Ok(stage) if stage.image == from => {}
            Ok(stage) => problems.push(Problem::at(
                SHIPPED_DOCKERFILE,
                format!("stage {name:?} is FROM {:?}, want FROM {from}", stage.image),
            )),
            Err(problem) => problems.push(problem),
        }
    }
    for &(name, copy) in BINARIES {
        match named(&stages, name) {
            Ok(stage) if runs(stage, copy) => {}
            Ok(_) => problems.push(Problem::at(
                SHIPPED_DOCKERFILE,
                format!("stage {name:?} does not run {copy:?}"),
            )),
            Err(problem) => problems.push(problem),
        }
    }
    if let (Ok(prebuilt), Ok(runtime)) = (named(&stages, "prebuilt"), named(&stages, "runtime")) {
        let (labels, wanted) = (dockerfile::labels(prebuilt), dockerfile::labels(runtime));
        if wanted.is_empty() || labels != wanted {
            problems.push(Problem::at(
                SHIPPED_DOCKERFILE,
                format!("the prebuilt target's labels {labels:?} are not the runtime target's {wanted:?}"),
            ));
        }
    }
    match named(&stages, "base") {
        Ok(stage) => {
            for line in [ENTRYPOINT, USER] {
                if !runs(stage, line) {
                    problems.push(Problem::at(
                        SHIPPED_DOCKERFILE,
                        format!("stage \"base\" does not set {line}"),
                    ));
                }
            }
            let probes = stage.joined().lines().any(|joined| {
                joined.starts_with("HEALTHCHECK ") && joined.ends_with(HEALTHCHECK_CMD)
            });
            if !probes {
                problems.push(Problem::at(
                    SHIPPED_DOCKERFILE,
                    format!("stage \"base\" has no HEALTHCHECK that runs {HEALTHCHECK_CMD}"),
                ));
            }
        }
        Err(problem) => problems.push(problem),
    }
    problems
}

/// Every base is pinned by an explicit tag in one literal FROM, which Dependabot moves; not by
/// digest, on purpose (dhi.io rebuilds its tags too often), and every release build is
/// smoke-tested and scanned. A variable base, a missing tag or `latest` would leave the base
/// unreviewable. The two targets run on the one base stage, the prebuilt target with the
/// release asset as its binary, and the default target stays last.
#[test]
fn shipped_stages_are_pinned_and_share_the_base() {
    let problems = stage_problems(&shipped());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// `content` with its first `from` replaced by `to`; `from` must be there, so an edit cannot
/// silently change nothing.
fn edited(content: &str, from: &str, to: &str) -> String {
    assert!(content.contains(from), "{from:?} is gone");
    content.replacen(from, to, 1)
}

/// `image` without its digest.
fn without_digest(image: &str) -> &str {
    image.split_once('@').map_or(image, |(tag, _)| tag)
}

/// A negative control of the frontend rule: each edit of the real Dockerfile breaks it, and exactly
/// one problem comes back.
#[test]
fn frontend_rule_refuses_each_edit() {
    let content = shipped();
    let edit = |from: &str, to: &str| edited(&content, from, to);

    let syntax = dockerfile::syntax(&content).expect("the shipped Dockerfile has a frontend");
    for (name, broken) in [
        (
            "a frontend by tag alone",
            edit(&syntax, without_digest(&syntax)),
        ),
        (
            "another frontend",
            edit("docker/dockerfile:", "example/dockerfile:"),
        ),
        ("no frontend", edit(&format!("# syntax={syntax}\n"), "")),
    ] {
        let problems = syntax_problems(SHIPPED_DOCKERFILE, &broken, true);
        assert_eq!(problems.len(), 1, "{name}: {}", report(&problems));
    }
}

/// A negative control of the stage rules: each edit of the real Dockerfile breaks one rule, and
/// exactly one problem comes back.
#[test]
fn stage_rules_refuse_each_edit() {
    let content = shipped();
    let edit = |from: &str, to: &str| edited(&content, from, to);

    let stages = dockerfile::parse(&content).expect("the shipped Dockerfile parses");
    let base = named(&stages, "base").expect("a base stage").image.clone();
    let build = named(&stages, "build")
        .expect("a build stage")
        .image
        .clone();
    let repository = dockerfile::repository(&base);
    for (name, broken) in [
        (
            "a base by tag and digest",
            edit(&base, &format!("{base}{ANY_DIGEST}")),
        ),
        (
            "a base by digest alone",
            edit(&base, &format!("{repository}{ANY_DIGEST}")),
        ),
        ("a base without a tag", edit(&base, repository)),
        (
            "a base by latest",
            edit(&base, &format!("{repository}:latest")),
        ),
        (
            "a base from another repository",
            edit(&base, &base.replacen("dhi.io/static", "dhi.io/alpine", 1)),
        ),
        (
            "a build base by tag and digest",
            edit(&build, &format!("{build}{ANY_DIGEST}")),
        ),
        (
            "a variable base",
            edit(&format!("FROM {base}"), "FROM $BASE"),
        ),
        (
            "a stage after the default target",
            format!("{content}\nFROM base AS extra\n"),
        ),
        (
            "the prebuilt target on another stage",
            edit("FROM base AS prebuilt", "FROM build AS prebuilt"),
        ),
        (
            "a dist stage that is an image",
            edit("FROM scratch AS dist", &format!("FROM {base} AS dist")),
        ),
        (
            "the prebuilt binary from the build stage",
            edit(
                "COPY --from=dist --chmod=0755 swarm-scheduler-exporter-${TARGETOS}-${TARGETARCH}",
                "COPY --from=build --chmod=0755 /out/swarm-scheduler-exporter",
            ),
        ),
        (
            "the runtime binary elsewhere",
            edit(
                "COPY --from=build /out/swarm-scheduler-exporter /usr/local/bin/swarm-scheduler-exporter",
                "COPY --from=build /out/swarm-scheduler-exporter /swarm-scheduler-exporter",
            ),
        ),
        (
            "a prebuilt label of its own",
            edit(
                "org.opencontainers.image.title=\"swarm-scheduler-exporter\"",
                "org.opencontainers.image.title=\"prebuilt\"",
            ),
        ),
        (
            "a base from another registry",
            edit("FROM dhi.io/static:", "FROM docker.io/library/alpine:"),
        ),
        ("root", edit(USER, "USER 0:0")),
        (
            "no health check",
            edit(
                HEALTHCHECK_CMD,
                r#"CMD ["/usr/local/bin/swarm-scheduler-exporter"]"#,
            ),
        ),
        (
            "no entrypoint",
            edit(
                ENTRYPOINT,
                r#"CMD ["/usr/local/bin/swarm-scheduler-exporter"]"#,
            ),
        ),
    ] {
        let problems = stage_problems(&broken);
        assert_eq!(problems.len(), 1, "{name}: {}", report(&problems));
    }

    // A renamed default target is three problems: the stage list, and the runtime stage's base and
    // binary, which no stage holds any more.
    let problems = stage_problems(&edit("AS runtime", "AS final"));
    assert_eq!(problems.len(), 3, "a renamed stage: {}", report(&problems));
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
