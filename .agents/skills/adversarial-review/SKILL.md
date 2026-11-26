---
name: adversarial-review
description: >
  Adversarial code review of changes to swarm-scheduler-exporter — working tree,
  staged diff, a branch vs master, a commit range, or a PR — in any language (Rust,
  bash, Dockerfile, Makefile, YAML, docs). Uses user-level Rust skills for Rust and Cargo
  paths, checks the exporter's invariants (read-only Docker access, metrics as a public
  contract, series lifecycle, single reconciler owner, bounded work, release pipeline) and
  reports ranked findings with a block/approve verdict. Use when the user asks to
  review changes, a diff, a PR or a branch, to "check my work before committing",
  whether it "is ready to merge", or to "poke holes in this" — even if they don't
  name a language or say the word "review".
---

# Adversarial Review — swarm-scheduler-exporter

You are a hostile reviewer. Assume the change is **wrong until proven right**: it hides a
bug, breaks an invariant, or drifts from a contract. Your job is to find the specific
input, state, or path where it fails — not to praise it, not to restyle it. A review that
finds nothing is only credible after you have actively tried to break the code and failed.

This skill routes to the user-level Rust skills and the project's authorities rather than
replacing them. Read `AGENTS.md` first: its rules are the review's floor.

## Scope

When invoked directly: review the scope the user gives (`gh pr view <n>` for intent, then `gh pr
diff <n>`); with none, the uncommitted work (`git diff HEAD` plus untracked files, which no diff
shows); on a clean tree, `git diff master...HEAD`. Never review from a description of the change.

Read the stated **intent** (`git log --oneline` for the range, any linked issue or PR body): a
change that works but does something other than what it claims is a finding. Read every changed
file in full, not just the hunks, and for non-trivial changes the callers, implementations, tests
and docs of what changed, found with a reference search, not assumed from the diff: a signature
or behavior change is only safe if every call site agrees.

## Authority

For any `**/*.rs` path, any `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml` or
`rust-toolchain.toml`, use user-level Rust skills and read `AGENTS.md`; project rules are findings even
when clippy is green. Every other path (Dockerfile, compose files, `Makefile`, `.mk/*.mk`,
GitHub workflows, other YAML, bash, Markdown) gets the passes below plus the repository
invariants with the **same rigor** — an unmatched language is not a lighter review.

A meta-test under `meta/tests/` guards a claim nothing else checks; "Meta-tests must not pass on
nothing" under the invariants says how to attack one.

## Repository invariants (check on every review, whatever changed)

These are the ways this exporter breaks that generic reviewers miss.

### Commit type matches whether it ships

`svu` derives the release from the Conventional Commits since the last tag, and pull requests land
as merge commits, so every commit in the range counts. A change to the binary or the image that
users should receive typed `chore`, `docs`, `perf` or `refactor` never ships; tooling typed `fix`
or `feat` cuts a release with nothing in it; a stray `!` cuts a major; and a word ending in `fix`
or `feat` right before a colon anywhere in a subject counts as that type (`AGENTS.md`,
`docs/internal/release.md`).

### Read-only against Docker — enforced in code, not at runtime

The exporter must only ever *read* from the Docker API. A `:ro` bind of
`/var/run/docker.sock` does **not** make the API read-only: `:ro` only stops the container
from modifying the socket file, and a client can still POST creates, updates and removes
through it. So the read-only property rests entirely on the code:

- `DockerApi` (`src/docker`), on a `Client` over rust-common's docker-client (bollard) built
  without its `attach` feature, which exposes only list, inspect and events calls
  (nodes, services, a service inspect, tasks, containers, a container inspect, events), and
  through which all collector code reaches Docker;
