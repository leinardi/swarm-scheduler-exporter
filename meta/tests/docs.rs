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

//! The offline documentation check: every relative link and fragment in the tracked Markdown
//! resolves. The docs-links pre-commit hook runs this file. The checker is
//! `repo_meta::links`'s.

use repo_meta::links::{Checker, check_repository, slug};
use repo_meta::scratch::ScratchRepo;
use repo_meta::{report, strings};

/// Go: `TestRelativeLinksResolve`. Every relative link and fragment in the tracked Markdown. The
/// list comes from git, which keeps an untracked scratch file out, and a listing with no Markdown
/// is a problem, not a pass on nothing.
#[test]
fn relative_links_resolve() {
    let problems = check_repository(&meta::repo());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestCheckerFindsBrokenLinks`, the negative control: the link check is not blind, on a
/// scratch tree.
#[test]
fn checker_finds_broken_links() {
    let scratch = ScratchRepo::new();
    scratch.write(
        "a.md",
        &[
            "# Title",
            "## 1. Section — `GET /x`",
            "[ok](b.md#heading-two) [ok](#1-section--get-x) [ok](https://example.com/missing)",
            "[broken file](missing.md) [broken anchor](b.md#nope) `[in code](missing-too.md)`",
            "```text",
            "[in a fence](missing-three.md)",
            "```",
        ]
        .join("\n"),
    );
    scratch.write("b.md", "# Heading Two\n");

    assert_eq!(
        strings(&Checker::new(scratch.path()).check_file("a.md")),
        [
            r#"a.md:4: link "missing.md": missing.md does not exist"#,
            r#"a.md:4: link "b.md#nope": b.md has no heading or anchor #nope"#,
        ]
    );
}

/// A Markdown file that cannot be read is a problem, not a pass on nothing.
#[test]
fn an_unreadable_file_is_a_problem() {
    let scratch = ScratchRepo::new();
    let problems = Checker::new(scratch.path()).check_file("gone.md");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "gone.md");
}

/// The repository check refuses a repository whose git lists no Markdown at all.
#[test]
fn no_markdown_is_a_problem() {
    let scratch = ScratchRepo::new();
    scratch.write("a.txt", "no Markdown here\n");
    scratch.git(&["add", "a.txt"]);
    assert_eq!(
        strings(&check_repository(&scratch.repo())),
        ["git lists no Markdown file; the link check would pass on nothing"]
    );
}

/// Go: `TestSlug`. The GitHub slug rules the anchors follow.
#[test]
fn slugs_follow_github() {
    for (heading, want) in [
        ("1. Test (Home) — `/`", "1-test-home--"),
        ("5. `GET /api/v1/info`", "5-get-apiv1info"),
        ("5.1 Implementation", "51-implementation"),
        (
            "Why the Build Happens Before the Tag",
            "why-the-build-happens-before-the-tag",
        ),
        (
            "11.2 Path A fallback — HTTP probe",
            "112-path-a-fallback--http-probe",
        ),
        ("5. History Module (`idb`)", "5-history-module-idb"),
        (
            "**Bold** and _emphasis_ with snake_case",
            "bold-and-emphasis-with-snake_case",
        ),
        ("[Linked](other.md) heading", "linked-heading"),
        ("2.2 Stateless Server", "22-stateless-server"),
        ("Tags, Moves and `X.Y.Z`", "tags-moves-and-xyz"),
        ("Ünïcode Lëtters Stay", "ünïcode-lëtters-stay"),
        (
            "Required Checks and Fork Pull Requests",
            "required-checks-and-fork-pull-requests",
        ),
        (
            "6. `compat.networkquality.max_upload_bytes`",
            "6-compatnetworkqualitymax_upload_bytes",
        ),
        (
            "💡 What is “Swarm Scheduler Exporter”?",
            "-what-is-swarm-scheduler-exporter",
        ),
    ] {
        assert_eq!(slug(heading), want, "{heading:?}");
    }
}
