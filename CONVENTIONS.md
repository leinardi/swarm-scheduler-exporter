# Code Conventions

This document captures the idioms, patterns, and style rules used throughout this
codebase. Follow it when adding new code so that everything reads as if written by
the same hand.

---

## Table of Contents

1. [Project Structure](#1-project-structure)
2. [File and Package Naming](#2-file-and-package-naming)
3. [License Header and Package Comments](#3-license-header-and-package-comments)
4. [Error Handling](#4-error-handling)
5. [Logging](#5-logging)
6. [Context Usage](#6-context-usage)
7. [Concurrency and Synchronization](#7-concurrency-and-synchronization)
8. [HTTP Handlers and Server](#8-http-handlers-and-server)
9. [Prometheus Metrics](#9-prometheus-metrics)
10. [Configuration and CLI Flags](#10-configuration-and-cli-flags)
11. [Dependency Injection](#11-dependency-injection)
12. [Comment and Documentation Style](#12-comment-and-documentation-style)
13. [Import Organization](#13-import-organization)
14. [Key Dependencies](#14-key-dependencies)
15. [Linter Rules and Tooling](#15-linter-rules-and-tooling)
16. [What to Avoid](#16-what-to-avoid)

---

## 1. Project Structure

```
swarm-scheduler-exporter/
├── cmd/
│   └── swarm-scheduler-exporter/   # Binary entry point (one package per binary)
│       ├── main.go                 # Flag parsing, wiring, lifecycle management
│       └── version.go              # Version variables injected via -ldflags
├── internal/
│   ├── collector/                  # Prometheus collectors and Swarm state logic
│   │   ├── metrics_ids.go          # Namespace/subsystem constants (one file)
│   │   ├── types.go                # Shared types, metadata cache, cache helpers
│   │   ├── health.go               # Health gauge + build-info gauge
│   │   ├── exporter_metrics.go     # Self-observability metrics (counters/histograms)
│   │   ├── nodes.go                # Cluster node state metric
│   │   ├── desired_replicas.go     # Service desired replicas + event stream
│   │   ├── replicas_state.go       # Task state aggregation
│   │   ├── service_update.go       # Update/rollback state metrics
│   │   └── containers.go           # Optional container state metrics
│   ├── labels/
│   │   └── sanitize.go             # Prometheus label sanitization and validation
│   ├── logger/
│   │   ├── logger.go               # slog configuration and global accessor
│   │   └── plain_handler.go        # Custom plain-text slog handler
│   └── server/
│       └── http.go                 # HTTP mux for /metrics and /healthz
├── deployments/
│   └── docker/                     # Dockerfile, docker-compose, .dockerignore
├── scripts/                        # Shell helpers (build scripts, etc.)
├── .mk/                            # Shared Makefile snippets (included by Makefile)
└── .github/                        # CI workflows, PR templates
```

**Principles:**

- All application code lives under `internal/`. Nothing in `internal/` is intended
  to be imported by external modules.
- `cmd/<binary-name>/` contains only the wiring layer: flag parsing, dependency
  construction, goroutine orchestration, and server lifecycle. No business logic.
- Within `internal/`, each sub-package has a single, clear responsibility. Shared
  types used by multiple files in the same package live in `types.go`.
- Prometheus namespace/subsystem constants are centralized in `metrics_ids.go`
  rather than scattered across files.

---

## 2. File and Package Naming

- Package names are **lowercase, single words** with no underscores: `collector`,
  `logger`, `server`, `labels`.
- File names are **lowercase with underscores** when they contain multiple words:
  `plain_handler.go`, `desired_replicas.go`, `replicas_state.go`.
- Each file in `internal/collector/` corresponds to one metric or functional group.
  New metric families get their own file.
- The import alias `labelutil` is used for the `internal/labels` package where it
  would otherwise conflict with Prometheus's own `prometheus.Labels` type:

  ```go
  labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
  ```

---

## 3. License Header and Package Comments

Every `.go` file starts with the MIT license block, followed immediately by the
package declaration. If the package warrants a doc comment, it goes between the
license and the `package` line:

```go
/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
 * ...
 */

// Package server owns the tiny HTTP surface of the exporter.
// It exposes helpers to construct the mux that serves /metrics and /healthz.
package server
```

Files that are part of a larger package (e.g., a second file in `collector`) and
do not need an additional package doc comment still carry the license header but
omit the `// Package ...` line—the comment on `types.go` is the canonical one for
the package.

Files may also include a block comment below the `package` declaration to explain
the file's specific responsibility within the package:

```go
package collector

// desired_replicas exposes the gauge "swarm_service_desired_replicas", which tracks
// the scheduler's desired replica count for each service. For replicated services,
// this is the configured replica count. For global services, it approximates the
// number of eligible nodes by evaluating placement constraints and node status.
```

---

## 4. Error Handling

### Wrapping with context

Always wrap errors with `fmt.Errorf` and the `%w` verb so that callers can use
`errors.Is` / `errors.As`:

```go
nodes, err := cli.NodeList(ctx, swarm.NodeListOptions{Filters: filters.Args{}})
if err != nil {
    return fmt.Errorf("node list: %w", err)
}
```

The prefix is a short, lowercase noun phrase describing the operation, not a full
sentence. No capital letters, no trailing period.

### Sentinel errors

Sentinel errors are declared as package-level `var` with the `errors.New`
constructor:

```go
var ErrNoCachedMetadata = errors.New("no cached metadata found for removed service")
var ErrEmptyFlagValue   = errors.New("empty flag value")
var ErrEventsStreamClosed = errors.New("events stream closed")
```

Exported sentinels (`Err…`) are used when callers need to distinguish them with
`errors.Is`. Unexported sentinels are acceptable for internal-only use.

### Custom error types

When an error needs to carry structured data (e.g., for clear messages without
dependencies), implement the `error` interface on a private struct:

```go
type labelError struct {
    reason    string
    original  string
    sanitized string
}

func (e *labelError) Error() string {
    return "invalid label: " + e.reason +
        " (original=" + e.original + ", sanitized=" + e.sanitized + ")"
}

func newLabelError(reason, original, sanitized string) error {
    return &labelError{reason: reason, original: original, sanitized: sanitized}
}
```

The constructor returns `error` (not the concrete type) so callers are not
unnecessarily coupled to the private struct.

### `errors.Is` for well-known library errors

Use `errors.Is` from `errors` (stdlib) and the `errdefs` package from
`github.com/containerd/errdefs` to classify Docker API errors:

```go
if errdefs.IsNotFound(labelErr) {
    continue
}
```

Do **not** import `github.com/pkg/errors`; the linter forbids it.

### Early returns; no `else` after `return`

Prefer flat code with early returns over nested `if/else` chains:

```go
// Good
if err != nil {
    return fmt.Errorf("node list: %w", err)
}
// continue happy path

// Avoid
if err == nil {
    // happy path
} else {
    return fmt.Errorf("node list: %w", err)
}
```

### Ignoring errors explicitly

When an error return genuinely cannot be acted upon (e.g., writing to a
`ResponseWriter` or `fmt.Fprintln` on stdout), assign it to the blank identifier
with a comment where intent is unclear:

```go
_, _ = fmt.Fprintln(outWriter, "Usage:")
_, _ = io.WriteString(responseWriter, okBody)
```

---

## 5. Logging

### Library

Use **`log/slog`** (stdlib, Go 1.21+) exclusively. The global logger is accessed
through `logger.L()`:

```go
import "github.com/leinardi/swarm-scheduler-exporter/internal/logger"

log := logger.L()
log.Info("swarm-scheduler-exporter starting",
    "version", version,
    "commit", commit,
    "date", date,
)
```

`logrus` and any other third-party logging library are **banned** by the linter.

### Log levels

| Level | When to use |
| ------- | ------------- |
| `Debug` | Operational detail useful for development (e.g., "polling loop: context canceled") |
| `Info` | Significant lifecycle events (startup, shutdown, version) |
| `Warn` | Degraded-but-recoverable situations (HTTP shutdown error, failed container poll) |
| `Error` | Failures that affect correctness or availability |

There is no `Fatal` or `Panic` level; treat those as `Error` and return a non-zero
exit code from `run()` instead.

### Structured fields

Always use key-value pairs, never `fmt.Sprintf` inside a log call:

```go
// Good
log.Error("docker client init failed", "err", newClientErr)
log.Error("poll replicas state failed", "err", pollErr)
log.Warn("HTTP server shutdown", "err", shutdownErr)

// Avoid
log.Error(fmt.Sprintf("docker client init failed: %v", newClientErr))
```

Common field names used throughout the codebase:

- `"err"` — the error value
- `"version"`, `"commit"`, `"date"` — build info
- `"every"` — a `time.Duration` interval
- `"label"`, `"sample_value"` — label diagnostics
- `"service_id"` — Docker service identifier

### Logger initialization in goroutines

Goroutines that need a logger call `logger.L()` at the start of the function body,
not at capture time:

```go
waitGroup.Go(func() {
    loggerInstance := logger.L()
    // ...
    loggerInstance.Debug("start polling replicas state", "every", delay)
})
```

---

## 6. Context Usage

- Every function that calls a Docker API or performs I/O takes `context.Context`
  as its **first parameter**, named `ctx` or `parentContext` when it is the root
  context being threaded through:

  ```go
  func UpdateNodesByState(ctx context.Context, cli *client.Client) error { ... }
  func PollReplicasState(parentContext context.Context, dockerClient *client.Client) (serviceCounter, error) { ... }
  ```

- The root context is created once in `run()` via `signal.NotifyContext` and
  canceled on `SIGINT`/`SIGTERM`:

  ```go
  rootContext, cancelRoot := signal.NotifyContext(
      context.Background(),
      syscall.SIGINT,
      syscall.SIGTERM,
  )
  defer cancelRoot()
  ```

- When spawning a goroutine that needs a timeout (e.g., HTTP shutdown), create a
  child context with `context.WithTimeout` and always `defer` the cancel:

  ```go
  shutdownContext, shutdownCancel := context.WithTimeout(parentContext, httpShutdownTimeout)
  defer shutdownCancel()
  ```

- Context cancellation errors are filtered at the call site, not silenced
  universally. Always check `errors.Is(err, context.Canceled)` before logging as
  an error:

  ```go
  if listenErr != nil && !errors.Is(listenErr, context.Canceled) {
      loggerInstance.Error("event listener exited with error", "err", listenErr)
  }
  ```

---

## 7. Concurrency and Synchronization

### sync.WaitGroup for goroutine lifecycle

Long-running goroutines are started with `waitGroup.Go(...)` and the caller
`waitGroup.Wait()` before returning:

```go
var workerGroup sync.WaitGroup
startEventListener(rootContext, &workerGroup, dockerClient)
startPoller(rootContext, &workerGroup, dockerClient, *pollDelay)
// ...
workerGroup.Wait()
```

### RWMutex for read-heavy shared state

Caches that are read far more often than written use `sync.RWMutex`:

```go
var (
    metadataMu    sync.RWMutex
    metadataCache = make(map[string]serviceMetadata)
)

func getServiceMetadata(serviceID string) (serviceMetadata, bool) {
    metadataMu.RLock()
    defer metadataMu.RUnlock()
    metadata, ok := metadataCache[serviceID]
    return metadata, ok
}

func setServiceMetadata(serviceID string, metadata serviceMetadata) {
    metadataMu.Lock()
    defer metadataMu.Unlock()
    metadataCache[serviceID] = metadata
}
```

Always `defer` unlock immediately after acquiring the lock.

### Defensive copies

When returning a slice from a locked region, return a copy to prevent the caller
from holding an implicit reference into shared data:

```go
func getCachedNodes() []swarm.Node {
    nodesMu.RLock()
    defer nodesMu.RUnlock()
    if len(cachedNodes) == 0 {
        return nil
    }
    dst := make([]swarm.Node, len(cachedNodes))
    copy(dst, cachedNodes)
    return dst
}
```

Similarly, when storing a slice received from an external API, copy it before
caching to avoid sharing memory with the Docker client.

### Atomic timestamps

High-frequency, single-value state (timestamps, flags) uses `sync/atomic`:

```go
var lastPollSuccessUnixNano int64 // protected by atomic ops

func MarkPollOK(now time.Time) {
    atomic.StoreInt64(&lastPollSuccessUnixNano, now.UnixNano())
}

func HealthSnapshot(...) (bool, string) {
    lastPoll := time.Unix(0, atomic.LoadInt64(&lastPollSuccessUnixNano))
    // ...
}
```

### sync.Once for one-time initialization

Lazy initialization of the global logger uses `sync.Once`:

```go
var initOnce sync.Once

func L() *slog.Logger {
    initOnce.Do(func() {
        if globalLogger == nil {
            // default init
        }
    })
    return globalLogger
}
```

### sync.Map for one-shot tracking

When you need to record "has this key been seen before" without a full mutex, use
`sync.Map`:

```go
var warnOnce sync.Map // map[string]struct{}

if _, loaded := warnOnce.LoadOrStore(labelKey, struct{}{}); loaded {
    return // already warned
}
```

### Bounded worker pools

Never spawn an unbounded number of goroutines. Worker pools are sized with named
constants and receive work through a buffered channel:

```go
const (
    eventWorkerCount   = 4
    eventQueueCapacity = 256
)
```

### Panic recovery in workers

Long-lived worker goroutines that handle untrusted input recover from panics to
prevent a single bad event from killing the process. Use `runtime/debug.Stack()`
to log the stack trace:

```go
defer func() {
    if r := recover(); r != nil {
        logger.L().Error("worker panic recovered",
            "panic", r,
            "stack", string(debug.Stack()),
        )
    }
}()
```

### Loop indexing to avoid copies

When iterating large slices of structs, iterate by index (not range value) to
avoid copying:

```go
for i := range nodes {
    node := &nodes[i]
    // use node pointer
}
```

---

## 8. HTTP Handlers and Server

### Handler construction

Handlers are constructed as closures that close over their dependencies, returned
as `http.HandlerFunc`:

```go
func healthHandler(isHealthy HealthFunc) http.HandlerFunc {
    return func(responseWriter http.ResponseWriter, _ *http.Request) {
        responseWriter.Header().Set("Content-Type", "text/plain; charset=utf-8")
        ok, reason := isHealthy()
        if ok {
            responseWriter.WriteHeader(http.StatusOK)
            _, _ = io.WriteString(responseWriter, okBody)
            return
        }
        responseWriter.WriteHeader(http.StatusServiceUnavailable)
        _, _ = io.WriteString(responseWriter, reason+"\n")
    }
}
```

The `http.Request` pointer is named `_` when it is unused.

### Mux construction

Route registration happens in a dedicated constructor that returns `*http.ServeMux`:

```go
func NewMuxWithHealth(isHealthy HealthFunc) *http.ServeMux {
    mux := http.NewServeMux()
    mux.Handle(metricsPath, promhttp.Handler())
    mux.HandleFunc(healthzPath, healthHandler(isHealthy))
    return mux
}
```

### Server with explicit timeouts

Never use `http.ListenAndServe` directly. Always construct `http.Server` with
explicit timeout fields:

```go
httpServer := &http.Server{
    Addr:              address,
    Handler:           handler,
    ReadHeaderTimeout: 5 * time.Second,
    ReadTimeout:       10 * time.Second,
    WriteTimeout:      15 * time.Second,
    IdleTimeout:       60 * time.Second,
}
```

### Graceful shutdown

The server is started in a goroutine; shutdown is triggered by context cancellation:

```go
errorChannel := make(chan error, 1)
go func() {
    errorChannel <- httpServer.ListenAndServe()
}()

select {
case resultError = <-errorChannel:
case <-parentContext.Done():
}

shutdownContext, shutdownCancel := context.WithTimeout(parentContext, httpShutdownTimeout)
defer shutdownCancel()
httpServer.Shutdown(shutdownContext)
```

Path constants are kept as package-level `const`, not inlined in `Handle` calls:

```go
const (
    metricsPath = "/metrics"
    healthzPath = "/healthz"
)
```

---

## 9. Prometheus Metrics

### Naming conventions

All metrics follow the pattern `<namespace>_<subsystem>_<name>`:

```go
const (
    prometheusNamespace        = "swarm"
    prometheusExporterSubsystem = "exporter"
    prometheusServiceSubsystem  = "service"
    prometheusTaskSubsystem     = "task"
    prometheusClusterSubsystem  = "cluster"
)
```

These constants live in `metrics_ids.go` and are referenced by all other files
in the package. Never hardcode the namespace/subsystem strings.

### Registration pattern

Each metric (or metric group) is registered by a dedicated `Configure…` function
called once during startup. Metrics are package-level variables:

```go
var nodesByStateGauge *prometheus.GaugeVec

func ConfigureNodesByStateGauge() {
    nodesByStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
        Namespace:   prometheusNamespace,
        Subsystem:   prometheusClusterSubsystem,
        Name:        "nodes_by_state",
        Help:        "Number of Swarm nodes grouped by role, availability, and status.",
        ConstLabels: nil,
    }, []string{"role", "availability", "status"})
    prometheus.MustRegister(nodesByStateGauge)
}
```

`ConstLabels: nil` is always set explicitly (not omitted) for clarity.

### Defensive nil checks on metric functions

Public functions that operate on metrics guard against being called before
`Configure…` has run (e.g., in tests):

```go
func ObservePollDuration(duration time.Duration) {
    if pollDurationHistogram == nil {
        return
    }
    pollDurationHistogram.Observe(duration.Seconds())
}
```

### Reset before re-emission

For gauges that represent the current state of a dynamic set (nodes, services),
call `Reset()` before re-writing to drop series for resources that no longer exist:

```go
nodesByStateGauge.Reset()
// then re-emit all current series
```

### Exhaustive zero emission

For categorical gauges (task state, update state), emit every known value—even
zero—so that Prometheus does not report absent series:

```go
var knownTaskStates = []string{
    string(swarm.TaskStateNew),
    string(swarm.TaskStateRunning),
    // ... all states
}

// For each service, for each state, emit the count (0 if absent):
for _, state := range knownTaskStates {
    labels["state"] = state
    gauge.With(labels).Set(counts[state]) // counts[state] is 0 for missing keys
}
```

### Series deletion on resource removal

When a service is removed, delete its metric series rather than setting them to 0:

```go
func ClearServiceUpdateMetrics(metadata serviceMetadata) {
    for _, state := range knownStates {
        labels := cloneLabelsWithState(baseLabels, state)
        serviceUpdateStateGauge.Delete(labels)
    }
    serviceUpdateStartedTimestamp.Delete(baseLabels)
    serviceUpdateCompletedTimestamp.Delete(baseLabels)
}
```

### Custom labels

Custom labels (user-supplied via `-label`) are appended after base labels and
always sanitized before registration. Helper functions handle the raw↔sanitized
mapping:

```go
baseLabels := append([]string{
    "stack", "service", "service_mode", "display_name",
}, getSanitizedCustomLabelNames()...)

gauge = prometheus.NewGaugeVec(opts, labelutil.SanitizeLabelNames(baseLabels))
```

---

## 10. Configuration and CLI Flags

### stdlib `flag` package

Flags are registered with the standard `flag` package (not cobra, pflag, etc.),
declared as package-level variables:

```go
var (
    listenAddr = flag.String("listen-addr", "0.0.0.0:8888", "IP address and port to bind")
    pollDelay  = flag.Duration("poll-delay", DefaultPollDelay,
        "How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s.")
    logLevel = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
)
```

Repeated flags implement `flag.Value`:

```go
type stringSlice []string

func (values *stringSlice) String() string { return fmt.Sprint(*values) }

func (values *stringSlice) Set(value string) error {
    if value == "" {
        return ErrEmptyFlagValue
    }
    *values = append(*values, value)
    return nil
}
```

### Validation before use

All flag values are validated immediately after `flag.Parse()`, before any
resource is created. Return a non-zero exit code (not `log.Fatal`) on invalid
input:

```go
if *pollDelay < minPollDelay {
    _, _ = fmt.Fprintf(os.Stderr, "poll-delay must be >= %s\n", minPollDelay)
    return 1
}
```

### Version injection

Version, commit, and date are declared as `var` (not `const`) so that `-ldflags`
can override them at build time:

```go
// version.go
var (
    version = "dev"
    commit  = "none"
    date    = "unknown"
)
```

Build command:

```
-ldflags="-X 'main.version=${VERSION}' -X 'main.commit=${COMMIT}' -X 'main.date=${DATE}'"
```

### Magic numbers become named constants

Any numeric literal used more than once, or that requires explanation, must be a
named constant:

```go
const (
    DefaultPollDelay    = 10 * time.Second
    minPollDelay        = 1 * time.Second
    httpShutdownTimeout = 10 * time.Second
    eventWorkerCount    = 4
    eventQueueCapacity  = 256
    backoffInitialDelay = 500 * time.Millisecond
    backoffMaxDelay     = 30 * time.Second
)
```

The mnd linter is configured to allow `0`, `1`, `2`, `3` as bare literals.

---

## 11. Dependency Injection

This project uses **manual constructor injection**—there is no DI framework.

Dependencies (Docker client, context, poll delay) are passed as explicit parameters
to each function that needs them. The `cmd/` layer owns construction and passes
concrete types down:

```go
// in cmd/main.go
dockerClient, err := client.NewClientWithOpts(client.FromEnv)
// ...
startEventListener(rootContext, &workerGroup, dockerClient)
startPoller(rootContext, &workerGroup, dockerClient, *pollDelay)
```

Functions accept concrete types (`*client.Client`) rather than interfaces because
the Docker client interface is not defined in this project and the mock surface is
not needed for production code.

The logger is a global singleton accessed via `logger.L()`. This is a deliberate
exception to the injection pattern; `logger.Set()` exists for tests.

---

## 12. Comment and Documentation Style

### Exported symbols

Every exported function, type, and variable has a doc comment beginning with the
symbol name:

```go
// HealthFunc returns whether the exporter is healthy and, if not, a short reason.
type HealthFunc func() (bool, string)

// NewMuxWithHealth returns an http.ServeMux with:
//   - /metrics bound to the Prometheus exposition endpoint
//   - /healthz returning 200 (healthy) or 503 (unhealthy) using the provided function
func NewMuxWithHealth(isHealthy HealthFunc) *http.ServeMux { ... }
```

### Unexported symbols

Unexported functions and types get doc comments when their purpose is not
immediately obvious from the name:

```go
// shortServiceName returns the visible service name without the "<stack>_" prefix
// when the stack namespace is present and the name follows the standard pattern.
func shortServiceName(stackNS, fullName string) string { ... }
```

Simple getters/setters that follow a clear naming pattern may omit the doc comment.

### Inline comments

Use inline comments to explain *why*, not *what*. Avoid restating the code:

```go
// Copy to avoid sharing memory with the Docker client slice.
dst := make([]swarm.Node, len(nodes))
copy(dst, nodes)

// Capture an anchor *before* we read the world, so any concurrent changes
// during seeding will still be caught by the event stream started with this "since".
initialSinceAnchor := time.Now()
```

### Section separators

Use `// --- Section name ---` comments to divide large files into logical sections:

```go
// --- Package-level state (protected by locks) ---

// --- Nodes snapshot management ---

// --- Service metadata helpers ---
```

---

## 13. Import Organization

Imports are grouped and ordered by `goimports` with the project's local prefix
configured:

```
goimports:
  local-prefixes:
    - github.com/leinardi/swarm-scheduler-exporter
```

This produces three groups (separated by blank lines):

1. Standard library
2. Third-party packages
3. Internal packages (`github.com/leinardi/swarm-scheduler-exporter/…`)

```go
import (
    "context"
    "errors"
    "fmt"
    "sync"

    "github.com/docker/docker/api/types/filters"
    "github.com/docker/docker/api/types/swarm"
    "github.com/docker/docker/client"
    "github.com/prometheus/client_golang/prometheus"

    labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
    "github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)
```

---

## 14. Key Dependencies

| Dependency | Purpose | Notes |
| --- | --- | --- |
| `log/slog` (stdlib) | Structured logging | Only logger allowed; logrus is banned |
| `flag` (stdlib) | CLI flag parsing | No cobra/pflag |
| `sync`, `sync/atomic` (stdlib) | Concurrency primitives | Preferred over external sync libs |
| `github.com/docker/docker` | Docker Swarm API client | Configured from environment via `client.FromEnv` |
| `github.com/containerd/errdefs` | Docker error classification | Used for `IsNotFound` checks |
| `github.com/prometheus/client_golang` | Prometheus metrics exposition | `prometheus.MustRegister` for all metrics |

**Transitive dependencies** (OpenTelemetry, gRPC, protobuf) are pulled in by the
Docker client SDK and are not used directly by application code.

### Docker client initialization

The Docker client is always constructed from environment variables, which allows
socket path, TCP/TLS, and API version to be configured externally:

```go
dockerClient, err := client.NewClientWithOpts(client.FromEnv)
if err != nil {
    logger.L().Error("docker client init failed", "err", err)
    return 1
}
defer dockerClient.Close()
dockerClient.NegotiateAPIVersion(rootContext)
```

`NegotiateAPIVersion` must be called before any API call to ensure compatibility
with the daemon.

---

## 15. Linter Rules and Tooling

The project runs `golangci-lint` with `default: all` (all linters enabled) and
selectively disables only a few:

| Disabled linter | Reason |
| --- | --- |
| `exhaustruct` | Requires every struct field to be set; too noisy for short-lived structs |
| `gochecknoglobals` | Package-level gauge variables are intentional |
| `nonamedreturns` | Named returns not required |
| `wsl` | Whitespace style is enforced by `gofumpt` instead |

**Key enforced rules:**

- `cyclop`: Max cyclomatic complexity 15 per function
- `gocognit`: Max cognitive complexity 35
- `funlen`: Max 50 statements per function (line count is not limited)
- `lll`: Max line length 140 characters
- `mnd`: Magic numbers banned except 0, 1, 2, 3
- `depguard`: `logrus` and `pkg/errors` are banned
- `govet shadow`: Shadowed variable declarations are reported as errors

**Formatters** (`gofmt`, `gofumpt`, `goimports`, `gci`, `golines`) are enforced
in CI. Run them before committing. `gofmt` rewrites `interface{}` to `any`.

**`//nolint` directives** must:

- Name the specific linter: `//nolint:mnd`
- Include an explanation: `//nolint:mnd // sentinel value, not magic`
- Not be left unused (the `nolintlint` linter checks this)

---

## 16. What to Avoid

### Do not use logrus or pkg/errors

Both are banned by the linter. Use `log/slog` and stdlib `errors`/`fmt.Errorf`
respectively.

### Do not use log.Fatal or os.Exit outside of main

`main()` calls `os.Exit(run())` exactly once. Everywhere else, return errors up
the call stack. This ensures deferred cleanup (cancel, Close) always runs.

### Do not spawn unbounded goroutines

Every goroutine that could be triggered by external events (e.g., Swarm events)
must be guarded by a bounded worker pool with a fixed queue. Direct
`go processEvent(...)` patterns inside event loops are forbidden.

### Do not set a metric to 0 when a resource is removed

Removing a resource (service, node) leaves a stale `{service="foo"} 0` series
visible in Prometheus. Call `gauge.Delete(labels)` instead, so the series
disappears from the scrape output.

### Do not hardcode namespace/subsystem strings

Never write `"swarm_service_"` inside a `GaugeOpts`. Use the named constants from
`metrics_ids.go`.

### Do not use interface{} (use any)

`gofmt` is configured to rewrite `interface{}` to `any`. Write `any` in new code.

### Do not design for hypothetical future requirements

The codebase follows a strict "minimum needed for the current task" philosophy.
Do not add configurability, abstractions, or helpers for features that do not yet
exist.

### Do not skip or suppress pre-commit hooks

The project uses pre-commit hooks (`.pre-commit-config.yaml`). Never commit with
`--no-verify`. Fix the underlying issue instead.

### Do not add docstrings or comments to code you did not change

Only add or update comments where logic is non-obvious. Do not add comment
scaffolding to existing functions as a side effect of a bug fix.
