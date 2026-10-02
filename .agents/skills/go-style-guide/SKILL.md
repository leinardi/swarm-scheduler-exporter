---
name: go-style-guide
description: >
  Go coding rules for swarm-scheduler-exporter: the golangci-lint v2 (default: all)
  settings and how to satisfy them (noinlineerr, err113, nolintlint, mnd,
  varnamelen, modernize), the helpers to reuse, and the exporter's own logging,
  context, concurrency, HTTP server and Prometheus metric patterns. Use when
  writing, editing or reviewing any .go file in swarm-scheduler-exporter — consult
  it before generating Go code, not after lint fails.
---

# Go Style Guide — swarm-scheduler-exporter

Rules derived from `.golangci.yaml` (golangci-lint v2, `default: all`) and verified
against the existing codebase in `cmd/` and `internal/`. §1–§11 are what the linters
enforce, §12–§14 are review rules, and §15–§18 are this exporter's own patterns. Detail
that only some changes need lives in `references/`; each section says when to read it.

golangci-lint is not on `PATH` in every environment; run it through pre-commit (see
*Lint and test loop* at the end). Never commit with `--no-verify` — fix the underlying
issue instead.

Linters that are **disabled** in `.golangci.yaml`, so their rules do not apply:

| Disabled linter | Reason |
| --- | --- |
| `exhaustruct`, `exhaustruct_v5` | Requires every struct field to be set; too noisy for short-lived structs |
| `gomodguard` | Replaced by `gomodguard_v2`, which is enabled |
| `gochecknoglobals` | Package-level gauge variables are intentional |
| `nonamedreturns` | Named returns are allowed |
| `wsl` | The deprecated v4 linter; its successor `wsl_v5` stays enabled |

The formatters (`gci`, `gofmt`, `gofumpt`, `goimports`, `golines`) run in the
`golangci-lint-fmt` hook and in CI. The `integration` build tag is set in `.golangci.yaml`,
so the integration harness and suite are linted too.

---

## 1. Import grouping

Three groups separated by blank lines — stdlib, third-party, then local
`github.com/leinardi/swarm-scheduler-exporter/...` — alphabetical within each group. `gci`
and `goimports` both enforce it and the `golangci-lint-fmt` hook rewrites it.

`internal/labels` is imported as `labelutil` wherever it would otherwise be confused with
Prometheus's own `prometheus.Labels` type.

---

## 2. Error handling

### 2a. No inline error assignment in `if` (`noinlineerr`)

```go
// Wrong
if err := doSomething(); err != nil {

// Right
err := doSomething()
if err != nil {
    return err
}

err = secondThing() // = not := for the second and later assignments
```

### 2b. Wrap errors with `%w` (`errorlint`)

```go
listResult, listErr := cli.NodeList(ctx, client.NodeListOptions{Filters: nil})
if listErr != nil {
    return fmt.Errorf("node list: %w", listErr)
}
```

