---
name: rust-style-guide
description: >
  Rust coding rules for swarm-scheduler-exporter: the workspace lint set (clippy pedantic plus the
  restriction lints) and how to satisfy it, #[expect] with a reason as the only suppression, the
  maintainer's rules (descriptive names, exhaustive matches, public items first, no panics or
  printing outside tests), the helpers to reuse, and the exporter's patterns (metric families and
  snapshot publishing, the reconciler's ownership of Swarm state, the poll protocol, the
  slog-compatible logger, the read-only Docker client, the injected clock, concurrency and
  tests). Use when writing, editing or reviewing any .rs file in swarm-scheduler-exporter;
  consult it before generating Rust code, not after clippy fails.
---

# Rust Style Guide — swarm-scheduler-exporter

Rules from the root `Cargo.toml` (`[workspace.lints]`), `clippy.toml`, `rustfmt.toml` and the
existing code in `src/`, `tests/`, `testenv/` and `meta/`. §1–§4 are what the lints enforce,
§5–§7 are the maintainer's rules, §8–§13 are this exporter's patterns, §14–§16 are review rules.

Every lint and test command runs workspace-wide, so the three members (the exporter, `testenv`
and `meta`) are held to the same set:
`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo fmt --all --check`. Never commit with `--no-verify`;
fix the underlying issue instead.

---

## 1. The lint set

`[workspace.lints]` is declared once, and every member opts in with `[lints] workspace = true`
(a member does not inherit it otherwise; `meta/tests/workspace_lints.rs` fails when one does not).
`clippy.toml` sits at the workspace root, so its settings apply to every member.

| Lint | Level | What to do |
| --- | --- | --- |
| `unsafe_code` | forbid | No `unsafe`, anywhere |
| `clippy::pedantic` | warn (priority -1), an error under `-D warnings` | Satisfy it; the usual ones below |
| `allow_attributes`, `allow_attributes_without_reason` | deny | `#[expect(lint, reason = "...")]` only (§2) |
| `unwrap_used`, `expect_used` | deny | Return an error; tests may use both (§3) |
| `print_stdout`, `print_stderr` | deny | Log through the `Logger`; `writeln!` to a handle when output is the point (`--help`, `--healthcheck`) |
| `wildcard_enum_match_arm` | deny | Name every variant (§6) |
| `min_ident_chars` | deny | Descriptive names (§5) |
| `disallowed_methods` | deny | No wall clock outside `src/clock.rs` (§8) |

The pedantic lints that come up most:

- `must_use_candidate`: put `#[must_use]` on a pure function or constructor that returns a value.
- `missing_errors_doc`, `missing_panics_doc`: a public function returning `Result` has an
  `# Errors` section; one that can panic has `# Panics` (better: make it not panic).
- `doc_markdown`: identifiers in doc comments go in backticks (`` `service_id` ``,
  `` `DockerApi` ``); `clippy.toml`'s `doc-valid-idents` adds only `OpenMetrics`.
- `too_many_lines` (100): split the function into named steps.
- `needless_pass_by_value`, `trivially_copy_pass_by_ref`: borrow what you only read; pass small
  `Copy` types by value.
- `cast_possible_truncation`, `cast_precision_loss`, `cast_sign_loss`: use `try_from` or `From`;
  where the cast is the point (Go's float and duration formatting in `src/go_compat`),
  `#[expect]` it with the reason the range is safe.

`rustfmt.toml` sets `edition = "2024"` and `max_width = 100`. The `rustfmt-on-edit.sh` hook
formats a `.rs` file after every edit in Claude Code, and the `cargo-fmt` pre-commit hook checks.
rustfmt does not wrap comments: keep them at 100 columns by hand.

## 2. Suppressions: `#[expect]` with a reason, counted

- Only `#[expect(clippy::lint, reason = "...")]`. `#[allow]` is denied, and so is either
  attribute without a reason. An `#[expect]` fails the build once the lint stops firing, so a
  stale suppression cannot linger.
- Put it on the smallest item that needs it: the statement or the function, not the module. A
  file-level `#![expect(...)]` is for an integration-test file whose helpers all share the
  reason (`expect_used` in helpers that are not `#[test]` functions, which clippy's test
  allowance does not cover).
- The reason says why the fix does not apply here, not which lint fired.
- Suppressions are ratcheted: `scripts/check-suppressions.sh` counts every lint of an
  `#[expect]` or `#[allow]` against `.suppressions-baseline` and refuses a commit that adds one
  without changing the baseline. After adding, moving or removing one: stage it, run
  `scripts/check-suppressions.sh --update`, and commit the baseline with it. Raising a count
  needs a reviewer's agreement.

## 3. Errors

- Errors are hand-written types with `Display` and `std::error::Error` (there is no
  `thiserror` or `anyhow`), each carrying its source: `docker::Error` for the client,
  `collector::error::CallError` for a collector's Docker call, `RegisterError`,
  `LabelsMismatch` for the metrics.
- The wording is part of the contract (Q9): every prefix and message the exporter writes itself
  (`service list:`, `service inspect <id>:`, `events stream error:`, `http listen:`,
  `docker client:`) stays as 1.x wrote it. Wrap with `CallError::new(context, source)`; a test
  pins any new text.
- Classify by method, not by string: `docker::Error::is_not_found` and friends.
- No `unwrap`/`expect` outside tests. A `Mutex` is read with
  `.lock().unwrap_or_else(PoisonError::into_inner)`: a step that panicked was recovered (§9),
  and its state is still consistent enough to carry on from.
- The one deliberate panic is `collector::must`, the Prometheus client's `With` contract (label
  values that do not fit the family); the reconciler's step recovery catches it.
