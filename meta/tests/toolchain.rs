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

//! The repository's tooling, the language-independent part: the always-run hooks running once,
//! and no flag that makes a gate pass on nothing. The Go version pins and the untagged-tests rule
//! stay Go tests until the cutover.

use meta::repo;
use regex::Regex;
use serde_yaml_ng::Value;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// The hooks of .pre-commit-config.yaml by id, as written; an id with aliases (the actionlint,
/// shellcheck and yamllint variants) appears more than once.
fn pre_commit_hooks() -> Vec<(String, Value)> {
    let config: Value = serde_yaml_ng::from_str(&repo::read(".pre-commit-config.yaml"))
        .unwrap_or_else(|error| panic!("parse .pre-commit-config.yaml: {error}"));
    let repos = config
        .get("repos")
        .and_then(Value::as_sequence)
        .unwrap_or_else(|| panic!(".pre-commit-config.yaml has no repos list"));
    repos
        .iter()
        .filter_map(|entry| entry.get("hooks").and_then(Value::as_sequence))
        .flatten()
        .map(|hook| {
            let id = hook.get("id").and_then(Value::as_str).unwrap_or_default();
            (id.to_owned(), hook.clone())
        })
        .collect()
}

/// The hooks that read the whole tree rather than the files pre-commit passes, so each one runs
/// on every commit and must not run twice per commit.
const ALWAYS_RUN_HOOKS: [&str; 3] = ["check-license-headers", "check-suppressions", "docs-links"];

