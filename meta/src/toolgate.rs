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

//! The external tools the behavioural meta-tests drive.
//!
//! A behavioural test that quietly passes when its tool is missing passes in exactly the place it
//! is meant to guard: CI. So a missing tool ends the test early, with a note, on a developer
//! machine, and fails it wherever `SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS=1` is set, which the CI test
//! jobs do. The tools are a closed list: widening it is a decision, not a side effect of a new
//! test.

use std::env;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The variable that turns a missing tool from an early, passing return into a failure.
pub const REQUIRE_ENV: &str = "SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS";

/// The Dockerfile linter the hadolint pre-commit hook runs from `PATH`.
pub const HADOLINT: &str = "hadolint";

/// The closed list of tools a meta-test may be gated on.
pub const TOOLS: &[&str] = &[HADOLINT];

/// The path of `tool` on `PATH`. `None`, with a note on standard error, when it is missing and
/// the caller should return early.
///
/// # Panics
///
/// `tool` is not in [`TOOLS`], or it is missing while [`REQUIRE_ENV`] is `1`.
#[must_use]
pub fn lookup(tool: &str) -> Option<PathBuf> {
    assert!(
        TOOLS.contains(&tool),
        "toolgate: {tool:?} is not one of the gated tools {TOOLS:?}"
    );

    let found = env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(tool))
            .find(|candidate| is_executable(candidate))
    });
    if found.is_some() {
        return found;
    }

    assert!(
        env::var(REQUIRE_ENV).as_deref() != Ok("1"),
        "{tool} is required ({REQUIRE_ENV}=1) but was not found on PATH"
    );
    // Nowhere left to report a failed write to standard error.
    let _ = writeln!(
        io::stderr(),
        "{tool} not found on PATH: this check did not run; set {REQUIRE_ENV}=1 to make it a failure"
    );
    None
}

fn is_executable(candidate: &Path) -> bool {
    candidate
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
