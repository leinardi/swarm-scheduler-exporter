# AGENTS.md

## What this is

A Prometheus exporter for Docker Swarm's scheduler: desired and running replicas per service (with
accurate eligibility for `global` services), live task state, service update/rollback state, node
availability and, opt-in, container state. It only ever reads from the Docker API. A Rust
workspace (edition 2024, toolchain pinned in `rust-toolchain.toml`) since 2.0; 1.x was written in
Go, and the README's "Migrating from 1.x" section lists every behaviour that changed. Fork of
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
None of them skips on a missing tool.

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

Three workspace members, listed explicitly in the root `Cargo.toml`, all under the one
`[workspace.lints]` set:

- the root package, `swarm-scheduler-exporter` (`src/`, `tests/`): the shipped binary.
  - `src/main.rs`, `src/run.rs`, `src/cli.rs` — the flags (clap), the runtime, the signal watch
    and the shutdown budget; `src/healthcheck.rs` is `--healthcheck` (the image is distroless:
    there is no shell or curl a `HEALTHCHECK` could run instead).
  - `src/docker/` — the exporter's own HTTP/1 Docker client (unix socket, TCP, TLS with rustls),
    the version negotiation and ping, and `DockerApi`, the read-only trait all collector code
    reaches Docker through.
  - `src/collector/` — the collectors and the reconciler (`reconciler/`), which is the one owner of
    the service and node caches and of every gauge derived from them.
  - `src/metrics/` — the in-tree Prometheus registry, families, text exposition, the `process_*`
    collector and the promhttp handler with its content negotiation.
  - `src/server.rs`, `src/server/` — `/metrics` and `/healthz` on hyper.
  - `src/logger/` — the slog-compatible JSON and text handlers; `src/labels.rs` — sanitizing and
    validating custom label keys; `src/clock.rs` — the injected clock; `src/version.rs` — the
    build stamps.
  - `src/go_compat/` — the Go behaviours the contract depends on, ported deliberately (duration
    and float formatting, `SplitHostPort`, the local time zone, `strconv.Quote`).
  - `tests/` — the black-box tests: the Engine API wire captures (`engine_wire.rs`, against
    `tests/support/fake_engine.rs`), the exposition, the logger and `go_compat` golden files.
- `testenv/` — the DinD Swarm harness, with bollard behind its `integration` feature (the
  exporter package depends on it in no table; `meta/tests/docker_boundary.rs` checks it), and the
  integration suite's runner and tests under `testenv/tests/integration`.
- `meta/` — the meta-tests, which check the repository rather than the exporter, with their
  helpers in `meta/src` (`repo`, `gitenv`, the Dockerfile and Markdown readers).
- `deployments/docker/` — the Dockerfile (DHI bases, pinned by tag), compose examples and the
  socket-proxy allowlist.
- `docs/release.md` — how a release is cut and recovered.
- `scripts/` — the licence-header check, the suppression ratchet, the make-common bootstrap and the
  Claude Code `rustfmt` hook (`scripts/hooks/rustfmt-on-edit.sh`, wired in `.claude/settings.json`).

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
  read-only. `DockerApi` exposes list, inspect and events calls only, and the client sends `GET`
  only, plus one `HEAD /_ping`. `meta/tests/docker_boundary.rs` holds it there: requests are built
  in one place, ask only for paths the shipped socket-proxy allowlist grants, and bollard (which
  the harness creates and removes things with) stays out of the exporter package.
- **Metrics are the public contract.** A renamed metric or a changed label set updates the README's
  Metrics section (and any alert example) in the same change. No label fed by an unbounded value.
- **Series lifecycle.** A removed resource's series are deleted, never set to 0; known state sets
  are emitted exhaustively; snapshot families are built whole and swapped in one step.
- **One owner for Swarm state.** Only the reconciler task writes the caches and derived gauges;
  the event dispatcher only marks keys dirty through a `ReconcilerHandle`. The dirty set, the
  generations and the resync counters share one lock (`Shared::state`), never held across an
  `.await`.
