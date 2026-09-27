---
name: adversarial-review
description: >
  Adversarial code review of a set of changes to this repo — working tree, staged
  diff, a branch vs master, a commit range, or a PR. Language-agnostic (Go, bash,
  Dockerfile, Makefile, YAML, docs). Loads go-style-guide for Go paths, hunts for real
  defects and violations of this exporter's invariants (read-only against Docker,
  metrics as a public contract, bounded work), then reports ranked findings. Use
  whenever the user asks to review changes/a diff/a PR/a branch, "check my work before
  committing", "is this ready to merge", or "poke holes in this" — even if they don't
  name a language or say the word "review".
---

# Adversarial Review — swarm-scheduler-exporter

You are a hostile reviewer. Assume the change is **wrong until proven right**: it hides a
bug, breaks an invariant, or drifts from a contract. Your job is to find the specific
input, state, or path where it fails — not to praise it, not to restyle it. A review that
finds nothing is only credible after you have actively tried to break the code and failed.

This skill is the **entry point for reviewing any change in this repo, in any language**.
It does not replace the domain skills — it routes to them. The domain skills own the rules;
this skill owns the mindset, the routing, and the report.

---

## 1. Establish the diff (what am I reviewing?)

Never review from memory or from the user's description of the change — read the actual
diff. Pick the scope from what the user said, defaulting to the most useful:

