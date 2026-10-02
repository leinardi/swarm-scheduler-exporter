---
name: adversarial-review
description: >
  Adversarial code review of changes to swarm-scheduler-exporter — working tree,
  staged diff, a branch vs master, a commit range, or a PR — in any language (Go,
  bash, Dockerfile, Makefile, YAML, docs). Loads go-style-guide for Go paths, checks
  the exporter's invariants (read-only Docker access, metrics as a public contract,
  series lifecycle, single reconciler owner, bounded work, release pipeline) and
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

Copy this checklist and tick items as you go:

```text
Review progress:
- [ ] 1. Scope chosen; diff, stated intent and every changed file read in full
- [ ] 2. `go-style-guide` loaded for the Go paths
- [ ] 3. Repo invariants checked
- [ ] 4. Adversarial passes run; every candidate confirmed or dropped
- [ ] 5. Always-on passes (a)–(e) run
- [ ] 6. Gates run; `git status` checked for hook rewrites; skipped gates marked unverified
- [ ] 7. Report written: findings, open questions, verdict, gates run and not run
```

---

## 1. Establish the diff (what am I reviewing?)

Never review from memory or from the user's description of the change — read the actual
diff. Pick the scope from what the user said:

| User intent | Command |
| --- | --- |
| "my work" / "before I commit" / uncommitted | `git status --short`, then `git diff HEAD`; read untracked files too, which no diff shows |
| staged changes only | `git diff --staged` |
| a branch / "this PR" / "ready to merge" | `git diff master...HEAD` (merge-base diff; `master` is this repo's default branch) |
| a specific commit range | `git diff <base>..<head>` |
| a GitHub PR number | `gh pr view <n>` for intent, then `gh pr diff <n>` |

If the user names no scope, review the uncommitted work (first row); if the tree is
clean, review the branch against `master` (third row).

Also read `git log --oneline` for the range and any linked issue/PR body — the stated
**intent** is what you check the code against. A change that works but does something other
than what it claims is a finding.

Read every changed file in full, not just the hunks. A hunk looks correct in isolation and
wrong against the 40 lines above it that git didn't show you. For non-trivial changes, also
read the callers, implementations, tests and docs of what changed — found with a reference
search, not assumed from the diff: a signature or behavior change is only safe if every call
site agrees.

## 2. Load the domain skill

For any `**/*.go` path, load `go-style-guide` before judging it; its rules are findings even
when lint is green. Every other path (Dockerfile, compose files, `Makefile`, `.mk/*.mk`,
GitHub workflows, other YAML, bash, Markdown) gets the passes in §4 plus the invariants in
§3 with the **same rigor** — an unmatched language is not a lighter review.

## 3. Repo invariants — check these on every review, whatever changed

These are the ways this exporter breaks that generic reviewers miss.

### Read-only against Docker — enforced in code, not at runtime

The exporter must only ever *read* from the Docker API. A `:ro` bind of
`/var/run/docker.sock` does **not** make the API read-only: `:ro` only stops the container
from modifying the socket file, and a client can still POST creates, updates and removes
through it. So the read-only property rests entirely on the code:

- the `DockerAPI` interface (`internal/collector/docker_api.go`), which exposes only list,
  inspect and events calls (`NodeList`, `ServiceList`, `ServiceInspect`, `TaskList`,
  `ContainerList`, `ContainerInspect`, `Events`), and through which all collector code reaches
  Docker;
- the depguard rule `docker-sdk-boundary` in `.golangci.yaml`, which denies Docker SDK imports
  (`github.com/docker/docker/…`, `github.com/moby/moby/…`) outside `internal/collector` and
  `cmd/swarm-scheduler-exporter`. An SDK import anywhere else is a finding, and so is a diff
  that widens the rule's allowed trees, removes a deny entry or drops a trailing slash.

Critical:

- a new `DockerAPI` method that creates, updates, removes, kills, scales, restarts, prunes or
  otherwise changes anything;
- `main.go` (or any other code holding the concrete client) calling a mutating method on the
  client directly, bypassing `DockerAPI`;
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
added, removed or renamed, is a finding unless the README's Metrics section (and any alert
example that uses it) changes in the same diff — and even then, call out the break for
existing users. A label fed by an unbounded value (container or task IDs, timestamps,
free-form strings) is a cardinality finding.

### Series lifecycle

- Removing a resource **deletes** its series (`GaugeVec.Delete`, `ClearServiceUpdateMetrics`);
  it never sets them to 0. A stale zero series tells dashboards the resource still exists. A
  service whose label identity changes (stack, name, mode, a custom label) has its old series
  deleted before the new ones appear.
- A known state set (task states, update states, container states) is emitted exhaustively
  for every subject, zeros included, so absent series never mean "0".
- A family that mirrors a dynamic set (`replicas_state`, `running_replicas`, `at_desired`,
  `nodes_by_state`, `container_state`) is a snapshot collector: the whole set is built first
  and published in one swap, so a scrape never sees it empty or half-written. A build error
  keeps the previous set. `Reset()` followed by re-emission is a finding: a scrape can land in
  between.

### One owner for Swarm state

