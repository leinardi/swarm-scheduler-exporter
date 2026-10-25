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

//! Drives scripts/check-suppressions.sh, the hook that keeps every in-file linter suppression
//! counted against a reviewed baseline.
//!
//! Each case builds a scratch git repository and runs the script inside it, rather than against
//! this repository: what is under test is the rule, not today's counts, and a case has to be able
//! to add a suppression without that being a suppression anybody has to review. The Go fixtures
//! stay while the script still counts Go directives.

use std::fs;
use std::path::Path;
use std::process::Command;

use meta::{gitenv, repo};

/// The name the script looks for, relative to the repository it is run in.
const BASELINE_FILE: &str = ".suppressions-baseline";

/// The scratch Go file most cases edit.
const SCRATCH: &str = "scratch.go";

/// A Go file carrying exactly one `//nolint:gosec` directive.
const GO_WITH_GOSEC: &str = "package scratch

// Run does nothing in particular.
func Run() string {
\treturn \"ok\" //nolint:gosec // scratch fixture
}
";

/// The same single directive on a different line: the count is unchanged, which is the case the
/// per-rule counting alone cannot see.
const GO_WITH_GOSEC_MOVED: &str = "package scratch

//nolint:gosec // scratch fixture, moved
func Run() string {
\treturn \"ok\"
}
";

/// Two directives for the same linter.
const GO_WITH_TWO_GOSEC: &str = "package scratch

// Run does nothing in particular.
func Run() string {
\tvalue := \"ok\" //nolint:gosec // scratch fixture
\treturn value  //nolint:gosec // scratch fixture too
}
";

/// A string containing `//` before the directive. golangci-lint reads the directive; a detector
/// that stops at the first `//` on the line does not.
const GO_WITH_DIRECTIVE_AFTER_URL: &str = "package scratch

// Run does nothing in particular.
func Run() string {
\treturn \"https://example.com/x\" //nolint:gosec // scratch fixture behind a URL
}
";

/// A directive mentioned inside a sentence. It is not a suppression, and counting it as one would
/// make every comment about the ratchet fail the ratchet.
const GO_WITH_PROSE_MENTION: &str = "package scratch

// Run is written so that it needs no //nolint:gosec directive at all.
func Run() string {
\treturn \"ok\"
}
";

/// Both hadolint forms: the per-line one and the global one, which silences a rule for the whole
/// file and is therefore counted as its own family.
const DOCKERFILE_WITH_IGNORES: &str = "FROM scratch

# hadolint global ignore=DL3008
# hadolint ignore=DL3018
COPY x /x
";

/// A shellcheck directive.
const SHELL_WITH_DISABLE: &str = "#!/usr/bin/env bash
# shellcheck disable=SC2086
echo $NAMES
";

/// Two linters silenced with a space after the comma, which golangci-lint and nolintlint both
/// accept; stopping at the space would leave errcheck uncounted.
const GO_WITH_SPACED_LIST: &str = "package scratch

// Run does nothing in particular.
func Run() error {
\treturn nil //nolint:gosec, ErrCheck // scratch fixture
}
";

/// The broadest Go suppression there is: no linter named, so every one of them is silenced.
const GO_WITH_BARE_NOLINT: &str = "package scratch

//nolint // deliberately unspecific
func Run() {}
";

/// Another shellcheck directive, for the exclusion cases.
const SHELL_WITH_DISABLE_SC2034: &str = "#!/usr/bin/env bash
# shellcheck disable=SC2034
unused=1
";

/// Every Rust lint-attribute form the ratchet counts: an inner and an outer expect, an allow, and
/// a multi-line expect naming two clippy lints the way rustfmt wraps a long attribute. The reason
/// item is not a lint, commas and parentheses inside it included.
const RUST_WITH_ATTRIBUTES: &str = r#"#![expect(clippy::pedantic, reason = "scratch fixture, (inner)")]

#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "scratch fixture"
)]
fn long() {}

#[allow(dead_code)]
fn unused() {}

#![allow(missing_docs)]
"#;

/// Lint attributes only as data: in a raw string spanning lines, in a string, in a line comment
/// and in a nested block comment, around character literals holding a quote. None of it is a
/// suppression.
const RUST_WITH_ATTRIBUTE_TEXT: &str = r##"const FIXTURE: &str = r#"
#[allow(clippy::unwrap_used)]
"#;
const TEXT: &str = "#[expect(dead_code)]";
// #[allow(dead_code)]
/* #[allow(dead_code)] /* nested */ #[allow(dead_code)] */
fn quote<'a>(text: &'a str) -> char { let _quote = '"'; '#' }
"##;

