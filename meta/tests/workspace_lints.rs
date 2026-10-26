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

use std::fs;
use std::path::{Path, PathBuf};

use toml::Table;

fn repo_root() -> PathBuf {
    // The meta crate sits one level below the repository root. A plain join rather than parent():
    // clippy's allow-expect-in-tests covers #[test] functions, not the helpers beside them.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read_manifest(path: &Path) -> Table {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    text.parse::<Table>()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

#[test]
fn root_manifest_declares_workspace_lints() {
    let root = read_manifest(&repo_root().join("Cargo.toml"));
    let lints = root
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .and_then(toml::Value::as_table)
        .expect("the root Cargo.toml has a [workspace.lints] table");

    assert!(
        lints.contains_key("rust"),
        "[workspace.lints.rust] is missing"
    );
    assert!(
        lints.contains_key("clippy"),
        "[workspace.lints.clippy] is missing"
    );
}

#[test]
fn every_workspace_member_opts_into_the_workspace_lints() {
    let root_dir = repo_root();
    let root = read_manifest(&root_dir.join("Cargo.toml"));
    let members = root
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .expect("the root Cargo.toml lists [workspace] members");

    assert!(!members.is_empty(), "the workspace lists no members");

    for member in members {
        let member = member
            .as_str()
            .expect("a workspace member is a path string");
        assert!(
            !member.contains('*'),
            "member {member:?} is a glob; list members explicitly so this check sees each one"
        );

        let manifest_path = root_dir.join(member).join("Cargo.toml");
        let manifest = read_manifest(&manifest_path);
        let opts_in = manifest
            .get("lints")
            .and_then(|lints| lints.get("workspace"))
            .and_then(toml::Value::as_bool);

        assert_eq!(
            opts_in,
            Some(true),
            "{} does not declare [lints] workspace = true",
            manifest_path.display()
        );
    }
}
