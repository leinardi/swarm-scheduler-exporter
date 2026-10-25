# AGENTS.md

## What this is

A Prometheus exporter for Docker Swarm's scheduler: desired and running replicas per service (with
accurate eligibility for `global` services), live task state, service update/rollback state, node
availability and, opt-in, container state. It only ever reads from the Docker API. Fork of
[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter). The README
is the user-facing story; the metrics it lists are the public contract.

## Common commands

Build / test / lint use Make wrappers around `cargo`, workspace-wide and `--locked`:

```bash
make verify               # rust-build (stamped), rust-fmt-check, rust-clippy and rust-test in one go
make rust-build           # cargo build --workspace --all-targets --all-features, with the version stamps
make rust-test            # cargo test --workspace: every test but the integration suite, meta-tests included
make rust-clippy          # clippy over every target and feature, warnings as errors
make rust-fmt             # cargo fmt --all (rust-fmt-check only checks)
make rust-run ARGS='--containers --log-level debug'  # run the exporter against the local daemon
make rust-deny            # cargo-deny: advisories, licences, bans and sources (network required)
make audit-deps           # cargo-deny advisories only (network required); CI and the cargo-deny hook (on Cargo.toml/Cargo.lock/deny.toml changes) run every check
make check                # pre-commit on all files: the authoritative local gate, the same as CI's pre-commit-all
make check-stage          # pre-commit on the staging area only
make docker-build
make docker-run           # runs the image locally (read-only, no capabilities), binding the Docker socket and the metrics port
```

The meta-tests (`meta/tests/`, `cargo test -p meta`) check the repository rather than the
exporter, and need no Docker: the toolchain pins and the always-run hooks (`toolchain.rs`), the
plain/integration test separation (`integration_targets.rs`), the Dockerfile (`dockerfiles.rs`),
the Docker SDK boundary (`docker_boundary.rs`), the workspace lints (`workspace_lints.rs`), the
Markdown links (`docs.rs`) and the two scripts below (`license_headers.rs`, `suppressions.rs`).
`SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS=1` turns a missing hadolint into a failure instead of a
skip, as CI's `rust` job does.