/// A multi-line expect naming one lint.
const RUST_WITH_MULTI_LINE_EXPECT: &str = r#"#[expect(
    clippy::too_many_lines,
    reason = "scratch fixture"
)]
fn long() {}
"#;

/// A second lint added on its own line inside the same attribute: the opening line is untouched,
/// so only the line naming the lint shows the addition.
const RUST_WITH_MULTI_LINE_EXPECT_EXTENDED: &str = r#"#[expect(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "scratch fixture"
)]
fn long() {}
"#;

/// Directive text inside a Go raw string. None of it is a suppression.
const GO_WITH_RAW_STRING_FIXTURE: &str = "package scratch

// fixture is Go source held as data.
const fixture = `package other

func Run() string {
\treturn \"ok\" //nolint:gosec // inside a raw string
}
`
";

/// One more line added to that raw string -- the ordinary way a new case gets written, and a line
/// the staged-diff rule must not read as a directive.
const GO_WITH_RAW_STRING_FIXTURE_EXTENDED: &str = "package scratch

// fixture is Go source held as data.
const fixture = `package other

func Run() string {
\tvalue := \"ok\" //nolint:gosec // inside a raw string too
\treturn value //nolint:gosec // inside a raw string
}
`
";

/// A real directive after the raw string closes, which the same rule must still catch.
const GO_WITH_RAW_STRING_FIXTURE_AND_DIRECTIVE: &str = "package scratch

// fixture is Go source held as data.
const fixture = `package other

func Run() string {
\treturn \"ok\" //nolint:gosec // inside a raw string
}
`

func Run() string {
\treturn \"ok\" //nolint:gosec // a directive in code this time
}
";

/// A directive mentioned on a continuation line of a block comment. golangci-lint honors no
/// directive there, so neither does the ratchet.
const GO_WITH_BLOCK_COMMENT: &str = "package scratch

/*
 * Run needs no //nolint:gosec directive, and this line is not one.
 */
func Run() string {
\treturn \"ok\"
}
";

/// A scratch repository, removed when dropped.
struct Scratch(tempfile::TempDir);

impl Scratch {
    /// An empty git repository with an identity, so committing needs no global configuration.
    fn new() -> Self {
        let scratch =
            Self(tempfile::tempdir().unwrap_or_else(|error| panic!("scratch directory: {error}")));
        scratch.git(&["init", "-q", "-b", "main"]);
        scratch.git(&["config", "user.email", "ratchet@example.invalid"]);
        scratch.git(&["config", "user.name", "Ratchet Test"]);
        scratch.git(&["config", "commit.gpgsign", "false"]);
        scratch
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// Writes one file, creating the directories above it.
    fn write(&self, name: &str, content: &str) {
        let path = self.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("create the directory for {name}: {error}"));
        }
        fs::write(&path, content).unwrap_or_else(|error| panic!("write {name}: {error}"));
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path().join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"))
    }

    /// Writes a baseline holding `lines`, plus the comment a real one has.
    fn write_baseline(&self, lines: &str) {
        let mut content = "# scratch baseline\n".to_owned();
        if !lines.is_empty() {
            content.push_str(lines);
            content.push('\n');
        }
        self.write(BASELINE_FILE, &content);
    }

    /// Writes the scratch Go file and a baseline, and commits both, leaving a clean index.
    fn commit_initial(&self, source: &str, baseline: &str) {
        self.write(SCRATCH, source);
        self.write_baseline(baseline);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "scratch fixture"]);
    }

    /// Commits an empty baseline, leaving a clean index.
    fn commit_empty_baseline(&self) {
        self.write_baseline("");
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "empty baseline"]);
    }

    fn git(&self, args: &[&str]) {
        let output = gitenv::isolate(Command::new("git").args(args).current_dir(self.path()))
            .output()
            .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs the ratchet here: whether it accepted the tree, and its combined output.
    fn ratchet(&self, args: &[&str]) -> (bool, String) {
        let output = gitenv::isolate(
            Command::new(repo::path("scripts/check-suppressions.sh"))
                .args(args)
                .current_dir(self.path()),
        )
        .output()
        .unwrap_or_else(|error| panic!("run the ratchet: {error}"));
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }
}

/// A scratch state the ratchet must accept (`None`) or refuse naming the given complaint, so a
/// case cannot pass on the wrong one.
type Case = (&'static str, fn(&Scratch), Option<&'static str>);