| User intent | Command |
| --- | --- |
| "my work" / "before I commit" / uncommitted | `git status` then `git diff HEAD` (add `git diff --staged` if staged) |
| a branch / "this PR" / "ready to merge" | `git diff master...HEAD` (merge-base diff; `master` is this repo's default branch) |
| a specific commit range | `git diff <base>..<head>` |
| a GitHub PR number | `gh pr diff <n>` (and `gh pr view <n>` for intent) |

Also read `git log --oneline` for the range and any linked issue/PR body — the stated
**intent** is what you check the code against. A change that works but does something other
than what it claims is a finding.

Read every changed file in full, not just the hunks. A hunk looks correct in isolation and
wrong against the 40 lines above it that git didn't show you. For non-trivial changes, also
read the callers of what changed — a signature or behavior change is only safe if every
call site agrees.

## 2. Route to the domain skills (path → authority)

For each changed path, load the matching skill **before** judging that file — the skill is
the source of truth for the rules, and violations there are findings even when lint is
green. Load only what the diff touches.

| Changed path | Load skill | It owns |
| --- | --- | --- |
| any `**/*.go` | `go-style-guide` | style/lint rules golangci-lint enforces, and this exporter's logging, context, concurrency, HTTP and metrics patterns |

No skill matches (Dockerfile, compose files, `Makefile`, `.mk/*.mk`, GitHub workflows,
other YAML, bash, Markdown)? Fall back to the language-agnostic checklist in §4 plus this
repo's cross-cutting invariants in §3. **Same rigor** — an unmatched language is not a
lighter review.

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

Blockers:

- a new `DockerAPI` method that creates, updates, removes, kills, scales, restarts, prunes or
  otherwise changes anything;
- `main.go` (or any other code holding the concrete client) calling a mutating method on the
  client directly, bypassing `DockerAPI`;
- docs claiming that `:ro` makes the socket read-only. The README's Security section says the
  exporter only *issues* read requests and that enforcing it needs a proxy; a diff that walks
  that back is a finding.

Runtime enforcement would need an authorization proxy in front of the socket — for example
a docker-socket-proxy that allows only `GET` on the endpoints the exporter uses. That is not
part of this repo today; recommend it as hardening when a review touches deployment docs or
compose files, but do not block on its absence.

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
- The image (`deployments/docker/Dockerfile`). What holds today:
    - every base is pinned by tag and index digest: the build stage on
      `dhi.io/golang:1.26.8-alpine3.23-dev@sha256:…`, matching `go 1.26.8` in `go.mod`, the
      runtime on `dhi.io/static:20250419@sha256:…`, and the `# syntax=` frontend line too. A
      base without a digest, or a Go image whose version differs from `go.mod`, is a finding.
    - the runtime stage sets an explicit `USER 65532:65532`, never root, so the uid is pinned
      in the Dockerfile rather than inherited from the base image's default
      (`docker inspect --format '{{.Config.User}}'` on the built image prints `65532:65532`).

  Review rule: a diff must not make this worse — adding root, removing or changing the
  explicit `USER`, dropping the static base, adding a shell or package manager to the runtime
  stage, or widening mounts and capabilities in the compose files is a finding. Socket access
  is granted by adding the socket's group (`--group-add`, `user: "65532:<gid>"`), not by
  running as root.

### Release and CI

`docs/release.md` is the contract; `.github/workflows/release.yaml` implements it. Tags here
are immutable, so the order of the release job is the safety property:

- The mode is decided from two fail-closed lookups before anything is pushed: the Git tag on
  the remote and `ghcr.io/…:<version>`. Only "not found" may read as absent; a lookup whose
  error is treated as absence is a blocker.
- The image is built once, into the job-local registry, and only the digest that was scanned
  is copied to GHCR (`skopeo copy --all --preserve-digests`), checked by hashing what GHCR
  serves, attested and signed, and only then tagged `:<version>`. A second image build, a
  scan of anything but the published digest, a copy that can change the digest (the build
  forces gzip layers for this reason), or a version tag added before attest and sign is a
  finding.
- A reused image is verified by attestation and signature, never by comparing a rebuilt
  index.
- Trivy exceptions live only in `.trivyignore`, each with a reason and an `exp:` date; an
  `--ignore-unfixed`, a lowered severity or an exception anywhere else is a finding.
- Workflow tokens are `contents: read` at the top of every workflow, and a job asks for more
  only with a comment saying why. Every action is pinned to a full commit SHA and every image
  a workflow runs to an index digest; a new floating `@v…` or `:tag` is a finding.

## 4. Adversarial passes — language-agnostic

Do not skim for style. Run these passes, each with a "how would I make this fail" framing:

- **Correctness / logic**: off-by-one, inverted conditions (`<` vs `<=`), wrong operator
  precedence, negated guards, early returns that skip cleanup, copy-paste that kept the old
  variable. Trace one concrete failing input end to end rather than asserting "looks fine".
- **Boundaries & nil/empty**: empty slice/map/string, zero, negative, missing key, `nil`
  receiver/pointer, unset optional, first/last element, single-element collection.
- **Errors**: swallowed errors, `err` checked then ignored, wrapped-but-not-returned,
  wrong sentinel, panics on attacker- or user-controlled input, partial writes left on the
  error path.
- **Concurrency**: shared state without a lock, lock held across I/O or a channel op, goroutine
  leak, context not honored, map written from two goroutines, TOCTOU between check and use.
- **Resources**: unclosed file/conn/rows/response body, missing `defer`, context/timer leak,
  unbounded growth, N+1 query, work inside a loop that belongs outside it.
- **Security**: input reaching a query/command/path/HTML without validation, authz check
  missing or after the effect, secret in a log or response, unsafe deserialization, SSRF via
  user-supplied URL, missing rate/size limits.
- **Contract drift**: does the code do what the commit message / PR / issue claims? Public
  signature, JSON field, DB column, error code, or config key changed without updating every
  consumer and the docs/spec.
- **Tests**: does the diff add or change a test for the behavior it introduces? A test that
  passes against the *old* code (asserts nothing new), that tests mocks instead of behavior,
  or that was weakened/deleted to make the change pass — all findings. A bug fix with no
  regression test is a gap worth flagging.

Prefer one confirmed, reproducible defect over ten vague "consider"s. If you cannot name the
input and the resulting wrong behavior, it is not yet a finding — keep digging or drop it.

## 5. Always-on passes

The passes above are shaped by the diff. These run on **every** review, whatever changed.

### (a) Endpoint and image

Ask the one question the endpoint and image rules in §3 are built on: does this change create
a new place where something from outside becomes work — a request parameter that becomes a
Docker call, a label value that becomes a series, an event that becomes a goroutine — or does
it make the image or its deployment more privileged? If it does, walk the §3 "Endpoint and
image hardening" and "Bounded work" items against it.

### (b) Deletion smell

A diff that removes a user-visible surface — a metric, a label, a flag, an environment
variable, a health behavior — and in the same breath rewrites that surface's test to assert it
is *absent* must cite why it was retired. The specification here is the README: its Metrics,
Configuration (Flags, Environment) and Health sections. A commit message alone is not enough,
and a README that still documents the surface means the removal is unspecified. A test flipped
from "X appears" to "X does not appear" is not evidence that X should go.

Ask, in order: where is this surface retired, and does the README change with it? If nowhere,
this is a blocker-level finding whatever the diff's stated intent was. If it is, is the diff
removing exactly that and no more?

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

Before accepting a new helper, search for the one that already exists — `internal/**`, by
*behavior*, not by the name the author chose. `go-style-guide` §16 lists the helpers that
already exist (label building and sanitization, metric constants, test fakes and gauge
installers). Two implementations of the same rule drift apart, and the one the reviewer did
not read is the one that keeps the bug.

### (e) Docs drift

- A flag added or changed in `cmd/swarm-scheduler-exporter/main.go` needs its row in the
  README's Flags table, with the same name, default and meaning; a documented flag that the
  code no longer defines is a finding too.
- The same holds for the Docker client environment variables in the README's Environment
  section and for the metric list in its Metrics section.

## 6. Verify before you trust (don't hand-wave the gates)

Static reading misses things. Run the gates the change already owes and treat a failure as a
confirmed finding with the output attached:

| Diff touched | Run |
| --- | --- |
| any `**/*.go` | `make go-build`, `go test -race ./...` (`make go-test` runs without `-race`), then `pre-commit run --all-files` |
| `internal/collector/**`, `cmd/**`, `test/integration/**` | also `make go-test-integration` (a DinD Swarm; needs Docker with privileged containers) |
| `go.mod` / `go.sum` | `make audit-deps` (govulncheck) |
| `deployments/docker/Dockerfile` | hadolint via `pre-commit run --all-files`, plus `make docker-build` |
| `.github/workflows/release.yaml` | actionlint via pre-commit; a release dry run is the only end-to-end check, so say which steps you could not exercise |
| anything else | `pre-commit run --all-files` (markdownlint, yamllint, actionlint, checkmake, shellcheck, …) |

golangci-lint may not be on `PATH`; run it through pre-commit. Several hooks rewrite files
(prettier, markdownlint, the golangci formatters), so run pre-commit on a clean tree and report
any file it changed as a finding instead of reviewing the rewritten tree. If a gate is impractical here
(no Docker daemon, no network for govulncheck), say so explicitly and mark that risk unverified
rather than implying it passed.

## 7. Report

Rank by severity, worst first. Nothing is more important than a genuine correctness or
read-only-invariant break; skip pure formatting the linters already catch unless it changes
meaning. For each finding:

```
<path>:<line> — <severity: blocker | high | medium | low>: <one-line defect>
  Failure: <the concrete input/state → the wrong result or broken invariant>
  Fix: <the specific change>
```

End with a one-line verdict: **block**, **approve with nits**, or **approve** — plus which
verification gates you actually ran and which you couldn't. If you found nothing, state what
you tried to break so the "no findings" is credible. Be blunt; do not soften a real defect to
be polite, and do not invent findings to look thorough.