Integration tests (a throwaway DinD Swarm on your Docker daemon, privileged containers required; see
[`CONTRIBUTING.md`](CONTRIBUTING.md#integration-tests)):

```bash
make test-integration                            # the whole suite, against the release binary
make test-integration RUN='^TestNode_'           # a subset (Go's -run semantics)
make test-integration UPDATE=1 RUN=TestSnapshot_ # rewrite the golden file
make sweep-test-leaks                            # remove every labelled test container and network
```

Single test:

```bash
cargo test --lib collector::reconciler -- --nocapture
cargo test --test engine_wire
```

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run. To
refresh: `make mk-common-update`. Project targets live in the local `.mk/*.mk` files listed in
`MK_LOCAL_FILES`, never as recipes in the Makefile.

## Layout

- `cmd/swarm-scheduler-exporter/` — flags, logging, the Docker client with timeouts, the HTTP
  server, and `-healthcheck` (the image is distroless: there is no shell or curl a `HEALTHCHECK`
  could run instead).
- `internal/collector/` — the collectors and the `Reconciler`, which is the one owner of the service
  and node caches and of every gauge derived from them. `docker_api.go` is the read-only `DockerAPI`
  interface all collector code reaches Docker through.
- `internal/labels/` — sanitizing and validating custom Prometheus label keys.
- `internal/logger/`, `internal/server/` — slog setup; `/metrics` and `/healthz`.
- `internal/testenv/`, `test/integration/` — the DinD Swarm harness and the `integration`-tagged
  suite.
- `testenv/` — the harness ported to Rust, with bollard behind its `integration` feature (the
  exporter package depends on it in no table; `meta/tests/docker_boundary.rs` checks it), and the
  suite's runner under `testenv/tests/integration`.
- `deployments/docker/` — the Dockerfile (DHI bases, pinned by tag), compose examples and the
  socket-proxy allowlist.
- `docs/release.md` — how a release is cut and recovered.
- `test/` — the meta-tests, with their helpers in `test/internal/` (`gitenv`, `toolgate`).
- `scripts/` — the licence-header check, the suppression ratchet, the make-common bootstrap and the
  Claude Code `gofmt` hook (`scripts/hooks/gofmt-on-edit.sh`, wired in `.claude/settings.json`).

### Generated or vendored: never hand-edit

- `.mk/{docker,help,password,pre-commit}.mk`, `.mk/.mk-common-version` and
  `scripts/bootstrap-mk-common.sh`: fetched from make-common; refresh them with
  `make mk-common-update` and port a change upstream.
- `tests/testdata/**` and `testenv/tests/integration/testdata/**`: byte-for-byte Engine API
  captures and golden files. The integration golden file is rewritten with
  `make test-integration UPDATE=1 RUN=TestSnapshot_`; the whitespace hooks skip them.

## Invariants

The full list, with what counts as a finding, is in `.agents/skills/adversarial-review/SKILL.md` §3.
The ones to keep in mind while writing code:

- **Read-only against Docker, enforced in code.** A `:ro` socket bind does not make the API
  read-only. The exporter's own Docker client (`src/docker`) exposes list, inspect and events
  calls only, and `meta/tests/docker_boundary.rs` keeps it to `GET` (plus the ping's `HEAD`) and
  keeps bollard, the harness's SDK, out of the exporter package.
- **Metrics are the public contract.** A renamed metric or a changed label set updates the README's
  Metrics section (and any alert example) in the same change. No label fed by an unbounded value.
- **Series lifecycle.** A removed resource's series are deleted, never set to 0; known state sets
  are emitted exhaustively; snapshot families are built whole and swapped in one step.
- **One owner for Swarm state.** Only the `Reconciler` goroutine writes the caches and derived
  gauges; the event dispatcher only marks keys dirty. Lock order is `Reconciler.mu`, then
  `metadataMu`.
- **Bounded work.** Bounded dirty-key set, bounded inspects per cycle, retries with delays, every
  request under `withDockerTimeout`, and no map keyed by an external ID without an eviction path.
- **Health means published data.** `HealthSnapshot` is evaluated at scrape time; nothing marks
  health from the poller itself.

## Conventions worth knowing

- Version strings (`version`, `commit`, `date`) live in `cmd/swarm-scheduler-exporter/version.go`
  and are filled by `-ldflags -X main.version=...` from `GO_LDFLAGS` in `.mk/go.mk`.
- Every Go file carries the MIT license header, after the `//go:build` line where there is one.
- Fixtures under `testdata/` are byte-for-byte captures (Engine API wire captures, golden files);
  the whitespace pre-commit hooks skip them, so do not tidy them by hand.
- A new `//nolint`, `# shellcheck disable=` or `# hadolint ignore=` explains why the fix does not
  apply here, not which rule fired.

## Licence headers and suppressions

Every Go and shell source, Dockerfile, Makefile and repo-local `.mk` snippet carries the MIT header,
with the year of the file's first commit (`scripts/check-license-headers.sh`, the
`check-license-headers` hook). The make-common files are exempt: they carry make-common's own.

Every in-file suppression is counted per rule against `.suppressions-baseline` by the
`check-suppressions` hook (`scripts/check-suppressions.sh`): `//nolint:<linter>`,
`# shellcheck disable=` and `# hadolint ignore=`. A commit that adds one, swaps one or moves one
must change the baseline too, and so must one whose count fell below it: run
`scripts/check-suppressions.sh --update` and commit the baseline with the change.

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `**/*.rs`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` | `make verify`; after `testenv/**` or anything the integration suite exercises, `make test-integration` (Docker) |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | `make rust-deny` (network) |
| `deployments/docker/Dockerfile`, `.hadolint.yaml`, `deployments/docker/Dockerfile.dockerignore` | `cargo test -p meta --test dockerfiles --test toolchain`, `make docker-build` |
| `.github/workflows/**`, `.github/actions/**` | `pre-commit run actionlint --all-files`, `cargo test -p meta --test toolchain --test dockerfiles` |
| `.github/scripts/**` | `bash .github/scripts/binary-attestation-status.test.sh` |
| `.pre-commit-config.yaml`, `Makefile`, `.mk/**` | `cargo test -p meta --test toolchain`, `make help` |
| `scripts/check-license-headers.sh` | `cargo test -p meta --test license_headers` |
| `scripts/check-suppressions.sh`, `.suppressions-baseline` | `cargo test -p meta --test suppressions` |
| `meta/**` | `SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS=1 cargo test -p meta` |
| `**/*.md` | `cargo test -p meta --test docs`; prose wraps at 100 columns |

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load them before the work, not
after review:

- `go-style-guide` — before any `.go` edit.
- `adversarial-review` — for any review request ("review my diff", "is this ready to merge").
- `go-to-rust-port` — before planning, implementing or reviewing any part of the port to Rust.
  A new learning goes into this repository copy (never `~/.claude`), in the same commit as the
  change that found it.

## Commit messages

All commits MUST be Conventional Commits 1.0.0 **with a scope**:
`<type>(<scope>)[!]: <description>`, optional blank-line body and footers. Enforced by the
`conventional-pre-commit` `commit-msg` hook (`--force-scope`) and by the `conventional-commits` CI
job. Types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`,
`revert`. Breaking changes use `!` before `:` or a `BREAKING CHANGE:` footer. Release notes are not
built from these messages: `gh release create --generate-notes` lists the merged pull requests by
title. Examples: `fix(collector): drop stale task metrics when a service is removed`,
`ci(release): pin trivy`.

Release versions are derived by `svu` from the commits since the last tag, so a wrong type ships a
wrong version:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(metrics)!: rename the replica gauges` |
| minor | `feat` | `feat(collector): add node labels` |
| patch | `fix` | `fix(collector): skip removed services` |
| none | everything else: `perf`, `refactor`, `build`, `ci`, `chore`, `docs`, `style`, `test`, `revert` | `perf(collector): ...` |

The highest bump among the commits wins; with only "none" commits since the last tag, a release with
no version fails with "nothing to bump".

**Pick the type by whether the change should ship, not by what kind of change it is.** Anything that
changes the shipped binary or image and that users should receive is `fix` (or `feat`), even when it
is a performance improvement, a refactor or a revert. Use `perf`, `refactor`, `style` and `revert`
only when the commit is deliberately not meant to trigger a release on its own. A `revert` of a
shipped `feat` or `fix` is itself a `fix`. `svu` matches `feat`/`fix` anywhere in the subject (e.g.
`prefix:` counts as `fix:`), so avoid a word ending in `feat` or `fix` directly before a colon in
other subjects. PRs land as merge commits, so every commit counts, not just the PR title. See
[`docs/release.md`](docs/release.md).
