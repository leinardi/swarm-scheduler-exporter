# AGENTS.md

## What this is

A Prometheus exporter for Docker Swarm's scheduler: desired and running replicas per service (with
accurate eligibility for `global` services), live task state, service update/rollback state, node
availability and, opt-in, container state. It only ever reads from the Docker API. A Rust
workspace (edition 2024, toolchain pinned in `rust-toolchain.toml`) since 2.0; 1.x was written in
Go, and the README's "Migrating" section lists every behaviour that changed since. The metrics,
flags and health rules are the contract; Go 1.x is history, not an authority. Fork of
[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter). The README
is the user-facing story; the metrics it lists are the public contract.

## Common commands

Build / test / lint use Make wrappers around `cargo`, workspace-wide and `--locked`:

```bash
make verify               # rust-build (unstamped), rust-fmt-check, rust-clippy, rust-test and rust-doc
make rust-doc             # cargo doc --workspace --all-features --no-deps --locked, warnings as errors
make rust-build           # cargo build --workspace --all-targets --all-features, unstamped (as rust-test)
make rust-test            # cargo test --workspace: every test but the integration suite, meta-tests included
make rust-clippy          # clippy over every target and feature, warnings as errors
make rust-fmt             # cargo fmt --all (rust-fmt-check only checks)
make rust-run ARGS='--containers --log-level debug'  # run the exporter against the local Docker daemon
make rust-deny            # cargo-deny: advisories, licences, bans and sources (network required)
make audit-deps           # cargo-deny advisories only (network required); CI and the cargo-deny hook (on Cargo.toml/Cargo.lock/deny.toml changes) run every check
make check                # pre-commit on all files: the authoritative local gate, the same as CI's pre-commit-all
make check-stage          # pre-commit on the staging area only
make dist-static          # the static release binaries, dist/swarm-scheduler-exporter-linux-{amd64,arm64} (zig, cargo-zigbuild)
make image-build-prebuilt # the image's prebuilt target from dist/, for IMAGE_PLATFORM (default: the host's; needs a dhi.io login)
make image-test           # scripts/image-test.sh on that image, IMAGE_TEST_TIER full (default), reduced or smoke
make docker-build         # the image's default target, which compiles the binary itself
make docker-run           # runs the image locally (read-only, no capabilities), binding the Docker socket and the metrics port
```

The meta-tests (`meta/tests/`, `cargo test -p meta`) check the repository rather than the exporter,
and need no Docker: the toolchain pins and Trivy never pinned (`toolchain.rs`), the Dockerfile's
frontend, its five stages and their tag-pinned bases (`dockerfiles.rs`), the Docker SDK boundary
(`docker_boundary.rs`), and the workspace lints (`workspace_lints.rs`).