- **A panic costs a step, not the process.** Each reconciler step runs under `catch_unwind`
  (`reconciler/recover.rs`), logs `reconciler recovered from panic` and requests a resync, which
  is why the release profile keeps `panic = "unwind"`.
- **Bounded work.** Bounded dirty-key set, bounded inspects per cycle, retries with delays, every
  request under `with_docker_timeout`, and no map keyed by an external ID without an eviction
  path.
- **The clock is injected.** `clippy.toml` bans `Timestamp::now`, `Zoned::now`,
  `SystemTime::now` and `std::time::Instant::now`; the time comes from the `Clock` trait, so tests
  drive it with tokio's paused clock. Only `src/clock.rs` reads the wall clock.
- **Health means published data.** The health snapshot is evaluated at scrape time; nothing marks
  health from the poller itself.

## Conventions worth knowing

- The version strings come from `SWARM_SCHEDULER_EXPORTER_VERSION`, `_COMMIT` and `_DATE` at
  compile time (`src/version.rs`, `option_env!`), set by the make targets, the Dockerfile and the
  release workflow; unset, they read `dev`, `none` and `unknown`.
- Lints are silenced only with `#[expect(lint, reason = "...")]`: `allow_attributes` is denied,
  the reason is mandatory, and an `#[expect]` that stops firing fails the build.
- Fixtures under `testdata/` are byte-for-byte captures (Engine API wire captures, golden files);
  the whitespace pre-commit hooks skip them, so do not tidy them by hand.
- A new `#[expect]`, `# shellcheck disable=` or `# droast ignore=` explains why the fix does not
  apply here, not which lint fired.
- Where a test or a module replaces Go code, its provenance (`/// Go: TestX`) is a doc comment;
  sources name no Go source file, and the reasoning behind a port lives in the commit bodies.

## Licence headers and suppressions

Every Rust and shell source, Dockerfile, Makefile and repo-local `.mk` snippet carries the MIT
header, with the year of the file's first commit (`scripts/check-license-headers.sh`, the
`check-license-headers` hook). The make-common files are exempt: they carry make-common's own.

Every in-file suppression is counted per rule against `.suppressions-baseline` by the
`check-suppressions` hook (`scripts/check-suppressions.sh`), in five families: each lint of an
`#[expect(...)]` or `#[allow(...)]`, `# shellcheck disable=`, `# droast ignore=` and
`# droast global ignore=`. A commit that adds one, swaps one or moves one must change the
baseline too, and so must one whose count fell below it: run
`scripts/check-suppressions.sh --update` and commit the baseline with the change.

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `**/*.rs`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` | `make verify`; after `testenv/**` or anything the integration suite exercises, `make test-integration` (Docker) |
| `Cargo.toml`, `Cargo.lock`, `deny.toml` | `make rust-deny` (network) |
| `deployments/docker/Dockerfile`, `droast.toml`, `deployments/docker/Dockerfile.dockerignore` | `cargo test -p meta --test dockerfiles --test toolchain`, `make docker-build` |
| `.github/workflows/**`, `.github/actions/**` | `pre-commit run actionlint --all-files`, `cargo test -p meta --test toolchain --test dockerfiles` |
| `.github/scripts/**` | `bash .github/scripts/binary-attestation-status.test.sh` |
| `.pre-commit-config.yaml`, `Makefile`, `.mk/**` | `cargo test -p meta --test toolchain`, `make help` |
| `scripts/check-license-headers.sh` | `cargo test -p meta --test license_headers` |
| `scripts/check-suppressions.sh`, `.suppressions-baseline` | `cargo test -p meta --test suppressions` |
| `meta/**` | `cargo test -p meta` |
| `**/*.md` | `cargo test -p meta --test docs`; prose wraps at 100 columns |

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load them before the work, not
after review:

- `adversarial-review` — for any review request ("review my diff", "is this ready to merge").

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