The service and node caches and the gauges derived from them are written by exactly one
goroutine, the `Reconciler` (`internal/collector/reconciler.go`). Everything else only asks
it for work or for a snapshot:

- The event dispatcher only marks keys dirty (`enqueueEvent`), never blocking and never
  calling Docker. A cache or gauge written from the dispatcher, the poller or an HTTP
  handler is a finding.
- Lock order is `Reconciler.mu` then `metadataMu`, never the reverse.
- A generation (service, node, and the epoch for resyncs) is bumped when a change is
  *queued*, not when it is applied, so a queued, unapplied change already invalidates a poll.
  Moving a bump to apply time is a finding.
- Readiness is the first completed resync: the poller does not start and `/healthz` stays
  `503` ("initial resync not completed") until then. A resync fetches both `ServiceList` and
  `NodeList` before mutating anything, and a failed one mutates nothing.
- A resync is requested on overflow, on retry exhaustion, on a recovered panic, on every
  event-stream reconnect and every `periodicResyncInterval`. Dropping any of these leaves the
  caches behind Docker with nothing to catch them up.

### Poll consistency

The poller counts tasks against an immutable snapshot it gets from the reconciler and hands
the counts back; the reconciler publishes a service only if its generation, its dirty key,
the epoch and (for global services and global jobs) the node generation and the pending node
refresh all still match the snapshot. A rejected service keeps its last published series for
at most `maxCarriedPolls` (2) polls, only with an unchanged label identity; then its series
are omitted and the poll fails, so health turns red. Findings:

- counting or deduplicating tasks from the live caches instead of the snapshot;
- a new rejection reason not checked, or carry-over that is unbounded or crosses a label
  identity change;
- `MarkPollOK` on anything but a fully published poll;
- a poll or snapshot exchange that sends or waits without also selecting on `ctx.Done()`, or
  a reply channel without room for the answer, so a shutdown can hang `workerGroup.Wait()`.

### Bounded work

- Events become dirty keys in a bounded set (`pendingKeyCap`, 4096); past the cap the set is
  dropped with fresh allocations and a resync is requested instead. The reconciler inspects
  at most `serviceKeysPerCycle` (32) keys per cycle, a failed inspect is retried after
  `serviceRetryDelays` and then dropped for a resync, and a failed resync or node refresh backs
  off up to `backoffMaxDelay`. A goroutine per event, an unbounded queue, or a retry without a
  delay is a finding.
- Every request/response Docker call runs under `withDockerTimeout` (`dockerRequestTimeout`,
  15s) derived from a context cancelled on shutdown. The event stream is the one exception:
  it gets its own cancellable context per connection with no deadline, and its connection
  setup is bounded by the transport (`newDockerClientWithTimeouts`). Container enrichment
  shares one deadline per poll and at most `containersInspectCap` (300) inspects, failures
  included.
- No unbounded goroutines, and no map keyed by an external ID (service, node, container,
  task) without an eviction path when that resource disappears; that includes the
  reconciler's generation and retry maps.

### Health means published data

`HealthSnapshot` is evaluated at scrape time, by `/healthz` and by the
`swarm_exporter_health` `GaugeFunc` alike. It is unhealthy until the first resync completes,
until a poll has been published, when the last published poll is older than
`max(3 × poll delay, 30s)`, and when a requested resync has been outstanding for longer than
that window. A health signal set by the poller itself, a timestamp moved on a failed or
partial publish, or a README reason string that no longer matches the code is a finding.

### Endpoint and image hardening

- The HTTP server keeps explicit `ReadHeaderTimeout`, `ReadTimeout`, `WriteTimeout` and
  `IdleTimeout`. Removing one, or switching to `http.ListenAndServe`, is a finding.
- `/metrics` and `/healthz` take no input that becomes work: no query parameter, header or
  body may trigger a Docker call, widen a scrape, or allocate per request beyond the
  exposition itself.
- The image runs the static runtime base as an explicit non-root `USER`, with every base
  pinned by digest; the release job publishes only the digest it scanned. If the diff
  touches the Dockerfile, the compose files, `.trivyignore`, a workflow or `docs/release.md`,
  read [references/release-and-image.md](references/release-and-image.md) and walk its
  items: they are part of this section.

## 4. Adversarial passes — language-agnostic

Do not skim for style. Run these passes, each with a "how would I make this fail" framing:

- **Generic passes**: correctness and logic, boundaries and nil/empty, aliasing, error
  handling, concurrency, resources and security. For each, name one concrete failing input and
  trace it end to end rather than asserting "looks fine".
- **Contract drift**: does the code do what the commit message / PR / issue claims? A public
  signature, flag, environment variable, output format, exit code or error text changed without
  updating every consumer and the docs (§5 (e)).
- **Tests**: does the diff add or change a test for the behavior it introduces? A test that
  passes against the *old* code (asserts nothing new), that asserts on a fake's recorded calls
  instead of the behavior they produced, or that was weakened/deleted to make the change pass —
  all findings. A bug fix with no regression test is a gap worth flagging.

For each candidate defect:

1. Reproduce it with a focused test, or trace one concrete input through the code to the wrong
   result.
