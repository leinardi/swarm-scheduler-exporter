---
name: go-style-guide
description: >
  Project Go style rules enforced by golangci-lint v2 (all linters) + pre-commit,
  plus this exporter's own patterns (logging, context, metrics, concurrency).
  Apply when writing, editing, or reviewing any .go file in this repo. Consult
  before generating Go code, not after lint fails.
---

# Go Style Guide — swarm-scheduler-exporter

Rules derived from `.golangci.yaml` (golangci-lint v2, `default: all`) and verified
against the existing codebase in `cmd/` and `internal/`. §1–§15 are what the linters
enforce, §16–§18 are review rules, and §19–§27 are this exporter's own patterns. The
long pattern sections (concurrency, HTTP, Prometheus metrics) keep their rules here
and their worked examples in [`references/patterns.md`](references/patterns.md).

golangci-lint is not on `PATH` in every environment; run it through pre-commit
(`pre-commit run golangci-lint-full --all-files`). Never commit with `--no-verify` —
fix the underlying issue instead.

Linters that are **disabled** in `.golangci.yaml`, so their rules do not apply:

| Disabled linter | Reason |
| --- | --- |
| `exhaustruct`, `exhaustruct_v5` | Requires every struct field to be set; too noisy for short-lived structs |
| `gomodguard` | Replaced by `gomodguard_v2`, which is enabled |
| `gochecknoglobals` | Package-level gauge variables are intentional |
| `nonamedreturns` | Named returns are allowed |
| `wsl` | Whitespace style is enforced by `gofumpt` instead |

The formatters (`gci`, `gofmt`, `gofumpt`, `goimports`, `golines`) run in the
`golangci-lint-fmt` hook and in CI.

---

## 1. Import grouping

Three groups, separated by blank lines — this is enforced by both `gci` (explicit
`sections: standard, default, prefix(github.com/leinardi/swarm-scheduler-exporter)`)
and `goimports` (`local-prefixes: github.com/leinardi/swarm-scheduler-exporter`)
simultaneously, and they must agree:

```go
import (
    // Group 1: stdlib
    "context"
    "errors"
    "fmt"

    // Group 2: third-party (everything that is NOT this module)
    "github.com/moby/moby/api/types/swarm"
    "github.com/moby/moby/client"
    "github.com/prometheus/client_golang/prometheus"

    // Group 3: local module (github.com/leinardi/swarm-scheduler-exporter/...)
    labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
    "github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)
```

Within each group imports are sorted alphabetically. A blank line between groups
is required; no blank lines within a group. Getting this wrong triggers both
`gci` and `goimports`.

`internal/labels` is imported as `labelutil` wherever it would otherwise be
confused with Prometheus's own `prometheus.Labels` type.

---

## 2. Error handling

### 2a. No inline error assignment in `if` (`noinlineerr`)

**Wrong:**

```go
if err := doSomething(); err != nil {
```

**Right:**

```go
err := doSomething()
if err != nil {
```

When a variable is already declared in the same scope, use `=` not `:=` for
the second and later assignments:

```go
err := firstThing()
if err != nil { ... }
err = secondThing() // = not :=
if err != nil { ... }
```

### 2b. Wrap errors with `%w` (`errorlint`)

Always wrap errors so callers can use `errors.Is`/`errors.As`:

```go
listResult, listErr := cli.NodeList(ctx, client.NodeListOptions{Filters: nil})
if listErr != nil {
    return fmt.Errorf("node list: %w", listErr)
}
```

The prefix is a short, lowercase phrase naming the operation, not a full
sentence: no capital letters, no trailing period.

