# swarm-scheduler-exporter — core

Read-only Prometheus exporter for Docker Swarm's scheduler, in Rust: desired, schedulable and
running replicas per service (accurate eligibility for `global` services), task state, service
update/rollback state, node availability and, opt-in, container state. Fork of
akerouanton/swarm-tasks-exporter; a port of its Go 1.x implementation, metric for metric.

AGENTS.md (repo root) is the authoritative contract: layout, commands, invariants, validation
matrix, commit types. Read it before any change; memories below only add what it does not say.
The README "Migrating from 1.x" section lists the only intended differences from Go (Q1-Q13).

## Source map

- `src/main.rs` (hand-built tokio runtime, one shutdown budget), `src/run.rs` (startup order,
  workers, HTTP server), `src/cli.rs` (clap, double-dash flags), `src/healthcheck.rs`
  (`--healthcheck`), `src/version.rs` (compile-time stamps), `src/clock.rs` (the only wall clock).
- `src/docker/`: the in-tree read-only Docker client (transport, ping and version negotiation, TLS,
  endpoints, events stream, API types). GET only, plus `HEAD /_ping`.
- `src/collector/`:
  - `reconciler/`: the single owner of the service/node caches and derived gauges (`mod.rs`
    cycle, resync, retries; `poll.rs` the poll snapshot/verdict exchange; `listener.rs` the
    events listener, since/reconnect; `recover.rs` panic recovery).
  - `desired.rs` (placement eligibility, desired/schedulable), `replicas_state.rs` (task counting,
    `at_desired`), `service_update.rs`, `nodes.rs`, `containers.rs` (opt-in), `health.rs`,
    `exporter_metrics.rs`.
- `src/metrics/`: in-tree registry and families, the Go-compatible text encoder (`text.rs`), the
  promhttp negotiation (`exposition.rs`), process and promhttp collectors, snapshot families.
- `src/go_compat/`: Go library behaviour ported for byte parity (durations, floats, quoting,
  time layouts, sort, strings, net/ip, host:port).
- `src/labels.rs` (custom label keys), `src/logger/` (slog-compatible text/json/plain),
  `src/server.rs` (`/metrics`, `/healthz` on hyper).
- `tests/`: `engine_wire.rs` (fake Engine replaying `tests/testdata/engine_wire/`),
  `exposition.rs` (byte diff against the Go `/metrics` capture), `run.rs` (lifecycle, flags).
- `testenv/`: the DinD Swarm harness; bollard and every Docker-driving module behind its
  `integration` feature; `tests/integration/` is the `harness = false` suite that runs the release
  binary (`SSE_IT_BINARY`).
- `meta/`: meta-tests over the repository (toolchain pins, Dockerfile, docs links, licence
  headers, suppression ratchet, Docker SDK boundary), no Docker needed.
- User-facing contract: the README Metrics section (and alert examples).

## Non-obvious anchors

- `tests/testdata/**` and `testenv/tests/integration/testdata/**` are byte-for-byte captures and
  golden files: never hand-edit; `metrics.golden` is Go's, `steady_state.golden` is rewritten only
  by `make test-integration UPDATE=1 RUN=TestSnapshot_`.
- Health is evaluated at scrape time from published data: window `max(3×poll-delay, 30s)`.
- Events resume from the last event's `timeNano` minus 500ms; every reconnect requests a resync.

Further: tooling and versions in `mem:tech_stack`; commands in `mem:suggested_commands`; Rust
style and lint traps in `mem:conventions`; done-criteria in `mem:task_completion`.