/// Go: `TestAlwaysRunHooksRunOnce`. A whole-tree hook takes no file list and always runs, because
/// a commit that only deletes a file passes no file to it; and it is pinned to the pre-commit
/// stage, because `always_run` makes a hook eligible at every installed stage, commit-msg
/// included.
#[test]
fn always_run_hooks_run_once() {
    let hooks = pre_commit_hooks();
    let mut problems = Vec::new();
    for id in ALWAYS_RUN_HOOKS {
        let found: Vec<&Value> = hooks
            .iter()
            .filter(|(hook_id, _)| hook_id == id)
            .map(|(_, hook)| hook)
            .collect();
        let [hook] = found.as_slice() else {
            problems.push(format!(
                "hook {id} appears {} times, want once",
                found.len()
            ));
            continue;
        };
        if hook.get("always_run").and_then(Value::as_bool) != Some(true) {
            problems.push(format!("hook {id} does not set always_run: true"));
        }
        if hook.get("pass_filenames").and_then(Value::as_bool) != Some(false) {
            problems.push(format!("hook {id} does not set pass_filenames: false"));
        }
        let stages: Option<Vec<&str>> = hook
            .get("stages")
            .and_then(Value::as_sequence)
            .map(|stages| stages.iter().filter_map(Value::as_str).collect());
        if stages.as_deref() != Some(["pre-commit"].as_slice()) {
            problems.push(format!(
                "hook {id} runs at stages {stages:?}, want exactly [pre-commit]"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// A flag that turns a gate with nothing to check, or a failing one, into a pass.
struct VacuousFlag {
    name: &'static str,
    pattern: Regex,
}

/// The flags none of the files that define gates may carry, outside [`ALLOWED_FRAGMENTS`].
fn vacuous_flags() -> Vec<VacuousFlag> {
    [
        ("|| true", r"\|\|\s*(true\b|:(\s|;|$)|exit\s+0\b)"),
        ("make's - recipe prefix", r"^\t[@+]*-"),
        (".IGNORE", r"^\.IGNORE\b"),
        (
            "make --ignore-errors",
            r"\$\(MAKE\).*\s(-[a-zA-Z]*i[a-zA-Z]*|--ignore-errors)\b",
        ),
        ("continue-on-error", r"continue-on-error"),
        (
            "if: false",
            r#"\bif:\s*(["']?false["']?\s*$|["']?\$\{\{\s*false\s*\}\}["']?)"#,
        ),
    ]
    .into_iter()
    .map(|(name, pattern)| VacuousFlag {
        name,
        pattern: re(pattern),
    })
    .collect()
}

/// One exact piece of shell, in one file, whose `|| true` is not a gate passing on nothing; the
/// reason says why. A line is allowed only when the flag is gone once the fragment is removed, so
/// a second `|| true` on the same line is still caught.
struct AllowedFragment {
    file: &'static str,
    fragment: &'static str,
    reason: &'static str,
}

const ALLOWED_FRAGMENTS: [AllowedFragment; 2] = [
    AllowedFragment {
        file: ".github/workflows/release.yaml",
        fragment: r#"{ grep -E "$RELEASE_TAG_REGEX" || true; }"#,
        reason: "a tag list with no release tag in it is an empty answer, not an error; grep \
                 exits 1 on no match and pipefail would turn that into a failed lookup",
    },
    AllowedFragment {
        file: ".github/workflows/release.yaml",
        fragment: r#"{ git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        reason: "HEAD^ does not exist on a root commit, which has no previous release",
    },
];

/// The flags on `content`'s lines, after removing the fragments allowed in `file`, as
/// `<line>: <flag>: <text>`; `used` records which allowlist entries matched.
fn vacuous_hits(
    flags: &[VacuousFlag],
    file: &str,
    content: &str,
    used: &mut [bool; ALLOWED_FRAGMENTS.len()],
) -> Vec<String> {
    let mut hits = Vec::new();
    for (index, line) in content.split('\n').enumerate() {
        let mut line = line.to_owned();
        for (allowed_index, allowed) in ALLOWED_FRAGMENTS.iter().enumerate() {
            if allowed.file == file && line.contains(allowed.fragment) {
                line = line.replace(allowed.fragment, "");
                used[allowed_index] = true;
            }
        }
        for flag in flags {
            if flag.pattern.is_match(&line) {
                hits.push(format!("{}: {}: {}", index + 1, flag.name, line.trim()));
            }
        }
    }
    hits
}

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and the make
/// fragments, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let flags = vacuous_flags();
    let fragments = repo::files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let workflows = repo::files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !workflows.is_empty(),
        "no YAML found under .github; the checks would see nothing"
    );
    let mut files = vec!["Makefile".to_owned(), ".pre-commit-config.yaml".to_owned()];
    files.extend(fragments);
    files.extend(workflows);

    let mut used = [false; ALLOWED_FRAGMENTS.len()];
    let mut problems = Vec::new();
    for file in &files {
        for hit in vacuous_hits(&flags, file, &repo::read(file), &mut used) {
            problems.push(format!("{file}:{hit}"));
        }
    }
    for (allowed, used) in ALLOWED_FRAGMENTS.iter().zip(used) {
        if !used {
            problems.push(format!(
                "the allowlist entry {:?} in {} ({}) matches nothing; remove it",
                allowed.fragment, allowed.file, allowed.reason
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found, and the allowlist covers only its own fragment in its own file.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    let mut used = [false; ALLOWED_FRAGMENTS.len()];
    for sample in [
        "\tgo vet ./... || true",
        "\tgo vet ./... ||:",
        "\tgo vet ./... || exit 0",
        "\t-go vet ./...",
        "\t@-go vet ./...",
        ".IGNORE: go-vet",
        "\t$(MAKE) -i go-test",
        "\t$(MAKE) --ignore-errors go-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
    ] {
        assert!(
            !vacuous_hits(&flags, "Makefile", sample, &mut used).is_empty(),
            "no vacuous flag found in {sample:?}"
        );
    }

    let release = ".github/workflows/release.yaml";
    let allowed = r#"  { grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V"#;
    let hits = vacuous_hits(&flags, release, allowed, &mut used);
    assert!(
        hits.is_empty(),
        "the allowed fragment was flagged: {hits:?}"
    );

    let extended = format!("{allowed} || true");
    for (file, sample) in [
        (".github/workflows/ci.yaml", allowed),
        (release, extended.as_str()),
        (
            ".github/workflows/x.yaml",
            r#"  { git tag --merged "HEAD^" 2>/dev/null || true; }"#,
        ),
    ] {
        assert!(
            !vacuous_hits(&flags, file, sample, &mut used).is_empty(),
            "{file}: {sample:?} passed through the allowlist"
        );
    }
}
