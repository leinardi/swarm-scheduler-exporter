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
  inspect and events calls (`NodeList`, `ServiceList`, `ServiceInspectWithRaw`, `TaskList`,
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
  it never sets them to 0. A stale zero series tells dashboards the resource still exists.
- A known state set (task states, update states, container states) is emitted exhaustively
  for every subject, zeros included, so absent series never mean "0".
- A gauge that mirrors a dynamic set is `Reset()` before it is re-emitted.

### Bounded work

- Event handling goes through the fixed worker pool (`eventWorkerCount` workers,
  `eventQueueCapacity` queue). A goroutine per event is a finding.
- Every Docker call carries a context that is cancelled on shutdown and, where the call is
  not a stream, bounded in time by the poll cycle or an explicit timeout.
- No unbounded goroutines, and no map keyed by an external ID (service, node, container,
  task) without an eviction path when that resource disappears.

### Endpoint and image hardening

- The HTTP server keeps explicit `ReadHeaderTimeout`, `ReadTimeout`, `WriteTimeout` and
  `IdleTimeout`. Removing one, or switching to `http.ListenAndServe`, is a finding.
- `/metrics` and `/healthz` take no input that becomes work: no query parameter, header or
  body may trigger a Docker call, widen a scrape, or allocate per request beyond the
  exposition itself.
- The image (`deployments/docker/Dockerfile`). These are **today's facts plus a direction**,
  not invariants that already hold:
    - the build stages use a mutable tag, `dhi.io/golang:1-alpine3.23-dev` (lines 6 and 18);
    - the runtime base is date-tagged, `dhi.io/static:20250419`, not digest-pinned;
    - non-root comes only from the upstream image default: there is no explicit `USER`
      (lines 47–58; the comment on line 58 asserts it).

  Review rule: a diff must not make this worse — adding root, dropping the static base,
  adding a shell or package manager to the runtime stage, or widening mounts and
  capabilities in the compose files is a finding. A diff that touches the Dockerfile and
  leaves these gaps unaddressed gets a **low** finding, not a blocker. The fix (digest pins,
  plus an explicit `USER 65532:65532` or whatever uid the dhi static image documents,
  verified with `docker inspect --format '{{.Config.User}}'` on the built image) is a
  separate follow-up.

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
| `go.mod` / `go.sum` | `make audit-deps` (govulncheck); before that target exists, `go run golang.org/x/vuln/cmd/govulncheck@v1.8.0 ./...` |
| `deployments/docker/Dockerfile` | hadolint via `pre-commit run --all-files`, plus `make docker-build` |
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
