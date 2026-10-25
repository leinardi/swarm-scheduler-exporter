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

//! Drives scripts/check-license-headers.sh: on this repository, and on scratch repositories that
//! hold each kind of file it must reject or let through.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use meta::{gitenv, repo};

const SCRIPT_PATH: &str = "scripts/check-license-headers.sh";

/// A Go file carrying the lines the check looks for; the last one is its eighth line.
const GO_HEADER: &str = r#"/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * The above copyright notice and this permission notice shall be included in all
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 */

package x
"#;

/// Another licence's notice with the right copyright line.
const APACHE_HEADER: &str = r#"/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 */

package x
"#;

const SHELL_HEADER: &str = r#"#!/usr/bin/env bash
#
# MIT License
#
# Copyright (c) 2025-2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# The above copyright notice and this permission notice shall be included in all
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
echo ok
"#;

const MAKEFILE: &str = "MK_COMMON_FILES       := docker.mk help.mk\n";

/// Scratch files under this prefix are written but never added: the check reads tracked files
/// only.
const UNTRACKED_PREFIX: &str = "untracked/";

/// The hash-comment header without the shebang, as a Makefile or a Dockerfile carries it.
fn hash_header() -> &'static str {
    SHELL_HEADER
        .strip_prefix("#!/usr/bin/env bash\n")
        .unwrap_or(SHELL_HEADER)
}

