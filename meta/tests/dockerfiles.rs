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
//! a pinned `BuildKit` frontend, and two stages from tag-pinned dhi.io bases. The checks are
//! `repo_meta::image`'s, with this repository's parameters; the negative controls run them on
//! altered copies of the real files.

use repo_meta::dockerfile;
use repo_meta::image::{BasePolicy, stage_problems, syntax_problems};
use repo_meta::report;

const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const STAGES: &[&str] = &["build", "runtime"];

fn shipped() -> String {
    meta::repo().read(SHIPPED_DOCKERFILE)
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

/// `content` with its first `from` replaced by `to`; `from` must be there, so an edit cannot
/// silently change nothing.
fn edit(content: &str, from: &str, to: &str) -> String {
    assert!(content.contains(from), "{from:?} is gone");
    content.replacen(from, to, 1)
}

/// A negative control of the stage rules: a base from another registry.
#[test]
fn stage_rules_refuse_another_registry() {
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
