# Patterns — concurrency, HTTP server, Prometheus metrics

Rules and worked examples for the *Concurrency, HTTP server and Prometheus metrics* section of
[SKILL.md](../SKILL.md). Each area starts with its rules, followed by what they look like in this
codebase. Snippets are abbreviated from the real code — read the named function before copying
one.

## Contents

- Concurrency
    - Rules
    - Goroutine lifecycle with `sync.WaitGroup`
    - Bounded dirty set instead of a goroutine per event
    - Panic recovery in the reconciler
    - `sync.RWMutex` for read-heavy caches
    - Defensive copies
    - Atomics for single values
    - One-time init and "seen once"
    - Index loops over large structs
- HTTP server
    - Rules
    - Server with explicit timeouts, graceful shutdown
    - Handler and mux construction
- Prometheus metrics
    - Rules
    - Naming constants
    - Registration
    - Nil guard before `Configure…` has run
    - Publish rebuilt sets as a snapshot
    - Exhaustive zero emission
    - Delete, never zero, on removal
    - Custom labels

---

## Concurrency

### Rules

- Every long-running goroutine is owned by a `sync.WaitGroup` whose owner calls `Wait()` before
  returning. Use `waitGroup.Go(...)` (as `main`'s `startWorkers` does for the reconciler, the
  listener and the poller). `serveHTTP`'s bare `go` for `Serve` is not waited for: it ends when
  `Shutdown` makes `Serve` return into the buffered error channel.
- **Never spawn unbounded goroutines.** Work triggered by external input (Swarm events) is only
  recorded: the dispatcher marks a key dirty in the reconciler's bounded set (`pendingKeyCap`,
  falling back to a full resync on overflow) without blocking, and the one reconciler goroutine
  does the work. A `go processEvent(...)` inside an event loop is forbidden.
- **One writer.** The reconciler (`reconciler.go`) is the only writer of the service and node
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

### Goroutine lifecycle with `sync.WaitGroup`

```go
var workerGroup sync.WaitGroup
startWorkers(rootContext, &workerGroup, dockerClient, *pollDelay)
// ...
workerGroup.Wait()
```

Inside each starter the goroutine is launched with `waitGroup.Go(func() { ... })`.

### Bounded dirty set instead of a goroutine per event

External events never get a goroutine each, and the dispatcher never waits for the work they
cause. `Reconciler.markServiceDirty` records the key under a mutex and returns; one reconciler
goroutine inspects dirty keys a bounded number per cycle. Past `pendingKeyCap` the set is dropped
with fresh allocations and a full resync is requested, so memory stays bounded however long the
burst:

```go
if len(r.pendingSet) >= r.pendingCap {
    r.overflowLocked(now) // new map and slice, epoch++, resyncRequested++

    return
}

r.pendingSet[serviceID] = &pendingKey{attempts: 0, notBefore: now}
r.pendingOrder = append(r.pendingOrder, serviceID)
```

### Panic recovery in the reconciler

A step handling external input must not let one bad object kill the process
(`Reconciler.recoverStep`):

```go
defer func() {
    recovered := recover()
    if recovered == nil {
        return
    }

    logger.L().Error("reconciler recovered from panic",
        "step", step,
        "panic", recovered,
        "stack", string(debug.Stack()),
    )
    r.requestResync(time.Now(), "panic")
}()
```

### `sync.RWMutex` for read-heavy caches

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

func setServiceMetadata(serviceID string, metadata *serviceMetadata) {
    metadataMu.Lock()
    defer metadataMu.Unlock()
    // ...
    metadataCache[serviceID] = *metadata
}
```

The cache is keyed by service ID, so it has an eviction path: `deleteServiceMetadata` runs when
the service is removed.

### Defensive copies

A slice leaving a locked region is a copy, so the caller holds no reference into shared data
(`getCachedNodes`):

```go
nodesMu.RLock()
defer nodesMu.RUnlock()

if len(cachedNodes) == 0 {
    return nil
}

dst := make([]swarm.Node, len(cachedNodes))
copy(dst, cachedNodes)

return dst
```

`setCachedNodes` copies on the way in too, so the cache never shares memory with the Docker
client's slice.

### Atomics for single values

```go
var lastPollSuccessUnixNano atomic.Int64 // 0 means "never"

func MarkPollOK(now time.Time) {
    lastPollSuccessUnixNano.Store(now.UnixNano())
}

// in HealthSnapshot:
lastPoll := time.Unix(0, lastPollSuccessUnixNano.Load())
```

### One-time init and "seen once"

`logger.L()` initialises the default logger under a `sync.Once`. `internal/labels` warns once per
label key with a `sync.Map`:

```go
var warnOnce sync.Map // map[string]struct{}

