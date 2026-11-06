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

//! Drives scripts/check-license-headers.sh through `repo_meta::scratch::HeaderHarness`: on this
//! repository, and on scratch repositories that hold each kind of file it must reject or let
//! through. Each set of cases holds one the checker must accept and one it must reject.

use repo_meta::report;
use repo_meta::scratch::{HeaderCase, HeaderHarness};

/// A Rust file carrying the lines the check looks for; the last one is its eighth line.
const RUST_HEADER: &str = r#"/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * The above copyright notice and this permission notice shall be included in all
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 */

fn x() {}
"#;

/// Another licence's notice with the right copyright line.
const APACHE_HEADER: &str = r#"/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 */

fn x() {}
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

const MAKEFILE: &str = "MK_COMMON_FILES       := docker.mk help.mk rust.mk\n";

/// The hash-comment header without the shebang, as a Makefile or a Dockerfile carries it.
fn hash_header() -> &'static str {
    SHELL_HEADER
        .strip_prefix("#!/usr/bin/env bash\n")
        .unwrap_or(SHELL_HEADER)
}

/// The Makefile every scratch repository holds unless a case brings its own: the checker reads the
/// make-common files from its `MK_COMMON_FILES`.
fn makefile() -> String {
    format!("{}{MAKEFILE}", hash_header())
}

/// Runs `check` with the harness: the checker at its path, the Makefile as a base file, and
/// scratch files under `untracked/` written but never added, since the checker reads tracked
/// files only.
fn with_harness<T>(check: impl FnOnce(&HeaderHarness<'_>) -> T) -> T {
    let makefile = makefile();
    let base_files = [("Makefile", makefile.as_str())];
    check(&HeaderHarness {
        script: "scripts/check-license-headers.sh",
        base_files: &base_files,
        untracked_prefix: "untracked/",
    })
}

fn run_cases(cases: &[HeaderCase]) {
    let problems = with_harness(|harness| harness.run_cases(&meta::repo(), cases));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestRepositoryHeaders`. The repository passes its own check. Run with the repository's
/// own git environment: under the commit hook it names the index being committed.
#[test]
fn repository_headers() {
    let problems = with_harness(|harness| harness.check_repository(&meta::repo()));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestHeaderCases`, the notice on Rust files: present, missing, another licence, another
/// holder, a testdata fixture, and the edges of the window it must sit in. `RUST_HEADER`'s last
/// required line is its eighth: 22 lines before it put it on line 30, the last line of the window,
/// and one more pushes it out.
#[test]
fn header_cases_for_the_notice() {
    let other_holder = RUST_HEADER.replace("Roberto Leinardi", "Someone Else");
    let at_the_edge = format!("{}{RUST_HEADER}", "// filler\n".repeat(22));
    let past_the_edge = format!("{}{RUST_HEADER}", "// filler\n".repeat(23));
    run_cases(&[
        HeaderCase::new(
            "rust file with the header",
            &[("src/x.rs", RUST_HEADER)],
            true,
        ),
        HeaderCase::new(
            "rust file without a header",
            &[("src/x.rs", "fn x() {}\n")],
            false,
        ),
        HeaderCase::new(
            "another licence's header",
            &[("src/x.rs", APACHE_HEADER)],
            false,
        ),
        HeaderCase::new(
            "another copyright holder",
            &[("meta/src/x.rs", other_holder.as_str())],
            false,
        ),
        HeaderCase::new(
            "rust testdata fixture",
            &[("tests/testdata/fixture.rs", "fn x() {}\n")],
            true,
        ),
        HeaderCase::new(
            "header ending on the last line of the window",
            &[("src/x.rs", at_the_edge.as_str())],
            true,
        ),
        HeaderCase::new(
            "header ending one line past the window",
            &[("src/x.rs", past_the_edge.as_str())],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, every other governed file kind with and without the header.
#[test]
fn header_cases_for_each_file_kind() {
    let dockerfile = format!("# syntax=docker/dockerfile:1.7\n{}", hash_header());
    run_cases(&[
        HeaderCase::new(
            "shell script with the header",
            &[("scripts/a.sh", SHELL_HEADER)],
            true,
        ),
        HeaderCase::new(
            "shell script without a header",
            &[("scripts/a.sh", "#!/usr/bin/env bash\necho\n")],
            false,
        ),
        HeaderCase::new(
            "dockerfile with the header after the syntax line",
            &[("deployments/docker/Dockerfile", dockerfile.as_str())],
            true,
        ),
        HeaderCase::new(
            "dockerfile without a header",
            &[("deployments/docker/Dockerfile", "FROM scratch\n")],
            false,
        ),
        HeaderCase::new(
            "local make fragment, no header",
            &[(".mk/verify.mk", "x:\n")],
            false,
        ),
        HeaderCase::new(
            "make fragment make-common ships but MK_COMMON_FILES does not list, no header",
            &[(".mk/go.mk", "x:\n")],
            false,
        ),
        HeaderCase::new(
            "makefile without a header",
            &[("Makefile", MAKEFILE)],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, what is never governed: untracked files, make-common's files, a
/// testdata fixture, and Markdown, YAML and JSON; and a neighbour of an exempt path, which is.
#[test]
fn header_cases_for_the_exemptions() {
    run_cases(&[
        HeaderCase::new(
            "untracked file without a header",
            &[("untracked/scratch.rs", "fn x() {}\n")],
            true,
        ),
        HeaderCase::new(
            "upstream make fragment",
            &[
                (".mk/docker.mk", "x:\n"),
                (".mk/help.mk", "y:\n"),
                (".mk/rust.mk", "z:\n"),
            ],
            true,
        ),
        HeaderCase::new(
            "bootstrap script",
            &[("scripts/bootstrap-mk-common.sh", "#!/bin/sh\n")],
            true,
        ),
        HeaderCase::new(
            "a neighbour of the bootstrap script is governed",
            &[("scripts/bootstrap-mk-common-local.sh", "#!/bin/sh\n")],
            false,
        ),
        HeaderCase::new(
            "testdata fixture",
            &[("testenv/x/testdata/fixture.rs", "fn x() {}\n")],
            true,
        ),
        HeaderCase::new(
            "a neighbour of a testdata directory is governed",
            &[("testenv/x/testdatax/fixture.rs", "fn x() {}\n")],
            false,
        ),
        HeaderCase::new(
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

/// Negative control of the harness on this repository's checker: a case whose expectation is
/// wrong is reported, so a checker that accepted, or refused, everything could not pass the cases
/// above.
#[test]
fn a_wrong_expectation_is_reported() {
    let problems = with_harness(|harness| {
        harness.run_cases(
            &meta::repo(),
            &[
                HeaderCase::new(
                    "no header, expected to pass",
                    &[("src/x.rs", "fn x() {}\n")],
                    true,
                ),
                HeaderCase::new(
                    "the header, expected to fail",
                    &[("src/x.rs", RUST_HEADER)],
                    false,
                ),
            ],
        )
    });
    let names: Vec<&str> = problems
        .iter()
        .map(|problem| problem.location.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "no header, expected to pass",
            "the header, expected to fail"
        ]
    );
}
