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

//! The repository under test: its root, its files and its tracked paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The repository root: the meta crate sits one level below it.
#[must_use]
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The absolute path of a slash-separated path relative to the root.
#[must_use]
pub fn path(relative: &str) -> PathBuf {
    root().join(relative)
}

/// Reads a file by its slash-separated path relative to the root.
///
/// # Panics
///
/// The file cannot be read, naming it.
#[must_use]
pub fn read(relative: &str) -> String {
    fs::read_to_string(path(relative)).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// The tracked files matching `pathspecs`, as slash-separated paths relative to the root. Run
/// with the repository's own git environment: under the commit hook it names the index being
/// committed.
///
/// # Panics
///
/// git cannot list them.
#[must_use]
pub fn tracked(pathspecs: &[&str]) -> Vec<String> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("-z")
        .arg("--")
        .args(pathspecs)
        .current_dir(root())
        .output()
        .unwrap_or_else(|error| panic!("run git ls-files: {error}"));
    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every file under `directory` (relative to the root, recursively) whose name ends with one of
/// `suffixes`, as slash-separated paths relative to the root, sorted.
///
/// # Panics
///
/// The directory cannot be read.
#[must_use]
pub fn files_under(directory: &str, suffixes: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(current) = pending.pop() {
        let entries =
            fs::read_dir(path(&current)).unwrap_or_else(|error| panic!("list {current}: {error}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("list {current}: {error}"));
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = format!("{current}/{name}");
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("inspect {relative}: {error}"));
            if kind.is_dir() {
                pending.push(relative);
            } else if kind.is_file() && suffixes.iter().any(|suffix| name.ends_with(suffix)) {
                found.push(relative);
            }
        }
    }
    found.sort();
    found
}
