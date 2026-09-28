# AGENTS.md

## What this is

A Prometheus exporter for Docker Swarm's scheduler: desired and running replicas per service (with accurate eligibility for
`global` services), live task state, service update/rollback state, node availability and, opt-in, container state. It only ever
reads from the Docker API. Fork of [akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter). The
README is the user-facing story; the metrics it lists are the public contract.

## Common commands

Build / test / vet use Make wrappers around `go`:

```bash
make go-build             # CGO_ENABLED=0 build into ./dist/
make go-test              # go test -race ./...
make go-vet
make go-tidy              # go mod tidy + go mod verify
make audit-deps           # govulncheck over every package (network required); also runs in CI and as a pre-commit hook on go.mod/go.sum changes
make check                # pre-commit on all files
make check-stage          # pre-commit on the staging area only
make docker-build
make docker-run           # runs the image locally, binding the Docker socket and the metrics port
```

Integration tests (a throwaway DinD Swarm on your Docker daemon, privileged containers required; see
[`CONTRIBUTING.md`](CONTRIBUTING.md#integration-tests)):

```bash
make go-test-integration                            # the whole suite
make go-test-integration RUN='^TestNode_'           # a subset
make go-test-integration UPDATE=1 RUN=TestSnapshot_ # rewrite the golden files
make go-vet-integration                             # go vet including the integration-tagged code
make sweep-test-leaks                               # remove every labelled test container and network
```

Single test:

```bash
go test ./internal/collector -run TestReconciler -v
```

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run. To refresh: `make mk-common-update`.
Project targets live in the local `.mk/*.mk` files listed in `MK_LOCAL_FILES`, never as recipes in the Makefile.

## Layout

- `cmd/swarm-scheduler-exporter/` — flags, logging, the Docker client with timeouts, the HTTP server, and `-healthcheck` (the
  image is distroless: there is no shell or curl a `HEALTHCHECK` could run instead).
- `internal/collector/` — the collectors and the `Reconciler`, which is the one owner of the service and node caches and of every
  gauge derived from them. `docker_api.go` is the read-only `DockerAPI` interface all collector code reaches Docker through.
- `internal/labels/` — sanitizing and validating custom Prometheus label keys.
- `internal/logger/`, `internal/server/` — slog setup; `/metrics` and `/healthz`.
- `internal/testenv/`, `test/integration/` — the DinD Swarm harness and the `integration`-tagged suite.
- `deployments/docker/` — the Dockerfile (DHI bases, pinned by digest), compose examples and the socket-proxy allowlist.
- `docs/release.md` — how a release is cut and recovered.

## Invariants

The full list, with what counts as a finding, is in `.agents/skills/adversarial-review/SKILL.md` §3. The ones to keep in mind
while writing code:

- **Read-only against Docker, enforced in code.** A `:ro` socket bind does not make the API read-only. `DockerAPI` exposes list,
  inspect and events calls only, and the depguard rule `docker-sdk-boundary` in `.golangci.yaml` keeps Docker SDK imports inside
  `internal/collector` and `cmd/swarm-scheduler-exporter`.
- **Metrics are the public contract.** A renamed metric or a changed label set updates the README's Metrics section (and any
  alert example) in the same change. No label fed by an unbounded value.
- **Series lifecycle.** A removed resource's series are deleted, never set to 0; known state sets are emitted exhaustively;
  snapshot families are built whole and swapped in one step.
- **One owner for Swarm state.** Only the `Reconciler` goroutine writes the caches and derived gauges; the event dispatcher only
  marks keys dirty. Lock order is `Reconciler.mu`, then `metadataMu`.
- **Bounded work.** Bounded dirty-key set, bounded inspects per cycle, retries with delays, every request under
  `withDockerTimeout`, and no map keyed by an external ID without an eviction path.
- **Health means published data.** `HealthSnapshot` is evaluated at scrape time; nothing marks health from the poller itself.

## Conventions worth knowing

- Version strings (`version`, `commit`, `date`) live in `cmd/swarm-scheduler-exporter/version.go` and are filled by
  `-ldflags -X main.version=...` from `GO_LDFLAGS` in `.mk/go.mk`.
- Every Go file carries the MIT license header, after the `//go:build` line where there is one.
- Fixtures under `testdata/` are byte-for-byte captures (Engine API wire captures, golden files); the whitespace pre-commit hooks
  skip them, so do not tidy them by hand.
- A new `//nolint`, `# shellcheck disable=` or `# hadolint ignore=` explains why the fix does not apply here, not which rule
  fired.

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load them before the work, not after review:

- `go-style-guide` — before any `.go` edit.
- `adversarial-review` — for any review request ("review my diff", "is this ready to merge").

## Commit messages

All commits MUST be Conventional Commits 1.0.0 **with a scope**: `<type>(<scope>)[!]: <description>`, optional blank-line body and
footers. Enforced by the `conventional-pre-commit` `commit-msg` hook (`--force-scope`) and by the `conventional-commits` CI job.
Types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`, `revert`. Breaking changes use `!` before
`:` or a `BREAKING CHANGE:` footer. Release notes are not built from these messages: `gh release create --generate-notes` lists the
merged pull requests by title. Examples: `fix(collector): drop stale task metrics when a service is removed`,
`ci(release): pin trivy`.

Release versions are derived by `svu` from the commits since the last tag, so a wrong type ships a wrong version:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(metrics)!: rename the replica gauges` |
| minor | `feat` | `feat(collector): add node labels` |
| patch | `fix` | `fix(collector): skip removed services` |
| none | everything else: `perf`, `refactor`, `build`, `ci`, `chore`, `docs`, `style`, `test`, `revert` | `perf(collector): ...` |

The highest bump among the commits wins; with only "none" commits since the last tag, a release with no version fails with
"nothing to bump".

**Pick the type by whether the change should ship, not by what kind of change it is.** Anything that changes the shipped binary or
image and that users should receive is `fix` (or `feat`), even when it is a performance improvement, a refactor or a revert. Use
`perf`, `refactor`, `style` and `revert` only when the commit is deliberately not meant to trigger a release on its own. A `revert`
of a shipped `feat` or `fix` is itself a `fix`. `svu` matches `feat`/`fix` anywhere in the subject (e.g. `prefix:` counts as
`fix:`), so avoid a word ending in `feat` or `fix` directly before a colon in other subjects. PRs land as merge commits, so every
commit counts, not just the PR title. See [`docs/release.md`](docs/release.md).