Use `errors.Is`/`errors.As` (or Go 1.26's `errors.AsType[T]`) for comparisons, never
`==` on error values — `err113` flags that too. Docker API errors are classified with
`github.com/containerd/errdefs` (`errdefs.IsNotFound`), which works because the Docker
client maps HTTP status codes to those errors.

Prefer flat code with early returns; no `else` after a `return` (`revive`'s
`indent-error-flow`).

### 2c. Errors are sentinels, detail is wrapped (`err113`)

`err113` flags every `errors.New` inside a function body and every `fmt.Errorf`
without a `%w` verb — a static message included. Declare the error once as a
package-level sentinel and attach the runtime detail by wrapping it:

```go
return fmt.Errorf(
    "%w %s (DOCKER_API_VERSION): supported range is %s to %s",
    ErrUnsupportedAPIVersion,
    apiVersion,
    client.MinAPIVersion,
    client.MaxAPIVersion,
)
```

Suppress with `//nolint:err113` only when no sentinel can fit, and say why (§3).

Sentinels are package-level `var`s built with `errors.New`. Export them (`Err…`)
when callers need `errors.Is`; unexported is fine for package-internal use:

```go
var ErrReplicasStateNotConfigured = errors.New("replicas state gauge not configured")
var ErrEmptyFlagValue = errors.New("empty flag value")
var ErrEventsStreamClosed = errors.New("events stream closed")
```

When an error must carry structured data, implement `error` on a private struct
and have the constructor return `error`, not the concrete type, so callers are not
coupled to it (`labelError`/`newLabelError` in `internal/labels`).

### 2d. Aggregating multiple errors

Use `errors.Join`:

```go
var errs []error
for _, item := range items {
    processErr := process(item)
    if processErr != nil {
        errs = append(errs, fmt.Errorf("process %q: %w", item, processErr))
    }
}
return errors.Join(errs...)
```

### 2e. Ignoring errors explicitly

When an error return genuinely cannot be acted upon (writing to a
`ResponseWriter`, `fmt.Fprintln` on stdout, `GaugeVec.Delete`'s bool), assign it
to the blank identifier:

```go
_, _ = fmt.Fprintln(outWriter, "Usage:")
_, _ = io.WriteString(responseWriter, okBody)
```

---

## 3. `nolint` directives

`nolintlint` enforces three things:

- **Specific**: name every linter — no bare `//nolint`
- **Explanation required**: every directive needs `// reason`
- **No unused**: remove directives when the code no longer triggers that linter

The explanation says why the fix does not apply here, not which rule fired (§17).

### Inline (same-line) — for a single statement or return

```go
switch evt.Type { //nolint:exhaustive // the stream is filtered to service and node events
```

### Preceding-line — for a function or type declaration

```go
//nolint:cyclop,gocyclo // inherent: one branch per flag to validate
func validateFlags(...) error {
```

### Multiple linters — comma-separated, no spaces

```go
//nolint:gocyclo,cyclop,gocognit // complexity is inherent: handles cancel, stream error, and EOF
```

Always name all linters that fire. If `gocyclo` AND `cyclop` both fire for a
complex function, suppress both. Same for `gocyclo`/`cyclop`/`gocognit` when
all three exceed their thresholds.

---

## 4. Complexity limits

| Linter | Threshold | Note |
| --- | --- | --- |
| `gocyclo` | 15 | Cyclomatic complexity |
| `cyclop` | 15 | Same metric, different linter — both fire together |
| `gocognit` | 35 | Cognitive complexity |
| `funlen` | 50 statements | Lines are disabled (`lines: -1`) |

Prefer extracting helpers over suppressing. When suppression is the right call
(e.g., a function that branches over many independent config fields), explain
why in the nolint comment.

Test files are exempt from `funlen`, `gocognit`, `gocyclo`, `maintidx` and the
`cyclop` "calculated cyclomatic complexity" check.

---

## 5. Magic numbers (`mnd`)

Numbers 0, 1, 2, 3 are allowed everywhere. Any other literal integer/float in
an `argument`, `case`, `condition`, or `return` position needs a named constant.
Any literal used more than once, or that needs explaining, is a constant too:

```go
const (
    DefaultPollDelay    = 10 * time.Second
    minPollDelay        = 1 * time.Second
    httpShutdownTimeout = 10 * time.Second
    pendingKeyCap       = 4096
    serviceKeysPerCycle = 32
    backoffInitialDelay = 500 * time.Millisecond
    backoffMaxDelay     = 30 * time.Second
)
```

`strings.SplitN` is excluded from mnd checks.

Test files (`_test.go`) are fully exempt from `mnd`.

---

## 6. Type aliases

Use `any` instead of `interface{}`. `gofmt` rewrites `interface{}` → `any`
automatically, but write `any` in new code to avoid the formatter changing
your diff.

---

## 7. Struct size (`gocritic hugeParam`)

Structs passed by value that are over ~80 bytes trigger `hugeParam`. Pass by
pointer instead — or add `//nolint:gocritic // <interface constraint reason>`
when the signature is fixed by an interface (e.g., `slog.Handler`).

The same applies to `rangeValCopy`: iterate large slices by index
(`service := &services[index]`), see [§25](#25-concurrency-and-synchronization).

---

## 8. Line length (`lll`)

Max 140 characters. `golines` wraps automatically, but try to stay within
bounds when writing new code — especially long function signatures and struct
tags. Test files are exempt.

---

## 9. Forbidden packages (`depguard`)

| Forbidden | Use instead |
| --- | --- |
| `github.com/sirupsen/logrus` (rule `logger`; allowed only in `internal/logger`) | `github.com/leinardi/swarm-scheduler-exporter/internal/logger` (`logger.L()`, backed by `log/slog`) |
| `github.com/pkg/errors` (rule `forbidden-forks`) | stdlib `errors` + `fmt.Errorf(...%w...)` |
| `github.com/instana/testify` (rule `forbidden-forks`) | `github.com/stretchr/testify` |
| `github.com/docker/docker/…` and `github.com/moby/moby/…` outside `internal/collector` and `cmd/swarm-scheduler-exporter` (rule `docker-sdk-boundary`) | go through the `DockerAPI` interface in `internal/collector` — this boundary is half of what keeps the exporter read-only against Docker; a new method on `DockerAPI` must only read |

---

## 10. Comments and `godox`

- `FIXME` is flagged by `godox`. Do not leave `FIXME` comments in committed code.
- `TODO` is allowed.
- Comment style: gocritic's `whyNoLint` check is disabled, but all `//nolint`
  directives still need an explanation per nolintlint's `require-explanation` setting.

Doc comments:

- Every exported function, type, and variable has a doc comment beginning with
  the symbol name (`// HealthFunc returns whether the exporter is healthy…`).
- Unexported symbols get one when their purpose is not obvious from the name;
  simple getters/setters may omit it.
- Inline comments explain *why*, not *what* (see [§17](#17-comments-carry-rationale-history-goes-in-the-commit)):

  ```go
  // Cleared before the call: a node event that arrives during it sets the flag again.
  r.nodesDirty = false
  ```

- Large files are divided with `// --- Section name ---` separators
  (`// --- Nodes snapshot management ---`).
- Do not add doc comments or comment scaffolding to code you did not otherwise
  change, e.g. as a side effect of a bug fix.

---

## 11. Duplication (`dupl`)

Avoid copy-pasting blocks longer than ~100 tokens. Extract shared logic into a
helper. Test files are exempt from `dupl`.

---

## 12. Shadowing (`govet shadow`)

`govet` shadow detection is enabled. Avoid re-declaring variables with `:=`
when they shadow an outer-scope variable. Prefer distinct names or
restructuring to avoid shadows — this is why the codebase names errors after
their source (`listErr`, `pollErr`, `shutdownErr`) rather than reusing `err`.

---

## 13. Variable naming (`varnamelen`)

Short variable names are fine in tight scopes (loop indices `i`, `k`, map
values `v`). `varnamelen` flags a name shorter than 3 characters whose last use
is more than 5 lines from its declaration (its defaults: `min-name-length: 3`,
`max-distance: 5`). Test files are exempt.

**Specific rules that bite most often:**

- **Receivers are exempt**: `(f *fakeDocker)`, `(e *labelError)`, `(values *stringSlice)` — all fine.
- **Parameters are checked like locals.** A one-letter parameter passes in a
  three-line function and is flagged as soon as the body grows, so give
  parameters ≥ 3-char descriptive names from the start:

  ```go
  // Wrong — 'r', 'o', 's', 'n' are too short for params
  func newLabelError(r, o, s string) error
  func isNodeSchedulable(n *swarm.Node) bool

  // Right
  func newLabelError(reason, original, sanitized string) error
  func isNodeSchedulable(node *swarm.Node) bool
  ```

- **Local variables** follow the same distance rule: a variable named `c` that
  is still used more than 5 lines later is flagged; rename it to reflect its type
  or role.

Rule of thumb: if the name alone doesn't tell you what the variable holds,
make it longer. The codebase leans long: `parentContext`, `dockerClient`,
`responseWriter`, `loggerInstance`.

---

## 14. `modernize` — no pointer-boxing helpers

The `modernize` linter (`newexpr` check) flags any function whose sole purpose
is to return a pointer to its argument — the generic `func ptr[T any](v T) *T`
included — at the declaration and at every call site. Go 1.26's `new` takes an
expression, so no helper is needed:

```go
// Wrong — flagged twice
func uint64Ptr(v uint64) *uint64 { return &v }
service.Spec.Mode.Replicated = &swarm.ReplicatedService{Replicas: uint64Ptr(3)}

// Right
service.Spec.Mode.Replicated = &swarm.ReplicatedService{Replicas: new(uint64(3))}
```

Taking the address of a local (`val := computeSomething()`, then `&val`) is fine
too, and reads better when the value is computed or used more than once.

---

## 15. Constant strings (`goconst`)

String literals appearing 3+ times with length ≥ 2 should be extracted to a
named constant. Test files are exempt. Label names are the common case here: the
shared ones already exist as constants in `internal/collector/metrics_ids.go`
(`labelStack`, `labelService`, `labelServiceMode`, `labelDisplayName`, `labelState`,
`labelContainer`), and a label used by one family only is a constant next to that
family (`labelNodeRole`, `labelNodeAvailability`, `labelNodeStatus` in `nodes.go`).

---

## 16. Reuse before writing

Every helper below exists so the hand-written version of it is written once. Before adding a
helper, a fake, a label constant or a Docker call, check whether one of these already answers
the question — and if it nearly does, extend it rather than forking it.

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
| Isolating metrics in a collector test | `installDesiredReplicasGauges`, `installReplicasStateGauges`, `installNodesByStateGauge`, `installContainersStateGauge`, `installServiceUpdateGauges` (`gauge_helpers_test.go`) — unregistered vecs or snapshot collectors swapped in and restored on cleanup | registering on `prometheus.DefaultRegisterer` from a test |
| Reading a snapshot collector in a test | `gatherSeries`, `snapshotValue`, `seriesID`, `familySeries` (`gauge_helpers_test.go`) — gather through a throwaway pedantic registry | `testutil.ToFloat64` on a `With(...)`, which snapshot collectors do not have |
| Resetting the package caches between tests | `resetCollectorState(t)` (`types_test.go`) | clearing `metadataCache` / `cachedNodes` by hand |
| Test fixtures for service metadata and labels | `makeTestMetadata`, `serviceLabels`, `baseServiceLabels` (`gauge_helpers_test.go`) | a per-file copy |
| Waiting for a condition in a test | `eventually(t, what, cond)` (`internal/collector/reconciler_test.go`); `eventually(t, timeout, check)` in the integration suite (`test/integration/helpers_wait_test.go`) | `time.Sleep`, or another hand-rolled deadline loop (see §18) |

---

## 17. Comments carry rationale; history goes in the commit

A comment says **why the code is the way it is** — the constraint, the failure it avoids, the
alternative that was rejected and what broke. It does not narrate what changed, when, or at whose
request. That belongs in the commit body, where `git log` and `git blame` can find it and where it
does not rot as the code moves.

```go
// Bad — history in the code.
// Changed after the socket-proxy bug report; used to filter the task list by service ID.

// Good — rationale in the code.
// No service filter on purpose: the IDs would travel URL-encoded in the query, and a socket
// proxy in front of the daemon rejects that URL long before the cluster is large.
```

The same rule is what makes `//nolint` explanations useful: say why the fix does not apply here,
not that the linter complained.

---

## 18. Waiting in tests: classify before you write a sleep

There are no `time.Sleep` calls in this repo's tests today; keep it that way unless a site
really is one of the sleep classes below. Tests reach for a sleep for five different reasons,
and only some of them justify one: decide which of these a site is *before* writing it — the
class dictates the shape.

**Positive eventual — never a sleep.** "Something another goroutine will do has happened": the
reconciler updated a gauge, the listener returned after cancellation, the stream reconnected.
Wait on the signal, or poll with a deadline: a slow machine then costs milliseconds instead of
flaking, and the failure names the contract that was broken rather than "unexpected nil".

- When the goroutine signals completion on a channel, `select` on it against a timeout and
  `t.Fatal` naming what never happened (`TestListenSwarmEvents_CancelDuringPump_NoReconnectCounted`).
- When the only observable is state (a gauge value, a call counter behind `fakeDocker.mu`), use
  `eventually(t, what, cond)` in `internal/collector` or `eventually(t, timeout, check)` in the
  integration suite (§16). A hand-rolled deadline loop in a test body is this class too: use the
  helper.

This class needs something *observable* to poll. Where the only honest observable is unexported,
prefer a small read-only seam on the production type over poking at internals — or record the
call in the fake and poll that.

**Negative assertion — bounded and commented.** "Nothing happens": no second event connection
after a clean stream. There is no condition to poll for; give the wrong behavior a
bounded window to appear, then assert it did not, and say so in a comment so the next reader does
not "fix" it into a wait that cannot exist. Watching the window on a ticker and failing as soon as
the wrong thing appears (`assertNoSecondEventsConnection` in `engine_wire_test.go`) beats a sleep.

**Real elapsed window — a sleep, and the duration is the point.** A backoff step, a staleness
window. Shortening it changes what is asserted. Name it as a constant or a multiple of the
interval under test, never a bare literal chosen by feel.

**Ordering barrier with no quiescence signal — a sleep, and say why no seam exists.** These are
the ones worth revisiting when a seam appears; the comment is what makes that possible.

**Poll tick inside an eventual-wait helper — already correct.** The ticker inside `eventually`.

Two shapes are always wrong: a sleep whose comment says "give X time to Y" where Y is observable,
and a sleep added to make a flaky test pass without deciding which class it belongs to.

---

## 19. Project layout, naming, license header

```text
swarm-scheduler-exporter/
├── cmd/swarm-scheduler-exporter/   # wiring only: flags, construction, goroutines, server lifecycle
│   ├── main.go
│   └── version.go                  # version/commit/date, injected via -ldflags
├── internal/
│   ├── collector/                  # Prometheus collectors and Swarm state logic
│   │   ├── metrics_ids.go          # namespace/subsystem and shared label constants
│   │   ├── docker_api.go           # DockerAPI: the only way into Docker
│   │   ├── types.go                # shared types, metadata + nodes caches (package doc lives here)
│   │   ├── health.go               # health gauge + build-info gauge
│   │   ├── exporter_metrics.go     # self-observability counters/histograms
│   │   ├── nodes.go, desired_replicas.go, replicas_state.go,
│   │   │   service_update.go, containers.go   # one metric family (group) per file
│   ├── labels/sanitize.go          # label sanitization and validation
│   ├── logger/                     # slog configuration, global accessor, plain handler
│   └── server/http.go              # mux for /metrics and /healthz
├── deployments/docker/             # Dockerfile, compose, Dockerfile.dockerignore
├── scripts/                        # shell helpers
└── .mk/                            # Makefile snippets included by Makefile
```

- All application code lives under `internal/`. `cmd/` holds no business logic.
- Each `internal/` package has one responsibility; shared types of a package live in its
  `types.go`. A new metric family gets its own file in `internal/collector/`.
- Package names are lowercase single words (`collector`, `logger`, `server`, `labels`); file
  names are lowercase with underscores (`desired_replicas.go`, `plain_handler.go`).
- Every `.go` file starts with the MIT license block (`Copyright (c) 2025 Roberto Leinardi`),
  followed by the `package` line. A package doc comment (`// Package server owns …`) goes
  between them, once per package — `types.go` carries it for `collector`. A file may add a
  comment below `package` explaining its own responsibility.

---

## 20. Logging

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

## 21. Context

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

## 22. Flags and configuration

- stdlib `flag` only (no cobra/pflag), declared as package-level variables in `main.go`.
  Repeated flags implement `flag.Value` (`stringSlice`, returning `ErrEmptyFlagValue` for an
  empty value).
- Validate every flag right after `flag.Parse()`, before any resource is created. On invalid
  input print to stderr and return a non-zero exit code from `run()`.
- `version`, `commit`, `date` in `version.go` are `var`, not `const`, so `-ldflags -X` can set
  them (`make go-build` does).
- Every flag has a row in the README's flag table, and every documented flag exists.

---

## 23. Dependency injection

Manual constructor injection; no framework. `cmd/` owns construction and passes dependencies down
as explicit parameters (context, Docker client, poll delay). Collector functions take the
`DockerAPI` interface, never `*client.Client`, so tests can pass `fakeDocker`; `main` passes the
real `*client.Client`, which satisfies it with no adapter. The logger is the one global exception
(§20).

---

## 24. Key dependencies

| Dependency | Purpose | Notes |
| --- | --- | --- |
| `log/slog` (stdlib) | Structured logging | Only logger allowed; logrus is banned (§9) |
| `flag` (stdlib) | CLI flags | No cobra/pflag |
| `sync`, `sync/atomic` (stdlib) | Concurrency | Preferred over external sync libraries |
| `github.com/moby/moby/client`, `github.com/moby/moby/api` | Docker Swarm API client and types | Configured from the environment via `client.FromEnv`; only in `internal/collector` and `main` (§9) |
| `github.com/containerd/errdefs` | Docker error classification | `errdefs.IsNotFound` (the client still maps status codes to these errors) |
| `github.com/prometheus/client_golang` | Metrics exposition | `prometheus.MustRegister` for every metric |

OpenTelemetry, gRPC and protobuf come in transitively through the Docker SDK and are not used
directly; the OpenTelemetry modules are still pinned in `go.mod` so vulnerability fixes can be
taken without waiting for the SDK.

The Docker client is always built from the environment, so socket, TCP/TLS and API version are
configured externally:

```go
dockerClient, err := client.New(client.FromEnv)
if err != nil {
    logger.L().Error("docker client init failed", "err", err)
    return 1
}
defer dockerClient.Close()

versionErr := validateClientAPIVersion(dockerClient)
if versionErr != nil {
    logger.L().Error("unsupported Docker API version", "err", versionErr)
    return 1
}
```

API version negotiation is lazy: the client pings the daemon before its first request and
refuses a daemon below `client.MinAPIVersion`. `validateClientAPIVersion` covers the other
path — a `DOCKER_API_VERSION` pin, which skips negotiation — by refusing a version outside
`client.MinAPIVersion`..`client.MaxAPIVersion` at startup.

Every SDK call takes an `…Options` struct and returns a `…Result` (`NodeList` → `.Items`,
`ServiceInspect` → `.Service`, `ContainerInspect` → `.Container`, `Events` → `.Messages` /
`.Err`). `client.Filters` is a map: leave it `nil` for no filter, and allocate it before adding
(`make(client.Filters).Add("type", "service", "node")`) — `Add` on a nil `Filters` panics.

---

## 25. Concurrency and synchronization

Rules (examples in [`references/patterns.md`](references/patterns.md#concurrency)):

- Every long-running goroutine is owned by a `sync.WaitGroup` whose owner calls `Wait()` before
  returning. Use `waitGroup.Go(...)` (as `main`'s `startWorkers` does for the reconciler, the
  listener and the poller). `serveHTTP`'s bare `go` for `Serve` is not waited for: it ends when
  `Shutdown` makes `Serve` return into the buffered error channel.
- **Never spawn unbounded goroutines.** Work triggered by external input (Swarm events) is only
  recorded: the dispatcher marks a key dirty in the reconciler's bounded set (`pendingKeyCap`,
  falling back to a full resync on overflow) without blocking, and the one reconciler goroutine
  does the work. A `go processEvent(...)` inside an event loop is forbidden.
- **One writer.** The reconciler (`reconciler.go`) is the only writer of the metadata and nodes
  caches and of the event-driven families. New event-driven state goes through it, not through a
  second goroutine writing the caches.
- Long-lived workers that handle external input recover panics and log `debug.Stack()`
  (`Reconciler.recoverStep`).
- Read-heavy shared state uses `sync.RWMutex`; `defer` the unlock right after locking.
- Return copies of slices from locked regions, and copy slices received from the Docker client
  before caching them.
- Single-value, high-frequency state uses the `sync/atomic` types (`atomic.Int64`).
- `sync.Once` for one-time init (`logger.L()`); `sync.Map` `LoadOrStore` for "seen once"
  tracking (`labels.warnOnce`).
- A map keyed by an external ID (service, node, container) needs an eviction path — the entry
  is deleted when the resource goes away.

---

## 26. HTTP server

Rules (examples in [`references/patterns.md`](references/patterns.md#http-server)):

- Never `http.ListenAndServe`: build an `http.Server` with explicit `ReadHeaderTimeout`,
  `ReadTimeout`, `WriteTimeout` and `IdleTimeout`.
- Start it in a goroutine, stop it on context cancellation with `Shutdown` under a
  `httpShutdownTimeout` context.
- Handlers are closures over their dependencies returning `http.HandlerFunc`; routes are
  registered in one constructor (`server.NewMuxWithHealth`); path constants (`metricsPath`,
  `healthzPath`) are package `const`s. An unused `*http.Request` is named `_`.
- `/metrics` and `/healthz` take no input that becomes work.

---

## 27. Prometheus metrics

Metrics are the exporter's public contract: a renamed metric, or a label added, removed or
renamed, breaks dashboards and alerts, and must come with the README's metrics section changing
too. Rules (examples in [`references/patterns.md`](references/patterns.md#prometheus-metrics)):

- **Naming**: `<namespace>_<subsystem>_<name>`, built from the constants in `metrics_ids.go`.
  Never hardcode `"swarm"` or `"swarm_service_"` in `GaugeOpts`.
- **Registration**: package-level vars, created and `prometheus.MustRegister`ed by one
  `Configure…` function called once from `main`. Set `ConstLabels: nil` explicitly.
- **Nil guards**: new exported functions that touch a metric return early when its
  `Configure…` has not run.
- **Publish rebuilt sets as a snapshot**: a family whose full set is recomputed on every update
  (replicas state, container state, nodes by state) is a snapshot collector
  (`snapshot_gauge.go`), not a `GaugeVec`. Record every series on a `snapshotBuilder`, call
  `build()` once, and `publish` the result in one swap; on a build error, log and keep the
  previous set. Never `Reset()` a vec and re-`Set` it: a scrape in between sees the family empty
  or partial (#72). Families computed from the same poll share one collector, so a scrape never
  pairs values from different polls. A `GaugeVec` is for series updated one at a time, from
  events (`desired_replicas`, service update state), with `Delete` on removal.
- **Exhaustive zero emission**: a categorical gauge emits every known state (`knownTaskStates`,
  update states, container states) for each subject, zeros included.
- **Delete, never zero**: when a resource is removed, `Delete` its series
  (`ClearServiceUpdateMetrics`, `desiredReplicasGauge.Delete`) — a stale `0` series is a lie.
- **Custom labels**: user `-label` names are appended after the base labels and always
  sanitized (`internal/labels`) before the vec is created.
- **Cardinality**: never use an unbounded value (container or task IDs, timestamps) as a label
  value.

---

## What to avoid

- logrus or `pkg/errors` (§9).
- `log.Fatal` or `os.Exit` outside `main`: `main()` calls `os.Exit(run())` exactly once so
  deferred cleanup (cancel, `Close`) always runs.
- Unbounded goroutines (§25).
- Setting a metric to 0 when its resource is removed (§27).
- Hardcoded namespace/subsystem/label strings (§15, §27).
- `interface{}` (§6).
- Designing for hypothetical requirements: no configurability, abstractions or helpers for
  features that do not exist yet.
- Skipping or suppressing pre-commit hooks (`--no-verify`).
- Adding comments to code you did not change (§10).

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
- [ ] Checked §16 for an existing helper before writing a new one; Docker only through `DockerAPI`
- [ ] Comments say why, not what changed — history is in the commit body (§17)
- [ ] No guessed `time.Sleep` in tests: a positive eventual waits on a channel or `eventually`,
      and any sleep says in a comment which class it is (§18)
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
