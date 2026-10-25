# Conventions (beyond AGENTS.md and the go-style-guide skill)

Full rules: `.agents/skills/go-style-guide/SKILL.md` and its `references/` (patterns, project
conventions, test waits); load before any `.go` edit. Lint traps that repeatedly bite:

- golangci-lint v2 runs every linter (`default: all`); `make go-lint` runs it through pre-commit
  with `--fix`, which can rewrite code: re-run the tests after linting.
- `noinlineerr`, `err113` (wrap sentinel errors), `mnd`, `varnamelen`, `exhaustive`, `nolintlint`
  (every `//nolint:<linter>` needs a specific linter and an explanation of why the fix does not
  apply).
- The depguard rule `docker-sdk-boundary` keeps Docker SDK imports in `internal/collector` and
  `cmd/swarm-scheduler-exporter` (plus the integration harness).

Suppressions are ratcheted per rule against `.suppressions-baseline`; a commit adding, moving or
removing one runs `scripts/check-suppressions.sh --update` and commits the baseline.

Tests: waits follow the go-style-guide test-waits reference (bounded `eventually`, no bare sleeps);
integration code carries `//go:build integration`, with the MIT header after it.

Commits: Conventional Commits with mandatory scope; type chosen by whether it should ship
(`fix`/`feat` release, others don't); avoid words ending in `fix`/`feat` before a colon in other
subjects (svu matches them).