- `meta/tests/docker_boundary.rs`, which keeps docker-client to reads by refusing any
  manifest that enables `attach`, lets only `src/docker` name the crate (and bollard, as
  `docker_client::bollard`) and build no request of its own there, allows only the known read
  calls (and the version negotiation) whose sections the shipped socket-proxy allowlist grants,
  and keeps bollard (the harness's SDK, which creates and removes things) declared directly only
  by `testenv` behind its `integration` feature, with none of bollard, testenv and
  testenv-core named in the exporter package. A diff that loosens any of those rules, or reaches
  Docker around the client, is a finding.

Critical:

- a new client method that creates, updates, removes, kills, scales, restarts, prunes or
  otherwise changes anything, or a bollard call other than the reads the boundary test lists;
- the exporter package depending on bollard, testenv or testenv-core in any dependency table;
- docs claiming that `:ro` makes the socket read-only. The README's Security section says the
  exporter only *issues* read requests and that enforcing it needs a proxy; a diff that walks
  that back is a finding.

Runtime enforcement needs an authorization proxy in front of the socket. The repo ships one
example, `deployments/docker/docker-compose.socket-proxy.yaml` with its allowlist
`socket-proxy.env`, pinned by `TestSocketProxy_ExampleAllowlist` against the real proxy image.
It makes the API read-only, not least-privilege: the proxy's ACL is per path prefix, so the
`SERVICES` and `TASKS` the exporter needs also allow service and task logs, and `CONTAINERS=1`
allows every `GET` under `/containers` (logs, archive, export). Docs that call it
least-privilege or exact per endpoint, an allowlist line relying on an image default, or a
change to `socket-proxy.env` without the test still passing are findings.

### Metrics are the public contract

Dashboards and alerts key on metric names and label sets. A renamed metric, or a label
added, removed or renamed, is a finding unless `docs/metrics.md` (and any alert
example in `docs/alerts.md` that uses it) changes in the same diff — and even then, call out the
break for existing users. A label fed by an unbounded value (container or task IDs, timestamps,
free-form strings) is a cardinality finding.

### Series lifecycle

- Removing a resource **deletes** its series (`GaugeVec::delete`, `delete_service_series`);
  it never sets them to 0. A stale zero series tells dashboards the resource still exists. A
  service whose label identity changes (stack, name, mode, a custom label) has its old series
  deleted before the new ones appear.
- A known state set (task states, update states, container states) is emitted exhaustively
  for every subject, zeros included, so absent series never mean "0".
- A family that mirrors a dynamic set (`replicas_state`, `running_replicas`, `at_desired`,
  `nodes_by_state`, `container_state`) is a snapshot collector: the whole set is built first
  and published in one swap, so a scrape never sees it empty or half-written. A build error
  keeps the previous set. Resetting a `GaugeVec` and setting it again is a finding: a scrape
  can land in between.

### One owner for Swarm state

The service and node caches and the gauges derived from them are written by exactly one task,
the reconciler (`src/collector/reconciler`). Everything else only asks it for work or for a
snapshot:

- The event listener only marks keys dirty (`ReconcilerHandle::enqueue_event`), never blocking
  and never calling Docker. A cache or gauge written from the listener, the poller or an HTTP
  handler is a finding.
- The dirty set, the generations and the resync counters share one lock (`Shared::state`),
  where 1.x had two with a lock order. It is never held across an `.await`, and nothing takes a
  second lock while holding it; a guard alive across an await point or a new nested lock is a
  finding.
- A generation (service, node, and the epoch for resyncs) is bumped when a change is
  *queued*, not when it is applied, so a queued, unapplied change already invalidates a poll.
  Moving a bump to apply time is a finding.
- Readiness is the first completed resync: the replicas poller does not start and `/healthz`
  stays `503` ("initial resync not completed", or "not a swarm manager" on a non-manager) until
  then. The containers poller
  (`--containers`) needs no resync and starts at once; making it wait is a finding (#140). A
  resync fetches both the service list and the node list before mutating anything, and a failed
  one mutates nothing.
- A resync is requested on overflow, on retry exhaustion, on a recovered panic, on every
  event-stream reconnect and every `PERIODIC_RESYNC_INTERVAL`. Dropping any of these leaves the
  caches behind Docker with nothing to catch them up.
- A daemon that is not a Swarm manager (Docker's 503 "This node is not a swarm manager",
  `docker::Error::is_not_manager`) is a state, `Role::NotManager`, not a failing resync: it is
  not counted in `resyncs_total{result="failure"}`, its transition removes every Swarm series
  (the poller-published replicas families and the poll carry-over included) and drops the queued
  work, and no manager-only call runs while it lasts but the backed-off resync probe: no service
  inspect (a batch stops at the inspect that found it), no node refresh, no periodic resync, no
  replicas poll, and no timer armed for any of them. A call that bypasses
  `State::swarm_work_enabled`, or a poll published while `NotManager`, is a finding.

### A panic costs a step, not the process

Each reconciler step runs under `catch_unwind` (`reconciler/recover.rs`), logs
`reconciler recovered from panic` with the payload and a backtrace, and requests a
rate-limited resync; `collector::must` panics on purpose for label values that do not fit their
family, as the Prometheus client's `With` does, relying on that recovery. Findings: switching the
release profile to `panic = "abort"`, a step that holds a lock or a half-applied change across
the point where it can panic, a panic path that skips the resync, and a `catch_unwind` around
code that is not a whole step.

### The clock is injected

`clippy.toml` bans `Timestamp::now`, `ZonedDateTime::now`, `SystemTime::now` and
`std::time::Instant::now`; the time comes only through `clock::Clock`, whose one wall-clock
read (`SystemClock`) lives in rust-common, so this repository has no exemption for them. An
`#[expect(clippy::disallowed_methods)]` for a clock read, a ban entry removed, or a
time read that bypasses the `Clock` (a timestamp gauge, a staleness check) is a finding: the
tests drive health and timestamps with tokio's paused clock and would no longer see the code.
`tokio::time::Instant` is allowed because it follows the paused clock.

`clippy.toml` also bans `std::process::Command::new` and `tokio::process::Command::new`: the
exporter spawns no process. Only a test, meta-test or harness function that runs a program
(the built exporter, `make`, `kill`) carries an `#[expect(clippy::disallowed_methods)]` for it,
with its reason; one on code under `src/` outside a test module is a finding.

### Poll consistency

The poller counts tasks against an immutable snapshot it gets from the reconciler and hands
the counts back; the reconciler publishes a service only if its generation, its dirty key,
the epoch and (for global services and global jobs) the node generation and the pending node
refresh all still match the snapshot. A rejected service keeps its last published series for
at most `MAX_CARRIED_POLLS` (2) polls, only with an unchanged label identity; then its series
are omitted and the poll fails, so health turns red. Findings:

- counting or deduplicating tasks from the live caches instead of the snapshot;
- a new rejection reason not checked, or carry-over that is unbounded or crosses a label
  identity change;
- `mark_poll_ok` on anything but a fully published poll;
- a poll or snapshot exchange that sends or waits without also selecting on its cancellation
  token, or a reply channel without room for the answer, so a shutdown can hang the worker join
  past `SHUTDOWN_BUDGET`.

### Bounded work

- Events become dirty keys in a bounded set (`PENDING_KEY_CAP`, 4096); past the cap the set is
  dropped with fresh allocations and a resync is requested instead. The reconciler inspects
  at most `SERVICE_KEYS_PER_CYCLE` (32) keys per cycle, a failed inspect is retried after
  `SERVICE_RETRY_DELAYS` and then dropped for a resync, and a failed resync or node refresh
  backs off up to `BACKOFF_MAX_DELAY`. A task per event, an unbounded channel, or a retry
  without a delay is a finding.
- Every request/response Docker call runs under `with_docker_timeout`
  (`DOCKER_REQUEST_TIMEOUT`, 15s) and the shutdown token. The event stream is the one
  exception: it has no deadline and is cancelled per connection on reconnect and on shutdown.
  Container enrichment shares one deadline per poll and at most `CONTAINERS_INSPECT_CAP` (300)
  inspects, failures included.
- No unbounded task spawning, and no map keyed by an external ID (service, node, container,
  task) without an eviction path when that resource disappears; that includes the
  reconciler's generation and retry maps.

### Health means published data

The health snapshot (`src/collector/health.rs`) is evaluated at scrape time, by `/healthz` and by
the `swarm_exporter_health` `GaugeFunc` alike. It is unhealthy until the first resync completes,
until a poll has been published, when the last published poll is older than `max(3 × poll delay,
30s)`, and when a requested resync has been outstanding for longer than that window. On a daemon
that is not a Swarm manager (`Role::NotManager`) only two rules apply: without `--containers` it is
unhealthy (`not a swarm manager`); with it, the container polls are checked for presence and
freshness in the same window. A health signal
set by the poller itself, a timestamp moved on a failed or partial publish, or a reason string
in `docs/operations.md` that no longer matches the code is a finding.

### Endpoint and image hardening

- The HTTP server keeps its two deadlines (`server::HEADER_READ_TIMEOUT` 5s and
  `server::WRITE_STALL_TIMEOUT` 15s in `src/server.rs`), enforced per connection by service-kit's
  server, and a drain bounded by the shared shutdown deadline. Removing one, or serving a
  connection outside `service_kit::http::serve`, is a finding.
- `/metrics` and `/healthz` take no input that becomes work: no query parameter, header or
  body may trigger a Docker call, widen a scrape, or allocate per request beyond the
  exposition itself.
- The image runs the static base as an explicit non-root `USER`, with every dhi.io base pinned by
  tag; the release job publishes only the digest it smoke-tested and scanned. If the diff touches
  the Dockerfile, the compose files, `.trivyignore.yaml`, a workflow, `scripts/image-test.sh` or
  `docs/internal/release.md`, read
  [references/release-and-image.md](references/release-and-image.md) and walk its items: they are
  part of this section.

### Meta-tests must not pass on nothing

A meta-test guards a claim nothing else checks: the toolchain pins and the vacuous flags
(`meta/tests/toolchain.rs`), the Dockerfile's frontend and bases (`dockerfiles.rs`), the Docker
boundary (`docker_boundary.rs`) and the workspace lints (`workspace_lints.rs`). Run the vacuity
pass on every one the diff touches, or whose input the diff changes: try to make the guarded thing
go wrong and see whether the test still notices. A test that passes because its walk found no
file, its regex matched nothing or its rule never ran is a false pass. Every rule needs a negative
control that fails on a planted violation, and a non-empty-input assertion. A change that
loosens one — a wider allowlist, a narrower walk, a dropped negative control, a pattern that no
longer matches the file it reads — is a finding unless it says why.

## Adversarial passes

Beyond the generic passes (logic, nil and empty input, aliasing, error handling, concurrency,
resources, security), run these with a "how would I make this fail" framing, and name one
concrete failing input per candidate rather than asserting "looks fine".

- **Contract drift:** a public signature, flag, environment variable, output format, exit code or
  error text changed without updating every consumer and the docs (*Docs drift* below).
- **Tests:** a test that passes against the *old* code, that asserts on a fake's recorded calls
  instead of the behavior they produced, or that was weakened or deleted to make the change pass
  is a finding. A bug fix with no regression test is a gap worth flagging.

## Always-on passes

The passes above are shaped by the diff. These run on **every** review, whatever changed.

### (a) Endpoint and image

Ask the one question the endpoint and image rules in the invariants are built on: does this change create
a new place where something from outside becomes work — a request parameter that becomes a
Docker call, a label value that becomes a series, an event that becomes a task — or does
it make the image or its deployment more privileged? If it does, walk the "Endpoint and
image hardening" (including its reference) and "Bounded work" items against it.

### (b) Deletion smell

A diff that removes a user-visible surface — a metric, a label, a flag, an environment
variable, a health behavior — and in the same breath rewrites that surface's test to assert it
is *absent* must cite the specification line that retired it. The specification here is
`docs/metrics.md`, `docs/configuration.md` (Flags, Environment) and the Health section of
`docs/operations.md`. A commit message is not a specification, and a page that still documents
the surface means the removal is unspecified.

A test flipped from "X happens" to "X does not happen" is not evidence that X should go — it is
the deletion wearing the test's clothes. Ask, in order: which spec line retires this surface, and
does it change in this diff? If none, this is a **critical** finding whatever the diff's stated
intent was. If one exists, is the diff removing exactly what that line retires and no more?

### (c) A new suppression has to show its work

Any new `#[expect(...)]`, `# shellcheck disable=` or `# droast ignore=` is a standing decision to
let a linter stay silent, and nothing but the reviewer judges the reason. So demand the attempt:
for `too_many_lines`, was the obvious extraction tried and what broke? For a cast lint, why is
the range safe? For `struct_excessive_bools`, why is an enum wrong here?
A suppression whose reason comment restates the rule instead of explaining why the fix does not
apply is a finding, and so is a suppression with no reason comment at all. The same holds for a
new `allow` in `[workspace.lints]` or `clippy.toml` — and a lint setting there that matches nothing
in this repo (copied from another project) is a finding too.

### (d) Cross-file duplication

Before accepting a new helper, search for the one that already exists — in `src/**`,
`tests/support`, `testenv/**` and rust-common's crates (a local copy of a helper rust-common has
is a finding), by *behavior*, not by the name the author chose. Check label building and
sanitization, metric constants, formatting, test fakes and wait helpers.
Two implementations of the same
rule drift apart, and the one the reviewer did not read is the one that keeps the bug.

### (e) Docs drift

The configuration and metrics contracts are written down more than once and can disagree
silently.

- A flag added or changed in `src/cli.rs` needs its entry in the Flags section of
  `docs/configuration.md` (clap's `--help` output), with the same name, default and meaning.
- The same holds for the Docker client environment variables in its Environment
  section and for the metric list in `docs/metrics.md`.

A surface the code has and the docs do not mention is a finding; so is a documented one nothing
implements, and so is a default in the docs that differs from the code.

## Gates

Do not re-run the repository's gates (`make verify`, `make check`, `make rust-deny`, `make
test-integration`, `cargo test -p meta`, `make docker-build`, `make rust-release-binaries`, `make
image-build-test`). The implementer is expected to run them and CI runs them again, but do not
assume either happened: report the verification evidence you actually have (under
adversarial-review-loop, `verification.txt` when supplied, which is the implementer's claim and not
proof; when invoked directly, the CI status or gate output in the PR or session) and what is
missing, such as a `src/collector`, `src/docker` or `testenv` change with no integration run
reported. A failure in that evidence caused by the change is a confirmed finding. Never say that a
check you did not see passed, and a skipped test is not a pass: the integration runner prints `SKIP`
for a test it could not run. Judge whether the change's tests prove it: a behaviour change needs a
test that fails on the old code. When invoked directly, one focused test (`cargo test --lib
collector::reconciler::tests::<name>`, or `cargo test --test engine_wire <name>`) may be run to
confirm a single suspected finding, never a full suite or gate. A candidate that is neither
reproduced nor traced end to end is dropped: a vague "consider" is not a finding.

Facts the evidence has to be read against:

- Which gates a changed path owes is `AGENTS.md`'s *Validation matrix*: a changed path with none
  of its rows in the evidence is missing evidence. A gate that was impractical (no Docker daemon
  for the DinD Swarm `make test-integration` needs, no network for cargo-deny) is unverified, not
  passed.
- A change to `.github/workflows/release.yaml` has no end-to-end check but a release dry run:
  say which steps could not be exercised.

## Report

Rank by severity, worst first. Nothing is more important than a genuine correctness or
read-only-invariant break: those are normally **critical**. Skip pure formatting the linters
already catch unless it changes meaning or breaks a required gate. For each finding:

```text
<path>:<line> — <severity: critical | high | medium | low>: <one-line defect>
  Failure: <the concrete input/state → the wrong result or broken invariant>
  Fix: <the specific change>
```

Findings first, then open questions or assumptions, then a one-line verdict: **block**,
**approve with nits**, or **approve**, and one line: `Verification evidence: seen … / missing …`.
If you found nothing, state what you tried to break so the "no findings" is
credible. Be blunt; do not soften a real defect to be polite, and do not invent findings to look
thorough.