2. Confirmed: it is a finding. Record the input and the wrong behavior.
3. Not confirmed: dig once more (callers, tests, config path). Still not confirmed: drop it.
   A vague "consider" is not a finding.

## 5. Always-on passes

The passes above are shaped by the diff. These run on **every** review, whatever changed.

### (a) Endpoint and image

Ask the one question the endpoint and image rules in §3 are built on: does this change create
a new place where something from outside becomes work — a request parameter that becomes a
Docker call, a label value that becomes a series, an event that becomes a goroutine — or does
it make the image or its deployment more privileged? If it does, walk the §3 "Endpoint and
image hardening" (including its reference) and "Bounded work" items against it.

### (b) Deletion smell

A diff that removes a user-visible surface — a metric, a label, a flag, an environment
variable, a health behavior — and in the same breath rewrites that surface's test to assert it
is *absent* must cite the specification line that retired it. The specification here is the
README: its Metrics, Configuration (Flags, Environment) and Health sections. A commit message is
not a specification, and a README that still documents the surface means the removal is
unspecified.

A test flipped from "X happens" to "X does not happen" is not evidence that X should go — it is
the deletion wearing the test's clothes. Ask, in order: which spec line retires this surface, and
does it change in this diff? If none, this is a **critical** finding whatever the diff's stated
intent was. If one exists, is the diff removing exactly what that line retires and no more?

### (c) A new suppression has to show its work

Any new `//nolint:`, `# shellcheck disable=` or `# hadolint ignore=` is a standing decision to
let a linter stay silent, and nothing in this repo ratchets their number. So demand the attempt:
for a complexity or length rule, was the obvious extraction tried and what broke? For
`wrapcheck`, why is wrapping wrong here? For `varnamelen`, why does the name have to be short?
A suppression whose reason comment restates the rule instead of explaining why the fix does not
apply is a finding, and so is a suppression with no reason comment at all. The same holds for a
new exclusion in `.golangci.yaml` — and an exclusion, enable or setting there that matches nothing
in this repo (copied from another project) is a finding too.

### (d) Cross-file duplication

Before accepting a new helper, search for the one that already exists — in `internal/**` and
`cmd/**`, by *behavior*, not by the name the author chose. The *Reuse before writing* table in
`go-style-guide` lists the helpers that already exist (label building and sanitization, metric
constants, test fakes, gauge installers and wait helpers). Two implementations of the same rule
drift apart, and the one the reviewer did not read is the one that keeps the bug.

### (e) Docs drift

The configuration and metrics contracts are written down more than once and can disagree
silently.

- A flag added or changed in `cmd/swarm-scheduler-exporter/main.go` needs its row in the
  README's Flags table, with the same name, default and meaning.
- The same holds for the Docker client environment variables in the README's Environment
  section and for the metric list in its Metrics section.

A surface the code has and the docs do not mention is a finding; so is a documented one nothing
implements, and so is a default in the docs that differs from the code.

## 6. Verify before you trust (don't hand-wave the gates)

Static reading misses things. Use focused tests while investigating (`go test
./internal/collector -run <Name>`), then run the gates the change owes and treat a failure it
caused as a confirmed finding with the output attached:

| Diff touched | Run |
| --- | --- |
| any `**/*.go` | `make go-build`, `make go-test` (race detector on), then `make check` |
| `internal/collector/**`, `internal/testenv/**`, `cmd/**`, `test/integration/**` | also `make go-vet-integration` and `make go-test-integration` (a DinD Swarm; needs Docker with privileged containers) |
| `go.mod` / `go.sum` | `make go-tidy` and `make audit-deps` (govulncheck; network required) |
| `deployments/docker/Dockerfile` | hadolint via `make check`, plus `make docker-build` |
| `.github/workflows/release.yaml` | actionlint via `make check`; a release dry run is the only end-to-end check, so say which steps you could not exercise |
| anything else | `make check` (pre-commit on all files: markdownlint, yamllint, actionlint, checkmake, shellcheck, …) |

golangci-lint may not be on `PATH`; run it through pre-commit. Several hooks rewrite files
(prettier, markdownlint, the golangci formatters): check `git status` afterwards and report a
rewrite as a finding instead of reviewing the rewritten tree. A skipped test is not a pass —
check the `-v` output for `SKIP`. If a gate is impractical here (no Docker daemon, no network
for govulncheck), say so explicitly and mark that risk unverified rather than implying it
passed.

## 7. Report

Rank by severity, worst first. Nothing is more important than a genuine correctness or
read-only-invariant break: those are normally **critical**. Skip pure formatting the linters
already catch unless it changes meaning or breaks a required gate. For each finding:

```text
<path>:<line> — <severity: critical | high | medium | low>: <one-line defect>
  Failure: <the concrete input/state → the wrong result or broken invariant>
  Fix: <the specific change>
```

Findings first, then open questions or assumptions, then a one-line verdict: **block**,
**approve with nits**, or **approve** — plus which verification gates you actually ran and which
you couldn't. If you found nothing, state what you tried to break so the "no findings" is
credible. Be blunt; do not soften a real defect to be polite, and do not invent findings to look
thorough.
