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

//! The rust-common crates are pinned by release tag in the root `Cargo.toml`, one line each under
//! `[workspace.dependencies]`, and nothing in cargo checks that the lines agree: a tag-only bump
//! that leaves a `version` behind fails to resolve, and two tags lock two checkouts. The check is
//! `repo_meta::git_pins`; the negative controls run it on altered copies of the real manifest.

use repo_meta::git_pins::{RUST_COMMON_URL, git_pins, root_git_pins};
use repo_meta::{report, strings};

/// Every rust-common dependency is pinned by one `vX.Y.Z` tag, with `version` its `X.Y`.
#[test]
fn rust_common_pins_agree() {
    let problems = root_git_pins(&meta::repo(), RUST_COMMON_URL);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative controls: the real manifest passes, and each edit of it below breaks one rule.
#[test]
fn git_pins_rejects_each_divergence() {
    let text = meta::repo().read("Cargo.toml");
    let clean = strings(&git_pins(&[("Cargo.toml", &text)], RUST_COMMON_URL));
    assert_eq!(clean, Vec::<String>::new());

    let tag = text
        .lines()
        .find(|line| line.contains(RUST_COMMON_URL) && line.contains("tag = \""))
        .and_then(|line| line.split("tag = \"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .expect("a rust-common line with a tag")
        .to_owned();
    let version = tag
        .strip_prefix('v')
        .and_then(|rest| rest.rsplit_once('.'))
        .map(|(line, _)| line.to_owned())
        .expect("a vX.Y.Z tag");
    let tag_pin = format!("tag = \"{tag}\"");
    let version_pin = format!("version = \"{version}\"");
    assert!(text.contains(&tag_pin) && text.contains(&version_pin));
    // The first dependency line only: the comment above the table quotes the pin too.
    let once = |from: &str, to: &str| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let line = lines
            .iter_mut()
            .find(|line| line.contains(RUST_COMMON_URL) && line.contains(from))
            .expect("a rust-common line with the pin");
        *line = line.replacen(from, to, 1);
        lines.join("\n") + "\n"
    };

    for (name, altered, want) in [
        (
            "one line on a different tag",
            once(&tag_pin, &format!("tag = \"{tag}9\"")),
            "pin different tags",
        ),
        (
            "one version off its tag's major.minor",
            once(&version_pin, "version = \"0.0\""),
            "the version must be",
        ),
        (
            "one line pinned by a branch",
            once(&tag_pin, "branch = \"main\""),
            "instead of `tag`",
        ),
        (
            "a tag that is not vX.Y.Z",
            once(&tag_pin, "tag = \"latest\""),
            "not of the form vX.Y.Z",
        ),
        (
            "no rust-common dependency",
            text.replace(RUST_COMMON_URL, "https://example.invalid/other"),
            "no dependency on",
        ),
    ] {
        assert_ne!(altered, text, "{name}: the edit changed nothing");
        let problems = strings(&git_pins(&[("Cargo.toml", &altered)], RUST_COMMON_URL));
        assert!(
            problems.iter().any(|problem| problem.contains(want)),
            "{name}: want a problem containing {want:?}, got {problems:?}"
        );
    }
}