if _, loaded := warnOnce.LoadOrStore(labelKey, struct{}{}); loaded {
    return // already warned
}
```

### Index loops over large structs

```go
for index := range services { // avoid copying large struct
    service := &services[index]
    // ...
}
```

---

## HTTP server

### Rules

- Never `http.ListenAndServe`: build an `http.Server` with explicit `ReadHeaderTimeout`,
  `ReadTimeout`, `WriteTimeout` and `IdleTimeout`.
- Start it in a goroutine, stop it on context cancellation with `Shutdown` under a
  `httpShutdownTimeout` context.
- Handlers are closures over their dependencies returning `http.HandlerFunc`; routes are
  registered in one constructor (`server.NewMuxWithHealth`); path constants (`metricsPath`,
  `healthzPath`) are package `const`s. An unused `*http.Request` is named `_`.
- `/metrics` and `/healthz` take no input that becomes work.

### Server with explicit timeouts, graceful shutdown

`runHTTPServer` in `main.go` binds the address, then `serveHTTP` serves on the listener:

```go
listener, listenErr := new(net.ListenConfig).Listen(parentContext, "tcp", address)
if listenErr != nil {
    return fmt.Errorf("http listen: %w", listenErr)
}

// serveHTTP(parentContext, listener, handler):
httpServer := &http.Server{
    Handler:           handler,
    ReadHeaderTimeout: 5 * time.Second,
    ReadTimeout:       10 * time.Second,
    WriteTimeout:      15 * time.Second,
    IdleTimeout:       60 * time.Second,
}

errorChannel := make(chan error, 1)

go func() {
    errorChannel <- httpServer.Serve(listener)
}()

var resultError error

select {
case resultError = <-errorChannel:
case <-parentContext.Done():
}

shutdownContext, shutdownCancel := context.WithTimeout(
    context.WithoutCancel(parentContext),
    httpShutdownTimeout,
)
defer shutdownCancel()

shutdownErr := httpServer.Shutdown(shutdownContext)
if shutdownErr != nil {
    logger.L().Warn("HTTP server shutdown", "err", shutdownErr)
}
```

The shutdown context is detached from `parentContext` because that context is already done when
shutdown was triggered by a signal, and `Shutdown` given a done context returns at once instead
of waiting up to `httpShutdownTimeout` for in-flight scrapes.

### Handler and mux construction

`internal/server/http.go`:

```go
const (
    metricsPath = "/metrics"
    healthzPath = "/healthz"
)

func NewMuxWithHealth(isHealthy HealthFunc) *http.ServeMux {
    mux := http.NewServeMux()
    mux.Handle(metricsPath, promhttp.Handler())
    mux.HandleFunc(healthzPath, healthHandler(isHealthy))

    return mux
}

func healthHandler(isHealthy HealthFunc) http.HandlerFunc {
    return func(responseWriter http.ResponseWriter, _ *http.Request) {
        responseWriter.Header().Set("Content-Type", "text/plain; charset=utf-8")

        ok, reason := isHealthy()
        if ok {
            responseWriter.WriteHeader(http.StatusOK)
            _, _ = io.WriteString(responseWriter, okBody)

            return
        }
        // ...
        responseWriter.WriteHeader(http.StatusServiceUnavailable)
        _, _ = io.WriteString(responseWriter, reason+"\n")
    }
}
```

Neither handler reads the request: nothing a client sends can become work.

---

## Prometheus metrics

### Rules

Metrics are the exporter's public contract: a renamed metric, or a label added, removed or
renamed, breaks dashboards and alerts, and must come with the README's metrics section changing
too.

- **Naming**: `<namespace>_<subsystem>_<name>`, built from the constants in `metrics_ids.go`.
  Never hardcode `"swarm"` or `"swarm_service_"` in `GaugeOpts`.
- **Registration**: package-level vars, created and `prometheus.MustRegister`ed by one
  `Configure…` function called once from `main`. Set `ConstLabels: nil` explicitly.
- **Nil guards**: new exported functions that touch a metric return early when its
  `Configure…` has not run.
- **Publish rebuilt sets as a snapshot**: a family whose full set is recomputed on every update
  (replicas state, container state, nodes by state) is published by a snapshot collector
  (`snapshotCollector` in `snapshot_gauge.go`; `snapshotFamily` is its single-family form), not a
  `GaugeVec`. Record every series on a `snapshotBuilder`, call `build()` once, and `publish` the
  result in one swap; on a build error, log and keep the previous set. Never `Reset()` a vec and
  re-`Set` it: a scrape in between sees the family empty or partial. Families computed from the
  same poll share one snapshot collector, so a scrape never pairs values from different polls. A
  `GaugeVec` is for series updated one at a time, from events (`desired_replicas`, service update
  state), with `Delete` on removal.
- **Exhaustive zero emission**: a categorical gauge emits every known state (`knownTaskStates`,
  update states, container states) for each subject, zeros included.
- **Delete, never zero**: when a resource is removed, `Delete` its series
  (`ClearServiceUpdateMetrics`, `desiredReplicasGauge.Delete`) — a stale `0` series is a lie.
- **Custom labels**: user `-label` names are appended after the base labels and always
  sanitized (`internal/labels`) before the vec is created.
- **Cardinality**: never use an unbounded value (container or task IDs, timestamps) as a label
  value.

### Naming constants

`internal/collector/metrics_ids.go`:

```go
const (
    prometheusNamespace         = "swarm"
    prometheusExporterSubsystem = "exporter"
    prometheusServiceSubsystem  = "service"
    prometheusTaskSubsystem     = "task"
    prometheusClusterSubsystem  = "cluster"
)
```

The shared label names (`labelStack`, `labelService`, `labelServiceMode`, `labelDisplayName`,
`labelState`, `labelContainer`) live in the same file. Labels used by a single family live next to
it, like the node labels below (`nodes.go`).

### Registration

```go
const (
    labelNodeRole         = "role"
    labelNodeAvailability = "availability"
    labelNodeStatus       = "status"
)