fn run_cases(cases: &[Case]) {
    let mut failures = Vec::new();
    for (name, set_up, refusal) in cases {
        let scratch = Scratch::new();
        set_up(&scratch);
        let (accepted, output) = scratch.ratchet(&[]);
        match refusal {
            None if !accepted => {
                failures.push(format!(
                    "{name}: refused a tree it should accept:\n{output}"
                ));
            }
            Some(_) if accepted => {
                failures.push(format!(
                    "{name}: accepted the tree, want a refusal:\n{output}"
                ));
            }
            Some(message) if !output.contains(message) => {
                failures.push(format!(
                    "{name}: output does not mention {message:?}:\n{output}"
                ));
            }
            None | Some(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Go: `TestCheckSuppressions`, the staged-diff and count rules: each state the ratchet must
/// accept or refuse.
#[test]
fn check_suppressions() {
    run_cases(&[
        (
            "a tree that matches its baseline passes",
            |scratch| scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t1"),
            None,
        ),
        (
            "an added suppression with an untouched baseline is refused",
            |scratch| {
                scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t1");
                scratch.write(SCRATCH, GO_WITH_TWO_GOSEC);
                scratch.git(&["add", SCRATCH]);
            },
            Some("without touching .suppressions-baseline"),
        ),
        (
            "the same addition passes once the baseline is staged with it",
            |scratch| {
                scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t1");
                scratch.write(SCRATCH, GO_WITH_TWO_GOSEC);
                scratch.write_baseline("nolint/gosec\t2");
                scratch.git(&["add", SCRATCH, BASELINE_FILE]);
            },
            None,
        ),
        (
            "a suppression swapped one for one is refused even though the count holds",
            |scratch| {
                scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t1");
                scratch.write(SCRATCH, GO_WITH_GOSEC_MOVED);
                scratch.git(&["add", SCRATCH]);
            },
            Some("without touching .suppressions-baseline"),
        ),
        (
            "a count above the baseline is refused with nothing staged",
            |scratch| scratch.commit_initial(GO_WITH_TWO_GOSEC, "nolint/gosec\t1"),
            Some("baseline allows 1"),
        ),
        (
            "a count below the baseline is refused too",
            |scratch| scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t3"),
            Some("baseline still claims 3"),
        ),
        (
            "a rule the baseline has never heard of is refused",
            |scratch| scratch.commit_initial(GO_WITH_GOSEC, "nolint/dupl\t1"),
            Some("does not list this rule"),
        ),
        (
            "a directive behind a string containing a comment opener still counts",
            |scratch| scratch.commit_initial(GO_WITH_DIRECTIVE_AFTER_URL, "nolint/dupl\t1"),
            Some("nolint/gosec"),
        ),
        (
            "a directive mentioned in prose is not a suppression",
            |scratch| scratch.commit_initial(GO_WITH_PROSE_MENTION, ""),
            None,
        ),
        (
            "a directive inside a block comment is not a suppression either",
            |scratch| scratch.commit_initial(GO_WITH_BLOCK_COMMENT, ""),
            None,
        ),
        (
            "a deleted baseline is refused rather than treated as no suppressions",
            |scratch| {
                scratch.commit_initial(GO_WITH_GOSEC, "nolint/gosec\t1");
                scratch.git(&["rm", "-q", BASELINE_FILE]);
            },
            Some("is missing"),
        ),
        (
            "a deleted baseline is refused in a tree with no suppressions at all",
            |scratch| {
                scratch.commit_initial(GO_WITH_PROSE_MENTION, "");
                scratch.git(&["rm", "-q", BASELINE_FILE]);
            },
            Some("is missing"),
        ),
    ]);
}

/// Go: `TestCheckSuppressionsUpdate`. `--update` writes a baseline the checker then accepts: the
/// script's own instruction for a refusal is to run it, so a broken `--update` would leave no way
/// out.
#[test]
fn update_writes_an_accepted_baseline() {
    let scratch = Scratch::new();
    scratch.commit_initial(GO_WITH_TWO_GOSEC, "nolint/gosec\t1");
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a count above its baseline:\n{output}"
    );
    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let written = scratch.read(BASELINE_FILE);
    assert!(
        written.contains("nolint/gosec\t2"),
        "the rewritten baseline does not record the two directives:\n{written}"
    );
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet still refuses the tree after --update:\n{output}"
    );
}

/// Go: `TestCheckSuppressionsCountsEveryFamily`. The detector across every family at once: the
/// baseline is keyed per rule, so a family that resolves to the wrong key -- or to no key -- takes
/// its suppressions out of the ratchet without anything else noticing.
#[test]
fn counts_every_family() {
    let scratch = Scratch::new();
    scratch.write("Dockerfile", DOCKERFILE_WITH_IGNORES);
    scratch.write("run.sh", SHELL_WITH_DISABLE);
    scratch.write("bare.go", GO_WITH_BARE_NOLINT);
    scratch.write("spaced.go", GO_WITH_SPACED_LIST);
    scratch.write("src/attributes.rs", RUST_WITH_ATTRIBUTES);
    scratch.write("src/text.rs", RUST_WITH_ATTRIBUTE_TEXT);
    scratch.write_baseline("");
    scratch.git(&["add", "-A"]);
    scratch.git(&["commit", "-q", "-m", "one file per family"]);

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let written = scratch.read(BASELINE_FILE);

    // Each linter of a two-linter nolint counts separately, the global hadolint form is its own
    // family, and a bare //nolint is recorded as the everything-suppression it is.
    for line in [
        "hadolint-global-ignore/DL3008\t1",
        "hadolint-ignore/DL3018\t1",
        "shellcheck-disable/SC2086\t1",
        "nolint/*\t1",
        "nolint/gosec\t1",
        "nolint/errcheck\t1",
        "expect/clippy::pedantic\t1",
        "expect/clippy::too_many_lines\t1",
        "expect/clippy::cast_possible_truncation\t1",
        "allow/dead_code\t1",
        "allow/missing_docs\t1",
    ] {
        assert!(
            written.contains(line),
            "the baseline does not record {line:?}:\n{written}"
        );
    }
    // The Rust text fixture names clippy::unwrap_used and expect/dead_code only as data; the one
    // real allow(dead_code) is in the attributes fixture.
    for unwanted in ["allow/clippy::unwrap_used", "expect/dead_code", "reason"] {
        assert!(
            !written.contains(unwanted),
            "the baseline records {unwanted:?}, which is not a suppression:\n{written}"
        );
    }
}

/// Go: `TestCheckSuppressionsSeesALintAddedToAMultiLineExpect`. A Rust lint is located on the line
/// that names it, not on the line that opens the attribute: rustfmt puts each lint of a long
/// attribute on its own line, and adding one there leaves the opening line untouched.
#[test]
fn sees_a_lint_added_to_a_multi_line_expect() {
    let scratch = Scratch::new();
    scratch.write("scratch.rs", RUST_WITH_MULTI_LINE_EXPECT);
    scratch.write_baseline("expect/clippy::too_many_lines\t1");
    scratch.git(&["add", "-A"]);
    scratch.git(&["commit", "-q", "-m", "scratch fixture"]);
    scratch.write("scratch.rs", RUST_WITH_MULTI_LINE_EXPECT_EXTENDED);
    scratch.git(&["add", "scratch.rs"]);

    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a lint added to an existing expect:\n{output}"
    );
    assert!(
        output.contains("scratch.rs:3\texpect/clippy::cast_possible_truncation"),
        "the output does not locate the added lint on its own line:\n{output}"
    );
}

/// Go: `TestCheckSuppressionsSkipsUpstreamAndFixtures`. A suppression in make-common's bootstrap
/// script or in a testdata fixture is neither counted nor treated as newly added: refreshing the
/// script from upstream must not move a number nobody here chose, and a fixture is data.
#[test]
fn skips_upstream_and_fixtures() {
    let scratch = Scratch::new();
    scratch.commit_empty_baseline();
    scratch.write("scripts/bootstrap-mk-common.sh", SHELL_WITH_DISABLE_SC2034);
    scratch.write("internal/collector/testdata/fixture.go", GO_WITH_GOSEC);
    scratch.git(&["add", "-A"]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet counted an upstream or fixture file:\n{output}"
    );
}

/// Go: `TestCheckSuppressionsCountsNeighborsOfExcludedPaths`, the negative control of the test
/// above: a file next to an excluded path, but outside it, is counted like any other, so an
/// exclusion that grew too broad would fail here.
#[test]
fn counts_neighbors_of_excluded_paths() {
    let mut failures = Vec::new();
    for (name, content, key) in [
        (
            "scripts/bootstrap.sh",
            SHELL_WITH_DISABLE_SC2034,
            "shellcheck-disable/SC2034",
        ),
        (
            "scripts/bootstrap-mk-common.sh.bak.sh",
            SHELL_WITH_DISABLE_SC2034,
            "shellcheck-disable/SC2034",
        ),
        ("internal/testdatax/x.go", GO_WITH_GOSEC, "nolint/gosec"),
    ] {
        let scratch = Scratch::new();
        scratch.commit_empty_baseline();
        scratch.write(name, content);
        scratch.git(&["add", "-A"]);
        let (accepted, output) = scratch.ratchet(&[]);
        if accepted || !output.contains(key) {
            failures.push(format!("the ratchet did not count {name}:\n{output}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Go: `TestCheckSuppressionsReadsTheStagedFileNotTheHunk`. The staged-diff rule sees whole files.
/// The detector carries raw-string and block-comment state across lines, so judging added lines
/// on their own would both invent suppressions inside a fixture and miss real ones after a
/// closing backquote -- and an invented one cannot be resolved, because the whole-tree count
/// agrees with the baseline and `--update` would rewrite it unchanged.
#[test]
fn reads_the_staged_file_not_the_hunk() {
    for (name, edited, refusal) in [
        (
            "a line added inside a raw string is not a suppression",
            GO_WITH_RAW_STRING_FIXTURE_EXTENDED,
            None,
        ),
        // The staged-diff rule's own message: the count rule refuses this edit too, and
        // asserting on the refusal alone would let a detector that found nothing pass.
        (
            "a real directive added after the raw string still is",
            GO_WITH_RAW_STRING_FIXTURE_AND_DIRECTIVE,
            Some("without touching .suppressions-baseline"),
        ),
    ] {
        let scratch = Scratch::new();
        scratch.commit_initial(GO_WITH_RAW_STRING_FIXTURE, "");
        scratch.write(SCRATCH, edited);
        scratch.git(&["add", SCRATCH]);
        let (accepted, output) = scratch.ratchet(&[]);
        match refusal {
            None => assert!(
                accepted,
                "{name}: the ratchet read a raw-string line as a directive:\n{output}"
            ),
            Some(message) => {
                assert!(
                    !accepted,
                    "{name}: the ratchet accepted an added directive:\n{output}"
                );
                assert!(
                    output.contains(message),
                    "{name}: the output does not mention {message:?}, so the staged-diff rule \
                     did not see the directive:\n{output}"
                );
            }
        }
    }
}

/// Go: `TestCheckSuppressionsUpdateDischargesASwap`. The way out of the one refusal the counts
/// cannot explain: a swap leaves every count where it was, so `--update` has to produce a
/// baseline that differs anyway -- otherwise there is nothing to stage, the hook refuses again,
/// and the only remaining exits are a cosmetic edit or skipping the hook.
#[test]
fn update_discharges_a_swap() {
    let scratch = Scratch::new();
    scratch.write(SCRATCH, GO_WITH_GOSEC);
    scratch.write_baseline("");
    scratch.git(&["add", "-A"]);
    scratch.git(&["commit", "-q", "-m", "scratch fixture"]);

    // A real baseline, written by the script itself and committed, is the starting point: the
    // question is whether a second --update against unchanged counts still moves the file.
    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    scratch.git(&["add", BASELINE_FILE]);
    scratch.git(&["commit", "-q", "-m", "record the baseline"]);
    let before = scratch.read(BASELINE_FILE);

    scratch.write(SCRATCH, GO_WITH_GOSEC_MOVED);
    scratch.git(&["add", SCRATCH]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        !accepted,
        "the ratchet accepted a swapped suppression:\n{output}"
    );

    let (updated, output) = scratch.ratchet(&["--update"]);
    assert!(updated, "--update failed:\n{output}");
    let after = scratch.read(BASELINE_FILE);
    assert_ne!(
        after, before,
        "--update left the baseline byte-identical, so there is nothing to stage"
    );
    scratch.git(&["add", BASELINE_FILE]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet still refuses the swap after the baseline was staged:\n{output}"
    );
}

/// Go: `TestCheckSuppressionsAllowsARename`. Moving a file is not adding its suppressions. The
/// staged-diff rule works from the new name, so without rename detection a moved file would look
/// wholly new and every directive it already carried would be reported as freshly added -- with
/// counts unchanged, and therefore with nothing the baseline could say about it.
#[test]
fn allows_a_rename() {
    let scratch = Scratch::new();
    scratch.write("old.go", GO_WITH_TWO_GOSEC);
    scratch.write_baseline("nolint/gosec\t2");
    scratch.git(&["add", "-A"]);
    scratch.git(&["commit", "-q", "-m", "a file with suppressions"]);
    scratch.git(&["mv", "old.go", "new.go"]);
    let (accepted, output) = scratch.ratchet(&[]);
    assert!(
        accepted,
        "the ratchet read a moved file as newly added suppressions:\n{output}"
    );
}
