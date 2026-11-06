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

//! The shared lint set is declared once, in the root `[workspace.lints]`, and a package only obeys
//! it when its own manifest opts in: `[lints]` is not inherited by workspace members. A member that
//! forgets `[lints] workspace = true` builds and passes clippy with the default lints, silently.
//! The check is `repo_meta::workspace::lint_opt_in`; the negative controls run it on scratch
//! repositories.

use repo_meta::scratch::ScratchRepo;
use repo_meta::workspace::lint_opt_in;
use repo_meta::{report, strings};

/// The root manifest declares `[workspace.lints.rust]` and `[workspace.lints.clippy]`, lists its
/// members explicitly (no glob), and every member declares `[lints] workspace = true`.
#[test]
fn every_workspace_member_opts_into_the_workspace_lints() {
    let problems = lint_opt_in(&meta::repo(), None);
    assert!(problems.is_empty(), "{}", report(&problems));
}

const ROOT: &str = "[workspace]\nmembers = [\".\", \"meta\"]\n\n[workspace.lints.rust]\n\
                    unsafe_code = \"forbid\"\n\n[workspace.lints.clippy]\nall = \"deny\"\n\n\
                    [lints]\nworkspace = true\n";
const MEMBER: &str = "[package]\nname = \"meta\"\n\n[lints]\nworkspace = true\n";

/// The lint opt-in of a scratch workspace whose root manifest is `root` and whose `meta` member's
/// is `member`.
fn scratch_problems(root: &str, member: &str) -> Vec<String> {
    let scratch = ScratchRepo::new();
    scratch.write("Cargo.toml", root);
    scratch.write("meta/Cargo.toml", member);
    strings(&lint_opt_in(&scratch.repo(), None))
}

/// The negative controls: each manifest below breaks one rule, and the clean workspace passes.
#[test]
fn lint_opt_in_rejects_each_escape() {
    assert_eq!(scratch_problems(ROOT, MEMBER), Vec::<String>::new());
    for (name, root, member, want) in [
        (
            "a member without the opt-in",
            ROOT,
            "[package]\nname = \"meta\"\n",
            "meta/Cargo.toml",
        ),
        (
            "the root package without the opt-in",
            &*ROOT.replace("[lints]\nworkspace = true\n", ""),
            MEMBER,
            "Cargo.toml",
        ),
        (
            "a glob member",
            &*ROOT.replace("\"meta\"", "\"crates/*\""),
            MEMBER,
            "is a glob",
        ),
        (
            "no clippy lints",
            &*ROOT.replace("[workspace.lints.clippy]\nall = \"deny\"\n", ""),
            MEMBER,
            "[workspace.lints.clippy] is missing",
        ),
        (
            "no workspace lints",
            "[workspace]\nmembers = [\"meta\"]\n",
            MEMBER,
            "no [workspace.lints] table",
        ),
    ] {
        let problems = scratch_problems(root, member);
        assert!(
            problems.iter().any(|problem| problem.contains(want)),
            "{name}: want a problem containing {want:?}, got {problems:?}"
        );
    }
}
