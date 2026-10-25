# swarm-scheduler-exporter — core

Read-only Prometheus exporter for Docker Swarm's scheduler, in Go: desired, schedulable and
running replicas per service (accurate eligibility for `global` services), task state, service
update/rollback state, node availability and, opt-in, container state. Fork of
akerouanton/swarm-tasks-exporter.

AGENTS.md (repo root) is the authoritative contract: layout, commands, invariants, validation
matrix, commit types. Read it before any change; memories below only add what it does not say.
A Go-to-Rust port lands phase by phase on `feat/rust-rewrite`; Go stays the shipping product until
its single breaking cutover commit (load the `go-to-rust-port` skill for that work).

## Source map

- `cmd/swarm-scheduler-exporter/`: `main.go` (flags, `run()` order, worker start, HTTP server,
  `-healthcheck`), `docker_client.go` (transport, TLS from `DOCKER_CERT_PATH`, timeouts).
- `internal/collector/`:
  - `reconciler.go`: the single owner of the service/node caches and derived gauges (cycle,
    resync, retries, backoff); `reconciler_poll.go`: the poll snapshot/verdict exchange.
  - `desired_replicas.go`: events listener (since/reconnect), placement eligibility,
    desired/schedulable gauges; `replicas_state.go`: task counting and `at_desired`.
  - `service_update.go`, `nodes.go`, `containers.go` (opt-in), `health.go`,
    `exporter_metrics.go`, `snapshot_gauge.go` (families built whole, swapped in one step).
  - `docker_api.go`: the read-only `DockerAPI` interface every collector call goes through.
- `internal/labels` (custom label keys), `internal/logger` (slog text/json/plain),
  `internal/server` (`/metrics`, `/healthz`).
- `internal/testenv` + `test/integration`: `integration`-tagged DinD Swarm suite that runs the
  built binary (`SSE_IT_BINARY`), not the exporter in-process.
- `test/` (outside `test/integration`): meta-tests over the repository, no Docker needed.
- User-facing contract: the README Metrics section (and alert examples).

## Non-obvious anchors

- Engine wire behaviour is pinned by `internal/collector/engine_wire_test.go` against byte captures
  in `internal/collector/testdata/engine_wire/` (allowances W1–W5); its `metrics.golden` and the
  integration `steady_state.golden` are rewritten only by their update flags.
- Health is evaluated at scrape time from published data: window `max(3×poll-delay, 30s)`.
- Events resume from the last event's `timeNano` minus 500ms; every reconnect requests a resync.

Further: tooling and versions in `mem:tech_stack`; commands in `mem:suggested_commands`; Go style
and lint traps in `mem:conventions`; done-criteria in `mem:task_completion`.