- A failure the exporter can carry on from (one inspect, one poll) is logged with the error as an
  `err` attribute and the work continues; a startup failure returns an exit code from `run`.
  `main` is the only place that exits the process.

## 4. Documentation and comments

- Every module starts with a `//!` comment saying what it owns; every public item has a `///`
  comment starting with what it is or does.
- Every `.rs` file starts with the MIT licence block (`Copyright (c) 2026 Roberto Leinardi`, the
  year of the file's first commit), then the module comment.
- A comment says **why the code is the way it is**; history and "changed X to Y" belong in the
  commit body.
- Where the Rust deliberately differs from 1.x (Q1–Q13 in the README's migration section), the
  module comment names the difference; where it ports a Go behaviour on purpose
  (`src/go_compat`), it says which and why the contract depends on it. Sources never name a Go
  file; a ported test's provenance is its `Go:` doc comment (§12).
- `TODO` is allowed; a comment on code you did not otherwise change is not.

## 5. Names

- `min_ident_chars` refuses single-character names. Name a binding for what it holds at any
  distance: `index`, `service_id`, `node`, `labels`, `families`, never `i`, `s`, `n`. Closure
  parameters too (`|service| ...`, not `|s| ...`).
- Error bindings name their source when several are in scope: `list_error`, `inspect_error`.
- Metric and label names are constants (`NAMESPACE`, `SERVICE_SUBSYSTEM`, `LABEL_SERVICE`,
  `LABEL_STATE` in `src/collector/metrics_ids.rs`; a label one family uses sits beside it, as
  `LABEL_NODE_ROLE` in `nodes.rs`). Never hardcode `"swarm_service_"` at a call site.

## 6. Matches, item order, types

- **Exhaustive matches.** A `match` on an enum names every variant (`wildcard_enum_match_arm`);
  group them with `|` when they share an arm. A new variant is then a compile error everywhere it
  matters. Matching on integers or strings may keep a `_` arm.
- **Item order.** Within an `impl`, public items come before private ones. Not linted: checked in
  review.
- **Open Docker strings.** Engine API enums (task states, node availability, update states) are
  open sets: decode them as strings and compare against the known constants
  (`KNOWN_TASK_STATES`, `KNOWN_CONTAINER_STATES`), so an unknown value still flows through as
  1.x let it.
- **Wire types.** `src/docker/types` mirrors the Engine API's keys (`#[serde(rename = ...)]`),
  defaults what Go's zero value would fill, and is pinned by the byte-for-byte captures in
  `tests/testdata/engine_wire`. A field that changes the decoded shape updates those tests.

## 7. Reuse before writing

| Need | Use | Not |
| --- | --- | --- |
| The current time | the injected `Arc<dyn Clock>`; `TokioClock` (follows the paused clock) or `FakeClock` in tests | `Timestamp::now()`, `SystemTime::now()`, `Instant::now()` |
| A Docker call | a `DockerApi` method, under `with_docker_timeout` | a request built at the call site |
| A family rebuilt on every update | `SnapshotFamily` / `SnapshotCollector` with a `SnapshotBuilder` | `GaugeVec` reset and re-set |
| A family updated per series, from events | `GaugeVec`, with `delete` on removal | a snapshot rebuilt per event |
| Label values of a service | `labels_for_metadata` and the metadata cache | assembling stack/service/mode by hand |
| A custom label key | `labels::sanitize_label_name` and the validation in `src/labels.rs` | your own regex |
| Go-compatible formatting (durations, floats, `Quote`, host and port) | `src/go_compat` | `format!` that only looks right |
| A log line | `Logger::{debug,info,warn,error}` with `Attr` values | `format!` in the message |
| Captured logs in a unit test | `test_support::capture_logs` | a sink of your own |
| A bounded wait in a unit test | `test_support::eventually` | a loop with `sleep` |
| A fake daemon in a unit test | `test_support::FakeDocker` | a real socket |
| A fake daemon on the wire | `tests/support/fake_engine.rs` (`FakeEngine::start`, `script_events`, `phase_requests`) | a hand-rolled server |
| An integration wait | `wait.rs` in `testenv/tests/integration` (`want_positive`, `want_absent`, `consistently`, `wait_for_fresh_poll`) | `tokio::time::sleep` |

## 8. Time

- `clippy.toml`'s `disallowed-methods` bans `jiff::Timestamp::now`, `jiff::Zoned::now`,
  `std::time::SystemTime::now` and `std::time::Instant::now`. Only `src/clock.rs` reads them,
  behind an `#[expect(clippy::disallowed_methods, reason = "...")]`; everything else takes an
  `Arc<dyn Clock>`. `tokio::time::Instant` stays allowed: it follows the paused clock, so it is
  what deadlines (`with_deadline`, the shutdown budget) are measured with.
- Timestamps exported as gauges (`last_poll_success_timestamp_seconds`, update timestamps) come
  from the `Clock` or from the Docker object, never from a second clock read.
- Durations on the command line and in the environment use Go's syntax
  (`go_compat::duration`), as 1.x did.

## 9. The reconciler owns Swarm state

- The reconciler task is the only writer of the service metadata cache, the node cache and every
  gauge derived from them (desired replicas, schedulable nodes, update state, nodes by state).
  The event listener only marks keys dirty through a `ReconcilerHandle`, under one short lock and
  without blocking, so the event stream is always drained.
- The dirty set, the service generations and the resync counters share one lock
  (`Shared::state`). Hold it briefly and never across an `.await`; copy what you need out.
- The dirty set is bounded: an overflow drops the keys and requests a full resync instead of
  growing. Inspects per cycle are bounded, retries back off, and any map keyed by an external ID
  has an eviction path (`prune_service_generations`).
- Each step runs under `catch_unwind` (`reconciler/recover.rs`): a panic logs
  `reconciler recovered from panic` with the payload and a backtrace and requests a resync,
  rate-limited. That is why the release profile keeps `panic = "unwind"`; never switch it.
- A removed service's series are deleted (`delete_service_series`), never set to 0.

## 10. The poll protocol and metric publishing

- The poller asks the reconciler for an immutable polling snapshot, lists and counts tasks
  against it without touching the caches, and submits the counts
  (`reconciler/poll.rs`). The reconciler publishes a service's counts only if nothing about it
  changed in between (epoch, generation, mode, placement, scale, a job's iteration); otherwise
  the counts are rejected, and a third consecutive rejection omits the service and fails the
  poll. A new field that affects counting joins that comparison.
- Families computed from one poll are published together by one `SnapshotCollector`, so a
  scrape never pairs values from two polls. Build the series off to the side with a
  `SnapshotBuilder`, call `build()` once and `publish` the result; on a build error, log and
  keep the previous set. The build function is pure.
- Categorical gauges emit every known state per subject, zeros included; services without tasks
  emit zeros; absent node combinations emit nothing.
- Custom `--label` keys are appended after the base labels and sanitized before the family is
  created; values are never sanitized. No label takes an unbounded value (task or container IDs,
  timestamps).
- A renamed metric or a changed label set updates the README's Metrics section and any alert
  example in the same change; `tests/exposition.rs` and the snapshot golden file pin the output.

## 11. Logging

- The logger (`src/logger`) reproduces slog's text and JSON handlers and the plain format byte
  for byte. Call `logger.warn("message", &[Attr::string("service_id", id), Attr::error("err",
  &error)])`; the error key is always `err`. Never build the message with `format!`.
- Messages are the 1.x messages, lower case, without a module prefix
  (`container inspect failed; using base state`, `event stream ended; will reconnect`); a test
  that matches logs matches them exactly.
- Levels are slog's (debug -4, info 0, warn 4, error 8). A level is part of the contract: an
  event stream the server closed cleanly reconnects at info, one that ended any other way (an
  idle stream HAProxy cut without ending the chunked body included) at warn, as 1.x did.
- Never log credentials or full service specs; container and task IDs are fine in a log line,
  never in a label.

## 12. The Docker client

- `src/docker` is the exporter's own HTTP/1 client on hyper: unix socket, TCP and TLS (rustls
  with ring, the system roots plus `ca.pem`). It sends `GET` only, plus one `HEAD /_ping`, builds
  every request in one place, and asks only for paths the shipped socket-proxy allowlist grants;
  `meta/tests/docker_boundary.rs` enforces all three. A new call is a `DockerApi` method with a
  read-only verb, an allowlisted path and a wire test in `tests/engine_wire.rs`.
- Every request runs under `with_docker_timeout`, except the event stream, which has no deadline
  and is cancelled on shutdown or reconnect.
- The request details are moby's: the version prefix, the lazy negotiation, the `Host` header,
  the query encoding and the error wording. The User-Agent is the exporter's (Q11).
- bollard is the test harness's SDK and stays in `testenv`, behind its `integration` feature.

## 13. Concurrency, shutdown and tests

- Cancellation is a `tokio_util::sync::CancellationToken` tree from `run`. Shutdown has one
  budget, `SHUTDOWN_BUDGET` (10 s) from the signal, covering the HTTP drain, the worker join and
  `Runtime::shutdown_timeout(remaining)`; each step gets only what remains.
- Shared state sits behind a `std::sync::Mutex` (held briefly, never across an `.await`) or an
  atomic; readers get copies.
- Unit tests sit beside the code (`#[cfg(test)] mod tests` or a `tests.rs` in the module
  directory); black-box tests are in `tests/`; the integration suite is in
  `testenv/tests/integration`, behind the `integration` feature; meta-tests are in `meta/tests`.
- A test ported from Go carries a doc comment naming the Go function it replaces:
  ``/// Go: `TestReconciler_BurstCoalesces` ``. Integration tests keep their Go names, which the
  `RUN` filter selects.
- Time-dependent tests run on a paused runtime (`#[tokio::test(start_paused = true)]`) with a
  `TokioClock`. Never `std::thread::sleep`.
- Waiting, by class:
  - **Positive eventual:** poll with `eventually` (unit) or `want_positive` (integration), under a
    bound, and fail naming what never happened. Under a paused clock an unbounded wait spins
    forever instead of failing.
  - **Negative assertion:** give the wrong behaviour a bounded window (`consistently`), then
    assert it did not happen, and say so in a comment.
  - **Real elapsed time:** only when the duration is the point (the shutdown budget), as a named
    constant.
- A meta-test gated on a tool returns early through `meta::toolgate::lookup` when the tool is
  missing, unless `SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS=1`, which CI sets. Every meta-test has
  a negative control: a check that passes on an empty input is no check.

## 14. What to avoid

- `unwrap`/`expect`, `println!`/`eprintln!`, `unsafe`, or a wall-clock read outside the places
  above.
- `#[allow]`, or an `#[expect]` whose reason restates the lint (§2).
- A `_ =>` arm on an enum (§6).
- A second Docker request path, snapshot builder, label sanitizer or test harness when §7 has
  one.
- A write to the caches or derived gauges from anywhere but the reconciler task (§9).
- A non-`GET` request, or a Docker crate in the exporter package (§12).
- Designing for hypothetical requirements: no configurability or abstractions for features that
  do not exist yet.
- Skipping or suppressing pre-commit hooks (`--no-verify`).

## 15. Quick checklist before submitting Rust code

- [ ] `cargo fmt --all` clean; clippy clean with `--workspace --all-targets --all-features -- -D warnings`
- [ ] No `unwrap`/`expect`/printing outside tests; errors carry their source and keep 1.x wording
- [ ] Each `#[expect]` has a reason saying why the fix does not apply; baseline updated (§2)
- [ ] Descriptive names; every enum variant matched by name; public items first
- [ ] `#[must_use]`, `# Errors` and backticked identifiers where pedantic asks
- [ ] Checked §7 for an existing helper; time from the `Clock`
- [ ] Rebuilt families published as one snapshot; removed series deleted; README updated for any
      metric change
- [ ] Ported tests carry their `Go:` comment; no unbounded wait under a paused clock

## 16. Lint and test loop

1. `make rust-fmt` (or let the edit hook do it).
2. `make verify`: `rust-build`, `rust-fmt-check`, `rust-clippy` and `rust-test` over the
   workspace, the meta-tests included.
3. Fix each report and re-run until clean. For a single test:
   `cargo test --locked --lib collector::reconciler::tests::<name>`.
4. After a change the integration suite exercises (`src/collector`, `src/docker`, `testenv`):
   `make test-integration` (Docker with privileged containers).
5. `make check-stage` (or `make check`), then `git status`: hooks may rewrite files in place. If
   you added, moved or removed a suppression, run `scripts/check-suppressions.sh --update` and
   stage `.suppressions-baseline`.