The prefix is a short, lowercase phrase naming the operation, not a full sentence: no
capital letters, no trailing period. Compare with `errors.Is`/`errors.As` (or the generic
`errors.AsType[T]`), never `==` on error values. Docker API errors are classified with
`github.com/containerd/errdefs` (`errdefs.IsNotFound`), which works because the Docker
client maps HTTP status codes to those errors. Prefer flat code with early returns; no
`else` after a `return` (`revive`'s `indent-error-flow`).

### 2c. Errors are sentinels, detail is wrapped (`err113`)

`err113` flags every `errors.New` inside a function body and every `fmt.Errorf` without a
`%w` verb — a static message included. Declare the error once as a package-level
sentinel (`var ErrEmptyFlagValue = errors.New("empty flag value")`) and wrap the detail:

```go
return fmt.Errorf(
    "%w %s (DOCKER_API_VERSION): supported range is %s to %s",
    ErrUnsupportedAPIVersion,
    apiVersion,
    client.MinAPIVersion,
    client.MaxAPIVersion,
)
```

Export a sentinel (`Err…`) when callers need `errors.Is`. When an error must carry
structured data, implement `error` on a private struct and have the constructor return
`error`, not the concrete type (`labelError`/`newLabelError` in `internal/labels`).
Suppress with `//nolint:err113` only when no sentinel can fit, and say why (§3).

### 2d. Aggregating multiple errors

Use `errors.Join` over a slice of wrapped errors.

### 2e. Ignoring errors explicitly

When an error return cannot be acted upon (writing to a `ResponseWriter`, `fmt.Fprintln`
on stdout, `GaugeVec.Delete`'s bool), assign it to the blank identifier:

```go
_, _ = io.WriteString(responseWriter, okBody)
```

---

## 3. `nolint` directives

`nolintlint` enforces three things:

- **Specific**: name every linter — no bare `//nolint`
- **Explanation required**: every directive needs `// reason`
- **No unused**: remove directives when the code no longer triggers that linter

The explanation says why the fix does not apply here, not which rule fired (§13). A
statement gets an inline directive; a function or type gets it on the preceding line:

```go
switch evt.Type { //nolint:exhaustive // the stream is filtered to service and node events

//nolint:gocritic // slog.Handler requires slog.Record by value; cannot change the signature.
func (handler *PlainTextHandler) Handle(_ context.Context, record slog.Record) error {
```

Multiple linters are comma-separated with no spaces. Name all that fire: `gocyclo` and
`cyclop` measure the same thing, and `gocognit` often joins them.

---

## 4. Complexity limits

| Linter | Threshold | Note |
| --- | --- | --- |
| `gocyclo` | 15 | Cyclomatic complexity |
| `cyclop` | 15 | Same metric, different linter — both fire together |
| `gocognit` | 35 | Cognitive complexity |
| `funlen` | 50 statements | Lines are disabled (`lines: -1`) |

Prefer extracting helpers over suppressing; when suppression is the right call, the
nolint comment says why. Test files are exempt from `funlen`, `gocognit`, `gocyclo`,
`maintidx` and the `cyclop` "calculated cyclomatic complexity" check.

---

## 5. Magic numbers (`mnd`)

Numbers 0, 1, 2, 3 are allowed everywhere. Any other literal in an `argument`, `case`,
`condition`, or `return` position needs a named constant. Any literal used more than once,
or that needs explaining, is a constant too:

```go
const (
    DefaultPollDelay    = 10 * time.Second
    httpShutdownTimeout = 10 * time.Second
    pendingKeyCap       = 4096
    backoffMaxDelay     = 30 * time.Second
)
```

`strings.SplitN` is excluded from mnd checks. Test files are fully exempt from `mnd`.

---

## 6. Struct size (`gocritic hugeParam`)

Structs over ~80 bytes passed by value trigger `hugeParam`. Pass by pointer — or suppress
when an interface fixes the signature (`slog.Handler`, §3). The same applies to
`rangeValCopy`: iterate large slices by index (`service := &services[index]`; see *Index
loops over large structs* in [references/patterns.md](references/patterns.md)).

---

## 7. Forbidden packages (`depguard`)

| Forbidden | Use instead |
| --- | --- |
| `github.com/sirupsen/logrus` (rule `logger`; allowed only in `internal/logger`) | `github.com/leinardi/swarm-scheduler-exporter/internal/logger` (`logger.L()`, backed by `log/slog`) |
| `github.com/pkg/errors` (rule `forbidden-forks`) | stdlib `errors` + `fmt.Errorf(...%w...)` |
| `github.com/instana/testify` (rule `forbidden-forks`) | `github.com/stretchr/testify` |
| `github.com/docker/docker/…` and `github.com/moby/moby/…` outside `internal/collector`, `cmd/swarm-scheduler-exporter` and the integration-tagged `internal/testenv` and `test/integration` (rule `docker-sdk-boundary`) | go through the `DockerAPI` interface in `internal/collector` — this boundary is half of what keeps the exporter read-only against Docker; a new method on `DockerAPI` must only read |

---

## 8. Comments and `godox`

- `FIXME` is flagged by `godox`. `TODO` is allowed.
- gocritic's `whyNoLint` check is disabled, but every `//nolint` still needs an
  explanation (`require-explanation`).
- Every exported function, type, and variable has a doc comment beginning with the symbol
  name (`// HealthFunc returns whether the exporter is healthy…`). Unexported symbols get
  one when their purpose is not obvious from the name.
- Inline comments explain *why*, not *what* (§13).
- Large files are divided with `// --- Section name ---` separators.
- Do not add doc comments or comment scaffolding to code you did not otherwise change.

---

## 9. Variable naming (`varnamelen`)

`varnamelen` flags a name shorter than 3 characters whose last use is more than 5 lines
from its declaration (defaults: `min-name-length: 3`, `max-distance: 5`). Test files are
exempt.

- **Receivers are exempt**: `(f *fakeDocker)`, `(e *labelError)` are fine.
- **Parameters are checked like locals.** A one-letter parameter passes in a three-line
  function and is flagged as soon as the body grows, so name them from the start:

  ```go
  // Wrong
  func newLabelError(r, o, s string) error

  // Right
  func newLabelError(reason, original, sanitized string) error
  ```

The codebase leans long: `parentContext`, `dockerClient`, `responseWriter`.

---

## 10. `modernize` — no pointer-boxing helpers

The `modernize` linter (`newexpr` check) flags any function whose sole purpose is to return
a pointer to its argument — the generic `func ptr[T any](v T) *T` included — at the
declaration and at every call site. The Go version in `go.mod` lets `new` take an
expression: write `new(uint64(3))`. Taking the address of a local is fine too.

---

## 11. Other thresholds

| Rule | Setting | What to do |
| --- | --- | --- |
| `any` (`gofmt` rewrite rule) | `interface{}` → `any` | Write `any` in new code so the formatter does not change your diff |
| `lll` | 140 characters | `golines` wraps automatically; test files are exempt |
| `dupl` | 100 tokens | Extract shared logic into a helper; test files are exempt |
| `govet` shadow | enabled | Name errors after their source (`listErr`, `pollErr`, `shutdownErr`) instead of re-declaring `err` |
| `goconst` | 3+ occurrences, length ≥ 2 | Shared label names are constants in `internal/collector/metrics_ids.go`; a label used by one family only is a constant next to that family (`labelNode…` in `nodes.go`). Test files are exempt |

---

## 12. Reuse before writing

Before adding a helper, a fake, a label constant or a Docker call, check whether one of these
already answers the question — and if it nearly does, extend it rather than forking it.

| Need | Use | Not |
| --- | --- | --- |
| A logger | `logger.L()` (`internal/logger`); `logger.Set` swaps it in tests | a package-local `slog.New`, `log.Printf`, or a logger passed down as a parameter |
| Turning user-supplied label names into Prometheus label names | `internal/labels`: `ValidateAndSanitizeLabelNames` for the `-label` flag, `ValidateCustomLabelCount` for the cap, `SanitizeLabelNames` / `SanitizeMetricLabels` when building a vec or a label set | a hand-rolled regexp or `strings.Map` |
| Warning about a label value that looks unbounded | `labelutil.MaybeWarnHighCardinality` (warns once per label key) | a new warning path, or none |
| A metric namespace, subsystem or label name | the constants in `internal/collector/metrics_ids.go` (`prometheusNamespace`, `prometheus…Subsystem`, shared `label…` names); family-only labels next to their family (`labelNode…` in `nodes.go`) | a string literal in `GaugeOpts` or a label map, or a second constant for a name that already has one |
| The label names of a per-service vec | the base label constants followed by `getSanitizedCustomLabelNames()...` | a second list of custom label names |
| The label values of one service's series | `labelsForMetadata` (base + custom labels, sanitized, with the high-cardinality warning) | rebuilding the map at the call site — `labelsForService` and the builder in `replicas_state.go` are existing near-copies, not patterns |
| Any call into Docker from `internal/collector` | the `DockerAPI` interface (`internal/collector/docker_api.go`); `*client.Client` satisfies it with no adapter | taking `*client.Client` in collector code, or widening `DockerAPI` with a method that mutates anything |
| A Docker fake in a collector unit test | `fakeDocker` (`fake_docker_test.go`) — canned lists, per-call errors, call counters, `eventsCh`/`errCh` for `Events` | a second fake, or a real daemon |
| Isolating metrics in a collector test | the `install…` helpers in `gauge_helpers_test.go` (`installDesiredReplicasGauges`, `installNodesByStateGauge`, …) — unregistered vecs or snapshot collectors swapped in and restored on cleanup | registering on `prometheus.DefaultRegisterer` from a test |
| Reading a snapshot collector in a test | `gatherSeries`, `snapshotValue`, `seriesID`, `familySeries` (`gauge_helpers_test.go`) — gather through a throwaway pedantic registry | `testutil.ToFloat64` on a `With(...)`, which snapshot collectors do not have |
| Resetting the package caches between tests | `resetCollectorState(t)` (`types_test.go`) | clearing `metadataCache` / `cachedNodes` by hand |
| Test fixtures for service metadata and labels | `makeTestMetadata`, `serviceLabels`, `baseServiceLabels` (`gauge_helpers_test.go`) | a per-file copy |
| Waiting for a condition in a test | `eventually(t, what, cond)` (`internal/collector/reconciler_test.go`); `eventually(t, timeout, check)` in the integration suite (`test/integration/helpers_wait_test.go`) | `time.Sleep`, or another hand-rolled deadline loop (see §14) |

---

## 13. Comments carry rationale; history goes in the commit

A comment says **why the code is the way it is** — the constraint, the failure it avoids,
the alternative that was rejected. It does not narrate what changed, when, or at whose
request; that belongs in the commit body.

```go
// Bad — history in the code.
// Changed after the socket-proxy bug report; used to filter the task list by service ID.

// Good — rationale in the code.
// No service filter on purpose: the IDs would travel URL-encoded in the query, and a socket
// proxy in front of the daemon rejects that URL long before the cluster is large.
```

The same rule makes `//nolint` explanations useful: say why the fix does not apply here.

---

## 14. Waiting in tests: classify before you write a sleep

Tests use no `time.Sleep` except for the sleep classes in the reference. A positive
eventual (something another goroutine will do) never sleeps: it waits on a channel or on
`eventually` (§12).

Writing a wait or a sleep in a test? Read [references/test-waits.md](references/test-waits.md)
first: it defines the five classes and the shape each one takes.

---

## 15. Logging

- `log/slog` only, through the global `logger.L()`. It is a deliberate singleton, configured once
  in `main` by `logger.Configure` (which installs it with `logger.Set`); do not pass loggers as
  parameters.
- Structured key/value fields, never `fmt.Sprintf` inside a log call:

  ```go
  logger.L().Error("docker client init failed", "err", newClientErr)
  ```

  Established field names: `"err"`, `"version"`/`"commit"`/`"date"`, `"every"` (a duration),
  `"service_id"`, `"label"`/`"sample_value"`. Reuse them.
- Levels: `Debug` for operational detail, `Info` for lifecycle (startup, shutdown, stream
  connected), `Warn` for degraded-but-recoverable (a failed container poll, a reconnect), `Error`
  for failures that affect correctness or availability. There is no fatal level: return a non-zero
  exit code from `run()` instead.
- A goroutine calls `logger.L()` at the start of its body, not at capture time.

---

## 16. Context

- Every new function that does I/O or calls Docker takes `context.Context` as its first
  parameter, named `ctx`, or `parentContext` when it is the root being threaded through.
- The root context is created once in `run()` with `signal.NotifyContext(context.Background(),
  syscall.SIGINT, syscall.SIGTERM)`; `defer` its cancel.
- Child contexts with a timeout (`context.WithTimeout`) always `defer` their cancel.
- Cancellation is filtered where the error is handled, not silenced everywhere: check
  `errors.Is(err, context.Canceled)` before logging as an error. A function that can end because
  its context is done returns the wrapped context error — it does not count or log the shutdown
  as a failure (`ListenSwarmEvents` returns before counting a reconnect).

---

## 17. Project conventions: layout, license header, flags, dependencies

- All application code lives under `internal/`; `cmd/` wires and holds no business logic. A
  new metric family gets its own file in `internal/collector/`.
- Every `.go` file starts with the MIT license block, after the `//go:build` line where there
  is one, then the `package` line.
- stdlib `flag` only; every flag is validated right after `flag.Parse()` and has a row in the
  README's flag table.
- Manual constructor injection: collector code takes `DockerAPI`, never `*client.Client`.

Adding a package, a flag, a dependency or a Docker SDK call? Read
[references/project-conventions.md](references/project-conventions.md) first.

---

## 18. Concurrency, HTTP server and Prometheus metrics

- Every long-running goroutine is owned by a `sync.WaitGroup` whose owner calls `Wait()`.
- No goroutine per external event: events mark keys dirty in the reconciler's bounded set.
- One writer: only the reconciler writes the service and node caches and the event-driven
  families.
- A recomputed family is published as one snapshot, never `Reset()` and re-`Set`.
- A removed resource's series are deleted, never set to 0; categorical gauges emit every known
  state.
- No label fed by an unbounded value; metric names come from the `metrics_ids.go` constants.

Touching goroutines, locks, caches, the HTTP server or any metric? Read
[references/patterns.md](references/patterns.md) first: it has the full rules for all three
areas, each followed by examples from this codebase.

---

## What to avoid

- logrus or `pkg/errors` (§7).
- `log.Fatal` or `os.Exit` outside `main`: `main()` calls `os.Exit(run())` exactly once so
  deferred cleanup (cancel, `Close`) always runs.
- Unbounded goroutines (§18).
- Setting a metric to 0 when its resource is removed (§18).
- Hardcoded namespace/subsystem/label strings (§11, §18).
- `interface{}` (§11).
- Designing for hypothetical requirements: no configurability, abstractions or helpers for
  features that do not exist yet.
- Skipping or suppressing pre-commit hooks (`--no-verify`).
- Adding comments to code you did not change (§8).

---

## Quick checklist before submitting Go code

- [ ] Imports in 3 groups: stdlib / third-party / local, alphabetical within each
- [ ] No `if err := f(); err != nil` — split to two lines
- [ ] All errors wrapped with `%w`; `errdefs.IsNotFound` / `errors.Is` for classification
- [ ] No `errors.New` or `%w`-less `fmt.Errorf` in a function body: wrap a package-level sentinel
- [ ] `any` not `interface{}`; `new(expr)`, not a pointer-boxing helper
- [ ] Numbers other than 0–3 extracted to named constants (non-test code)
- [ ] Each `//nolint` names specific linters and explains why the fix does not apply
- [ ] No `FIXME` comments
- [ ] Function statement count ≤ 50 (non-test code)
- [ ] No shadowed variables
- [ ] Checked §12 for an existing helper before writing a new one; Docker only through `DockerAPI`
- [ ] Comments say why, not what changed — history is in the commit body (§13)
- [ ] No guessed `time.Sleep` in tests: a positive eventual waits on a channel or `eventually`,
      and any sleep says in a comment which class it is (§14)
- [ ] Metric names and labels built from `metrics_ids.go` constants; any rename or label change
      is reflected in the README metrics section
- [ ] Removed resources `Delete` their series; categorical gauges emit every known state;
      recomputed sets are built whole and published as one snapshot, never `Reset()` and re-`Set`
- [ ] No label fed by an unbounded value
- [ ] No new goroutine per external event — events mark keys dirty for the reconciler; every
      request/response Docker call has a `withDockerTimeout` deadline, and only the event stream
      goes without one
- [ ] Shared state behind a mutex or atomic, with copies returned from locked regions and an
      eviction path for maps keyed by external IDs

## Lint and test loop

1. Run `pre-commit run golangci-lint-fmt --files <changed .go files>` and
   `pre-commit run golangci-lint-full --files <changed .go files>` (or `--all-files`).
2. Run `make go-vet` and `make go-test`; after touching integration-tagged code, also
   `make go-vet-integration`.
3. Fix each report and re-run from step 1 until all of them are clean.
4. Check `git status`: the formatter hook rewrites files in place, so review and keep its changes.