fn run_git(directory: &Path, args: &[&str]) {
    let output = gitenv::isolate(Command::new("git").args(args).current_dir(directory))
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A git repository holding the checker, a Makefile (unless the case brings its own) and `files`,
/// every one of them added except those under [`UNTRACKED_PREFIX`].
fn scratch_repo(files: &[(&str, String)]) -> tempfile::TempDir {
    let scratch = tempfile::tempdir().unwrap_or_else(|error| panic!("scratch directory: {error}"));
    let directory = scratch.path();
    let mut files = files.to_vec();
    files.push((SCRIPT_PATH, repo::read(SCRIPT_PATH)));
    if !files.iter().any(|(name, _)| *name == "Makefile") {
        files.push(("Makefile", format!("{}{MAKEFILE}", hash_header())));
    }

    for (name, content) in &files {
        let path = directory.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create the directory of {name}: {error}"));
        }
        fs::write(&path, content).unwrap_or_else(|error| panic!("write {name}: {error}"));
    }
    fs::set_permissions(
        directory.join(SCRIPT_PATH),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap_or_else(|error| panic!("make the checker executable: {error}"));

    run_git(directory, &["init", "--quiet"]);
    for (name, _) in &files {
        if !name.starts_with(UNTRACKED_PREFIX) {
            run_git(directory, &["add", "--", name]);
        }
    }
    scratch
}

/// Runs the checker kept at [`SCRIPT_PATH`] inside the repository at `directory`.
fn run_checker(directory: &Path) -> Output {
    gitenv::isolate(Command::new(directory.join(SCRIPT_PATH)).current_dir(directory))
        .output()
        .unwrap_or_else(|error| panic!("run the checker: {error}"))
}

/// Go: `TestRepositoryHeaders`. The repository passes its own check. Run with the repository's
/// own git environment: under the commit hook it names the index being committed.
#[test]
fn repository_headers() {
    let output = Command::new(repo::path(SCRIPT_PATH))
        .current_dir(repo::root())
        .output()
        .expect("run the checker");
    assert!(
        output.status.success(),
        "the repository fails its own license header check:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One scratch repository: its files, and whether the checker must accept it.
struct HeaderCase {
    name: &'static str,
    files: Vec<(&'static str, String)>,
    pass: bool,
}

fn case(name: &'static str, files: &[(&'static str, &str)], pass: bool) -> HeaderCase {
    HeaderCase {
        name,
        files: files
            .iter()
            .map(|(path, content)| (*path, (*content).to_owned()))
            .collect(),
        pass,
    }
}

fn run_cases(cases: Vec<HeaderCase>) {
    let mut failures = Vec::new();
    for HeaderCase { name, files, pass } in cases {
        let scratch = scratch_repo(&files);
        let output = run_checker(scratch.path());
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if pass && !output.status.success() {
            failures.push(format!("{name}: rejected, want accepted:\n{text}"));
        }
        if !pass && output.status.success() {
            failures.push(format!("{name}: accepted, want rejected:\n{text}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Go: `TestHeaderCases`, the notice on Go and Rust files: present, after a build constraint,
/// missing, another licence, another holder, a testdata fixture, and the edges of the window it
/// must sit in. `GO_HEADER`'s last required line is its eighth: 22 lines before it put it on
/// line 30, the last line of the window, and one more pushes it out.
#[test]
fn header_cases_for_the_notice() {
    let other_holder = GO_HEADER.replace("Roberto Leinardi", "Someone Else");
    let rust = GO_HEADER.replacen("package x\n", "fn x() {}\n", 1);
    let constrained = format!("//go:build integration\n\n{GO_HEADER}");
    let at_the_edge = format!("{}{GO_HEADER}", "// filler\n".repeat(22));
    let past_the_edge = format!("{}{GO_HEADER}", "// filler\n".repeat(23));
    run_cases(vec![
        case(
            "go file with the header",
            &[("internal/x/x.go", GO_HEADER)],
            true,
        ),
        case(
            "go file after a build constraint",
            &[("internal/x/x.go", &constrained)],
            true,
        ),
        case(
            "go file without a header",
            &[("internal/x/x.go", "package x\n")],
            false,
        ),
        case(
            "another licence's header",
            &[("internal/x/x.go", APACHE_HEADER)],
            false,
        ),
        case(
            "another copyright holder",
            &[("internal/x/x.go", &other_holder)],
            false,
        ),
        // Rust carries the same /* */ block as Go.
        case("rust file with the header", &[("src/x.rs", &rust)], true),
        case(
            "rust file without a header",
            &[("src/x.rs", "fn x() {}\n")],
            false,
        ),
        case(
            "rust file with another copyright holder",
            &[("meta/src/x.rs", &other_holder)],
            false,
        ),
        case(
            "rust testdata fixture",
            &[("tests/testdata/fixture.rs", "fn x() {}\n")],
            true,
        ),
        case(
            "header ending on the last line of the window",
            &[("internal/x/x.go", &at_the_edge)],
            true,
        ),
        case(
            "header ending one line past the window",
            &[("internal/x/x.go", &past_the_edge)],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, every other governed file kind with and without the header.
#[test]
fn header_cases_for_each_file_kind() {
    let dockerfile = format!("# syntax=docker/dockerfile:1.7\n{}", hash_header());
    run_cases(vec![
        case(
            "shell script with the header",
            &[("scripts/a.sh", SHELL_HEADER)],
            true,
        ),
        case(
            "shell script without a header",
            &[("scripts/a.sh", "#!/usr/bin/env bash\necho\n")],
            false,
        ),
        case(
            "dockerfile with the header after the syntax line",
            &[("deployments/docker/Dockerfile", &dockerfile)],
            true,
        ),
        case(
            "dockerfile without a header",
            &[("deployments/docker/Dockerfile", "FROM scratch\n")],
            false,
        ),
        case(
            "local make fragment, no header",
            &[(".mk/verify.mk", "x:\n")],
            false,
        ),
        case(
            "rust make fragment, no header",
            &[(".mk/rust.mk", "x:\n")],
            false,
        ),
        case(
            "makefile without a header",
            &[("Makefile", MAKEFILE)],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, what is never governed: untracked files, make-common's files, a
/// testdata fixture, and Markdown, YAML and JSON.
#[test]
fn header_cases_for_the_exemptions() {
    run_cases(vec![
        case(
            "untracked file without a header",
            &[("untracked/scratch.go", "package x\n")],
            true,
        ),
        case(
            "upstream make fragment",
            &[(".mk/docker.mk", "x:\n"), (".mk/help.mk", "y:\n")],
            true,
        ),
        case(
            "bootstrap script",
            &[("scripts/bootstrap-mk-common.sh", "#!/bin/sh\n")],
            true,
        ),
        case(
            "testdata fixture",
            &[("internal/x/testdata/fixture.go", "package fixture\n")],
            true,
        ),
        case(
            "markdown, yaml and json",
            &[
                ("README.md", "# x\n"),
                ("a.yaml", "a: 1\n"),
                ("b.json", "{}\n"),
            ],
            true,
        ),
    ]);
}
