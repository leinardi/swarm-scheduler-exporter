# swarm-scheduler-exporter — core

Read-only Prometheus exporter for Docker Swarm's scheduler, in Rust: desired, schedulable and
running replicas per service (accurate eligibility for `global` services), task state, service
update/rollback state, node availability and, opt-in, container state. Fork of
akerouanton/swarm-tasks-exporter; a Rust rewrite of its Go 1.x implementation (same metrics),
since moved onto shared crates.

AGENTS.md (repo root) is the authoritative contract: layout, commands, invariants, validation
matrix, commit types. Read it before any change; memories below only add what it does not say.
The README "Migrating" section lists the intended differences from 1.x (Go) and 2.x (Q1-Q17).

## Source map

- Shared code is rust-common's (git dependency pinned by tag `v0.4.0`): clock,
  tracing-setup, docker-client, service-kit, testenv-core,
  repo-meta. A per-project difference is a parameter there, never a fork here; fixes land in
  rust-common first.
- `src/main.rs` (service-kit's runtime and lazy shutdown signal, one shutdown budget), `src/run.rs`
  (startup order, workers, HTTP server), `src/cli.rs` (clap, double-dash flags),
  `src/healthcheck.rs` (`--healthcheck`), `src/version.rs` (`service_kit::buildinfo!` stamps).
- `src/docker/`: `DockerApi` (read-only trait) on a `Client` over docker-client/bollard (`tcp`,
  `tls`), the API types (`convert.rs` maps bollard's models) and timeouts. Read calls only, plus
  the `GET /version` negotiation.
- `src/collector/`:
  - `reconciler/`: the single owner of the service/node caches and derived gauges (`mod.rs`
    cycle, resync, retries; `poll.rs` the poll snapshot/verdict exchange; `listener.rs` the
    events listener, since/reconnect; `recover.rs` panic recovery).
  - `desired.rs` (placement eligibility, desired/schedulable), `replicas_state.rs` (task counting,
    `at_desired`), `service_update.rs`, `nodes.rs`, `containers.rs` (opt-in), `health.rs`,
    `exporter_metrics.rs`.
- `src/labels.rs` (custom label keys), `src/server.rs` (`/metrics`, `/healthz`: an axum
  router on service-kit's http server). Metrics are prometheus-client's (OpenMetrics text,
  `src/metrics.rs`), the logger tracing-setup's (`tracing` macros, clap-parsed flags); durations
  are humantime's, CIDRs ipnet's, `host_port.rs` and `text.rs` are the exporter's own.
- `tests/`: `engine_wire.rs` (fake Engine replaying `tests/testdata/engine_wire/`),
  `metrics.rs` (inventory, values, buckets, removal, atomicity), `run.rs` (lifecycle, flags).
- `testenv/`: the DinD Swarm harness over testenv-core; bollard and every Docker-driving module
  behind its `integration` feature; `tests/integration/` is the `harness = false` suite that runs
  the release binary (`SSE_IT_BINARY`).
- `meta/`: meta-tests over the repository, repo-meta's checks with this repository's parameters
  (toolchain pins, Dockerfile, licence headers) plus the local
  Docker SDK boundary, no Docker needed.
- User-facing contract: the README Metrics section (and alert examples).

## Non-obvious anchors

- `tests/testdata/**` and `testenv/tests/integration/testdata/**` are byte-for-byte captures and
  golden files: never hand-edit; `metrics.golden` is Go's, `steady_state.golden` is rewritten only
  by `make test-integration UPDATE=1 RUN=TestSnapshot_`.
- Health is evaluated at scrape time from published data: window `max(3×poll-delay, 30s)`.
- Events resume from the last event's `timeNano` minus 500ms; every reconnect requests a resync.

Further: tooling and versions in `mem:tech_stack`; commands in `mem:suggested_commands`; Rust
style and lint traps in `mem:conventions`; done-criteria in `mem:task_completion`.