var nodesByStateGauge *snapshotFamily

func ConfigureNodesByStateGauge() {
    nodesByStateGauge = newNodesByStateFamily()
    prometheus.MustRegister(nodesByStateGauge)
}

func newNodesByStateFamily() *snapshotFamily {
    return newSnapshotFamily(
        prometheus.BuildFQName(prometheusNamespace, prometheusClusterSubsystem, "nodes_by_state"),
        "Number of Swarm nodes grouped by role, availability, and status.",
        []string{labelNodeRole, labelNodeAvailability, labelNodeStatus},
    )
}
```

A family updated one series at a time is a `GaugeVec`, with `ConstLabels: nil` set explicitly
(`desired_replicas.go`):

```go
desiredReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
    Namespace:   prometheusNamespace,
    Subsystem:   prometheusServiceSubsystem,
    Name:        "desired_replicas",
    Help:        "Number of desired replicas for a Swarm service (...).",
    ConstLabels: nil,
}, labelutil.SanitizeLabelNames(baseLabels))
prometheus.MustRegister(desiredReplicasGauge)
```

### Nil guard before `Configure…` has run

```go
func ObservePollDuration(duration time.Duration) {
    if pollDurationHistogram == nil {
        return
    }

    pollDurationHistogram.Observe(duration.Seconds())
}
```

### Publish rebuilt sets as a snapshot

A family whose full set is recomputed on every update is built off to the side and swapped in
whole, so series that are gone disappear without a scrape ever seeing the family empty or half
rebuilt (`UpdateNodesByStateFromSlice`):

```go
func UpdateNodesByStateFromSlice(nodes []swarm.Node) {
    if nodesByStateGauge == nil {
        return
    }

    metrics, buildErr := buildNodesByState(nodesByStateGauge, nodes).build()
    if buildErr != nil {
        logger.L().Error("build nodes by state snapshot; keeping previous", "err", buildErr)

        return
    }

    nodesByStateGauge.publish(metrics)
}
```

The build function is pure: it only records series on a `snapshotBuilder`
(`builder.set(family.desc, family.labelNames, labels, value)`) and never touches what is
published. Do not `Reset()` a `GaugeVec` and re-`Set` it: the vec is locked per operation, not
across the rebuild.

### Exhaustive zero emission

```go
var knownTaskStates = []string{
    string(swarm.TaskStateNew),
    string(swarm.TaskStateRunning),
    // ... every state
}

for _, state := range knownTaskStates {
    labels[labelState] = state
    builder.set(families.stateDesc, families.stateLabelNames, labels, counts[state]) // 0 for states with no tasks
}
```

`knownStates` (update states) and `knownContainerStates` follow the same rule.

### Delete, never zero, on removal

```go
// ClearServiceUpdateMetrics deletes all series for a removed service.
func ClearServiceUpdateMetrics(metadata *serviceMetadata) {
    // ... nil guards
    baseLabels := labelsForService(metadata)

    for index := range knownStates {
        labelsWithState := cloneLabelsWithState(baseLabels, knownStates[index])
        _ = serviceUpdateStateGauge.Delete(labelsWithState)
    }

    _ = serviceUpdateStartedTimestamp.Delete(baseLabels)
    _ = serviceUpdateCompletedTimestamp.Delete(baseLabels)
}
```

A `{service="foo"} 0` left behind would tell dashboards the service still exists with nothing
running.

### Custom labels

User `-label` names are validated and sanitized in `main` (`labelutil.ValidateAndSanitizeLabelNames`,
`labelutil.ValidateCustomLabelCount`), then appended after the base labels when the vec is built:

```go
labelNames := append([]string{
    labelStack, labelService, labelServiceMode, labelDisplayName,
}, getSanitizedCustomLabelNames()...)
```

Values go through `labelsForMetadata`, which sanitizes the label map and warns once per key when a
value looks high-cardinality. `labelsForService` (`service_update.go`) and the label set built at
the end of `replicas_state.go` are near-copies of it; new code uses `labelsForMetadata` rather than
adding a fourth.
