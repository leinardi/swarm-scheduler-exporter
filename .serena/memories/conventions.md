# Conventions (beyond AGENTS.md)

Lint set: `[workspace.lints]` in the root Cargo.toml (pedantic plus denies), every member opts in
with `[lints] workspace = true`, clippy runs workspace-wide with `--all-features`. Traps that
repeatedly bite:

- `min_ident_chars`: no single-letter names, closures included.
- `wildcard_enum_match_arm`: name every variant, no `_ =>` on an enum.
- `unwrap_used`/`expect_used` denied outside `#[test]`/`#[cfg(test)]`; helpers in `tests/*.rs` and
  `testenv/tests/` are not covered.
- `print_stdout`/`print_stderr` denied: output goes through the logger or an explicit `writeln!`.
- `disallowed_methods`: no wall-clock read outside `src/clock.rs`; use the injected `Clock`.
- `large_futures`: `Box::pin` a big future rather than suppressing it.

Suppressions: only `#[expect(lint, reason = "...")]`, ratcheted per lint against
`.suppressions-baseline`; a commit adding, moving or removing one runs
`scripts/check-suppressions.sh --update` and commits the baseline. The reason says why the fix
does not apply, not which lint fired.

Parity: any difference from the Go 1.x behaviour not listed in the README's "Migrating from 1.x"
is a defect. Ported tests carry ``/// Go: `TestX` ``; Rust sources never name a Go source file.

Tests: paused tokio clock for time; integration waits poll (`eventually`, `consistently`), never
sleep. Every `.rs` file carries the MIT `/* */` header.

Commits: Conventional Commits with mandatory scope; type chosen by whether it should ship
(`fix`/`feat` release, others don't); avoid words ending in `fix`/`feat` before a colon in other
subjects (svu matches them).