Integration tests (a throwaway DinD Swarm on your Docker daemon, privileged containers required; see
[`CONTRIBUTING.md`](CONTRIBUTING.md#integration-tests)):

```bash
make test-integration                            # the whole suite, against the release binary
make test-integration RUN='^TestNode_'           # a subset (a regular expression per /-separated level)
make test-integration UPDATE=1 RUN=TestSnapshot_ # rewrite the golden file
make sweep-test-leaks                            # remove every labelled test container and network
```

Single test:

```bash
cargo test --lib collector::reconciler -- --nocapture
cargo test --test engine_wire
```

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run,
`rust.mk` (the `rust-*` targets and `verify-rust`) among them; the Makefile sets its knobs
(`RUST_STAMP_PREFIX`, `RUST_RUN_BIN`). To refresh: `make mk-common-update`. Project targets live
in the local `.mk/*.mk` files listed in `MK_LOCAL_FILES` (`docker-socket-run.mk`, `audit-deps.mk`,
`integration-test.mk`, `verify.mk`, `dist.mk`, `image.mk`), never as recipes in the Makefile.

## Layout

Three workspace members, listed explicitly in the root `Cargo.toml`, all under the one
`[workspace.lints]` set:

- the root package, `swarm-scheduler-exporter` (`src/`, `tests/`): the shipped binary.
  - `src/main.rs`, `src/run.rs`, `src/cli.rs` — the flags (clap), the startup order and the
    shutdown budget, on service-kit's runtime and lazy shutdown signal; `src/healthcheck.rs` is
    `--healthcheck` (the image is distroless: there is no shell or curl a `HEALTHCHECK` could run
    instead).
  - `src/docker/` — `DockerApi`, the read-only trait all collector code reaches Docker through, on
    a `Client` over `docker-client`'s bollard connection (features `tcp` and `tls`), the exporter's
    API types (mapped from bollard's models in `convert.rs`) and the timeouts its callers put
    around them.
  - `src/collector/` — the collectors and the reconciler (`reconciler/`), which is the one owner of
    the service and node caches and of every gauge derived from them.
  - `src/server.rs`, `src/server/` — `/metrics` and `/healthz` as an axum router on service-kit's
    HTTP server, with the exporter's timeouts, the listen-error prefix, the bind and the shared
    drain deadline.
  - `src/labels.rs` — sanitizing and validating custom label keys; `src/version.rs` — the build
    stamps (`service_kit::buildinfo!`).
  - `tests/` — the black-box tests: the Engine API wire captures (`engine_wire.rs`, against
    `tests/support/fake_engine.rs`), the metrics (`tests/metrics.rs`: the inventory against
    `tests/testdata/metrics/inventory.txt`, values, buckets, removal, scrape atomicity), the run
    lifecycle. The unit tests capture logs with `test_support::capture_logs` (a scoped
    `tracing_setup` subscriber).
  - `src/metrics.rs` — the building blocks over prometheus-client: `GaugeVec`, the snapshot
    collector and builder (families built whole per poll and swapped in at once), `ScrapeGauge`,
    and `render` (OpenMetrics text). The fixed families use prometheus-client's counters and
    histogram directly.
- `testenv/` — the DinD Swarm harness: the cluster, its bring-up and teardown and the suite's
  names (`config.rs`) over testenv-core, with bollard behind its `integration` feature (the
  exporter package names none of testenv, testenv-core and bollard in any table;
  `meta/tests/docker_boundary.rs` checks it),
  and the integration suite's runner and tests under `testenv/tests/integration`.
- `meta/` — the meta-tests, which check the repository rather than the exporter by running
  repo-meta's checks with this repository's parameters.
- `deployments/docker/` — the Dockerfile (`dhi.io` bases pinned by tag, deliberately not by
  digest: dhi.io rebuilds its tags too often; the `# syntax` frontend by digest; two targets on one
  `base` stage: `runtime`, the default, whose build stage cross-compiles a static musl binary with
  cargo-zigbuild, and `prebuilt`, which COPYs the release binary from the `dist` build context),
  compose examples and the socket-proxy allowlist.
- `.github/workflows/` — `ci.yaml` (`pre-commit-all`, `rust`, `image`, `conventional-commits` and
  the `ci-result` aggregator; the reviewdog jobs on pull requests), `integration.yaml` (the DinD
  Swarm suite, required by name), `release.yaml` (`guard`, `ci-green`, `audit`, `release`;
  [`docs/release.md`](docs/release.md)), `scheduled-scan.yaml` and `pre-commit-warmup.yaml`. Shared
  steps come from [gh-composite-actions](https://github.com/leinardi/gh-composite-actions), pinned
  by SHA. `.trivyignore.yaml` holds the scans' accepted findings.
- `docs/release.md` — how a release is cut and recovered, and what CI proves for it.
- `scripts/` — the licence-header check, `image-test.sh` (the image tests in three tiers: CI's full
  amd64 and reduced arm64 suites, and the release's smoke test), the make-common bootstrap and the
  Claude Code `rustfmt` hook (`scripts/hooks/rustfmt-on-edit.sh`, wired in `.claude/settings.json`).

### Shared code: rust-common

The generic code is [leinardi/rust-common](https://github.com/leinardi/rust-common)'s, a git
dependency pinned by the release tag (`tag = "v1.0.0"`, with `version = "1.0"`) in
`[workspace.dependencies]`: clock (the `Clock` trait),
tracing-setup (the logger's subscriber; the code logs with `tracing` macros), docker-client (the
Docker connection, over bollard), service-kit (the runtime, the signals, the stamps, the HTTP
server, the `process_*` collector), testenv-core (the harness's generic half) and repo-meta (the
meta-tests' checks). The exporter keeps only what is its own.

- Where this exporter behaves differently from another project, the difference is a parameter of
  the shared crate, passed from here, never a fork of the code into this repository.
- A fix to shared code lands in rust-common first; this repository then moves its pin: change
  the `tag` on every rust-common line, run `cargo fetch` (not `cargo update -p`), and check that
  `git diff Cargo.lock` touches only the rust-common lines.
- The parameters this exporter passes (the README's Migrating section lists what differs from 1.x
  and 2.x):
  the exposition (prometheus-client's OpenMetrics text, `process_*` from service-kit's
  `process-metrics`; Q2); the server's header-read (5s) and write-stall (15s) timeouts and the
  `http listen:` prefix (Q13);
  docker-client's `User-Agent` `swarm-scheduler-exporter/<version>` (Q11) and 30s
  response-header timeout (`RESPONSE_HEADER_TIMEOUT`); the log flags (Q4:
  `--log-format` text|json, `--log-level` debug|info|warn|error, an unknown value refused by clap),
  installed once through `tracing_setup::build` and `install_global`; `lazy_shutdown_signal`, whose
  handlers are installed only once the shutdown watch first polls it, so a signal during startup
  still terminates the process; the `buildinfo!`
  defaults `dev`, `none` and `unknown`, an empty stamp kept.
- Two workspace policies are the exporter's own: `panic = "unwind"` (the reconciler recovers a
  panicking step; see the invariants) and `unsafe_code = "forbid"`, with no exemption.

### Generated or vendored: never hand-edit

- `.mk/{docker,help,password,pre-commit,rust}.mk`, `.mk/.mk-common-version` and
  `scripts/bootstrap-mk-common.sh`: fetched from make-common; refresh them with
  `make mk-common-update` and port a change upstream.
- `tests/testdata/**` and `testenv/tests/integration/testdata/**`: Engine API fixtures (captured
  from a daemon) and golden files (the metrics golden was captured from the Go 1.3.2 exporter and
  is kept as a regression pin on values, not as a parity target). The integration golden file is
  rewritten with
  `make test-integration UPDATE=1 RUN=TestSnapshot_`; the whitespace hooks skip them.

## Invariants

The full list, with what counts as a finding, is in `.agents/skills/adversarial-review/SKILL.md` §3.
The ones to keep in mind while writing code:

- **Read-only against Docker, enforced in code.** A `:ro` socket bind does not make the API
  read-only. `DockerApi` exposes list, inspect and events calls only, and `src/docker` makes
  only the bollard read calls (the version negotiation's `GET /version` included), with
  `docker-client`'s `attach` feature off.
  `meta/tests/docker_boundary.rs` holds it there: no manifest enables that feature, only
  `src/docker` names the crate (and bollard, as `docker_client::bollard`) and builds no request of
  its own, the calls are only reads whose sections the shipped socket-proxy allowlist grants, and
  bollard (which the harness creates and removes things with) is declared directly only by the
  harness, outside the exporter package.
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
  drive it with tokio's paused clock. The time comes only through `clock::Clock`; the one
  wall-clock read is clock's `SystemClock`, in rust-common, so this repository has no
  `disallowed_methods` exemption.
- **Health means published data.** The health snapshot is evaluated at scrape time; nothing marks
  health from the poller itself.

## Conventions worth knowing

- The version strings come from `SWARM_SCHEDULER_EXPORTER_VERSION`, `_COMMIT` and `_DATE` at
  compile time (`src/version.rs`, `service_kit::buildinfo!`); unset, they read `dev`, `none` and
  `unknown`, and an empty one stays empty. `make rust-dist` and `make rust-run` set them from git;
  `make dist-static` from `DIST_VERSION`, `DIST_COMMIT` and `DIST_DATE`, which the release sets to
  the tag, the commit's first seven characters and its commit date; and the Dockerfile's build
  stage from its `VERSION`, `COMMIT` and `DATE` arguments, which also fill the image labels. The
  binary has no `--version`: it logs them in its startup line and exports them as
  `swarm_exporter_build_info`.
- Lints are silenced only with `#[expect(lint, reason = "...")]`: `allow_attributes` is denied,
  the reason is mandatory, and an `#[expect]` that stops firing fails the build.
- Fixtures under `testdata/` are captures (Engine API objects, golden files); the whitespace
  pre-commit hooks skip them, so do not tidy them by hand.
- A new `#[expect]`, `# shellcheck disable=` or `# droast ignore=` explains why the fix does not
  apply here, not which lint fired.
- An older test may carry its Go provenance (`/// Go: TestX`) as a doc comment; new tests do not
  need one, and sources name no Go source file.

## Licence headers

Every Rust and shell source, Dockerfile, Makefile and repo-local `.mk` snippet carries the MIT
header, with the year of the file's first commit (`scripts/check-license-headers.sh`, the
`check-license-headers` hook). The make-common files are exempt: they carry make-common's own.

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `**/*.rs`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` | `make verify`; after `testenv/**` or anything the integration suite exercises, `make test-integration` (Docker) |
| `Cargo.toml`, `Cargo.lock`, `deny.toml`, a rust-common `tag` bump | `make rust-deny`, which needs the network (cargo also fetches the git dependency) |
| `deployments/docker/Dockerfile`, `droast.toml`, `deployments/docker/Dockerfile.dockerignore` | `cargo test -p meta --test dockerfiles --test toolchain` (droast runs in `make check`); gh-composite-actions' `check-pinned-bases/check-pinned-bases.sh --tag-only dhi.io deployments/docker/Dockerfile`; `make dist-static`, then `make image-build-test`, and with QEMU `make image-build-test IMAGE_PLATFORM=linux/arm64 IMAGE_TEST_TIER=reduced`; `make docker-build` (all need a `dhi.io` login) |
| `scripts/image-test.sh`, `.mk/dist.mk`, `.mk/image.mk` | `make dist-static`, then `make image-build-test` (and the arm64 run above); keep `docs/release.md#what-ci-proves` true |
| `.trivyignore.yaml` | every entry scoped to paths or purls, with a statement and an `expired_at`; scan the prebuilt image with the latest Trivy as `trivy-gate` runs it (`--ignore-unfixed --severity HIGH,CRITICAL --ignorefile .trivyignore.yaml`) |
| `.github/workflows/**`, `.github/dependabot.yml` | `pre-commit run actionlint --all-files`, `cargo test -p meta --test toolchain --test dockerfiles`; reason through each trigger the workflow has (pull request, fork pull request, push, dispatch, schedule); for `release.yaml`, keep `docs/release.md` in step and dispatch a dry run once CI passed on the commit; for `ci.yaml`'s `image` job, keep `docs/release.md#what-ci-proves` true |
| `.pre-commit-config.yaml`, `Makefile`, `.mk/**` | `cargo test -p meta --test toolchain`, `make help` |
| `scripts/check-license-headers.sh` | `pre-commit run check-license-headers --all-files` |
| `meta/**` | `cargo test -p meta` |
| `**/*.md` | `pre-commit run lychee markdownlint-cli2 --files <file>`; prose wraps at 100 columns |

To work on a shared crate from here, add an uncommitted
`[patch."https://github.com/leinardi/rust-common"]` table to the root `Cargo.toml`, one
`<name> = { path = "../rust-common/crates/<name>" }` line per rust-common crate the workspace uses
(Cargo resolves two copies otherwise), and remove it before committing: CI cannot see the path.

## GitHub conventions

Shared by every `leinardi/*` repository.

- **Jobs:** every job sets `timeout-minutes` (a composite action cannot time out its own steps).
  Every checkout that pushes nothing sets `persist-credentials: false`; the release's checkout
  sets it too, and its tag push passes the token to that one `git push`. Every job that holds a
  write permission or `id-token` runs in the `release` environment.
- **Concurrency:** CI (and `integration.yaml`) groups a pull request's runs by
  `${{ github.workflow }} @ <head label>` and cancels the older one; every other run (a push to
  the default branch, a dispatch) gets its own group, `${{ github.run_id }}`, because GitHub keeps
  one pending run per group and the release gate needs CI on the exact commit (see `ci.yaml`). A
  warm-up uses `<name>-${{ github.ref }}`, never a static group. The release uses the one
  `release` group and is never cancelled.
- **Caches:** the default branch is the only writer, because every pull request can restore its
  caches and the default branch cannot restore a pull request's. The pre-commit cache is saved
  only by `pre-commit-warmup.yaml`; CI restores it with `actions/cache/restore` under the same key,
  `pre-commit|<OS>|<resolved Python version>|<hash of .pre-commit-config.yaml>`, so CI sets up the
  warm-up's `python_version` and installs its pre-commit version. The Rust cache is
  `Swatinem/rust-cache` with `shared-key: ci`, saved on `master` only (the `image` job under its
  own `key: static`, `integration.yaml` under `key: integration`); the release writes its own,
  `prefix-key: release`. No image build uses a layer cache: the prebuilt target is the base plus
  one COPY.
- **pre-commit** is pinned to the same version in CI, the warm-up and the
  `gha-pre-commit-reviewdog-actions` jobs (their `pre-commit-version` input).
- **Release:** `release.yaml` is a `workflow_dispatch` with an optional `version` and a `dry_run`,
  built from gh-composite-actions (`release-version`, `require-ci-green`, `release-mode`,
  `toolchain-record`, `toolchain-recover`, `check-pinned-bases`, `trivy-gate`,
  `binary-attestation-status`, `setup-zigbuild`, `setup-cargo-deny`). svu derives the version from
  the commits only when none is given, and is installed only then, so a version entered by hand
  releases even when GitHub's release downloads fail. Trivy is never pinned: `trivy-gate` installs
  its latest release.
- **Dependabot:** monthly, `cooldown: default-days: 7`, labels `dependencies` and `ci`, one group
  per ecosystem. The shipped ecosystems use `fix(deps)`: `cargo` (with `prefix-development:
  chore(deps)` and separate production and development groups) and `docker` (the runtime base's
  tag); GitHub Actions and pre-commit use `chore(deps)`.
- **Issue and pull request templates:** keep the Step 1/2/3 structure of template-default, with
  this repository's project name, version command, environment block, snippet language and extra
  checkboxes. The pull request checklist always keeps the Conventional Commits and
  validation-matrix items.

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
