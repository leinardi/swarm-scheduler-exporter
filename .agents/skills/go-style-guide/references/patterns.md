# Pattern examples — concurrency, HTTP server, Prometheus metrics

Worked examples for [SKILL.md](../SKILL.md) §25–§27. The rules live in `SKILL.md`; this file shows
what they look like in this codebase. Snippets are abbreviated from the real code — read the named
function before copying one.

---

## Concurrency

### Goroutine lifecycle with `sync.WaitGroup`

```go
var workerGroup sync.WaitGroup
startEventListener(rootContext, &workerGroup, dockerClient)
startPoller(rootContext, &workerGroup, dockerClient, *pollDelay)
// ...
workerGroup.Wait()
```

Inside each starter the goroutine is launched with `waitGroup.Go(func() { ... })`.

### Bounded worker pool

External events never get a goroutine each. `runEventPump` feeds a buffered queue drained by a
fixed pool. The pool is the one place that uses `Add` up front and a `Done` in each worker
(`startEventWorkers` launches `workerLoop`), because the worker count is known before any worker
starts:

```go
const (
    eventWorkerCount   = 4   // number of concurrent event workers
    eventQueueCapacity = 256 // buffered queue between dispatcher and workers
)

jobsChannel := make(chan events.Message, eventQueueCapacity)

var workerGroup sync.WaitGroup
workerGroup.Add(eventWorkerCount)
startEventWorkers(parentContext, dockerClient, jobsChannel, eventWorkerCount, &workerGroup)
```

### Panic recovery in workers

A worker handling external input must not let one bad event kill the process
(`processEventMessage`):

```go
defer func() {
    if recovered := recover(); recovered != nil {
        logger.L().Error("event worker recovered from panic",
            "worker", workerID,
            "panic", recovered,
            "stack", string(debug.Stack()),
        )
    }
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

### Server with explicit timeouts, graceful shutdown

`runHTTPServer` in `main.go`:

```go
httpServer := &http.Server{
    Addr:              address,
    Handler:           handler,
    ReadHeaderTimeout: 5 * time.Second,
    ReadTimeout:       10 * time.Second,
    WriteTimeout:      15 * time.Second,
    IdleTimeout:       60 * time.Second,
}

errorChannel := make(chan error, 1)

go func() {
    errorChannel <- httpServer.ListenAndServe()
}()

var resultError error

select {
case resultError = <-errorChannel:
case <-parentContext.Done():
}

shutdownContext, shutdownCancel := context.WithTimeout(parentContext, httpShutdownTimeout)
defer shutdownCancel()

shutdownErr := httpServer.Shutdown(shutdownContext)
if shutdownErr != nil {
    logger.L().Warn("HTTP server shutdown", "err", shutdownErr)
}
```

Note that the shutdown context above derives from `parentContext`, which is already done when
shutdown was triggered by a signal, so `Shutdown` does not actually wait up to
`httpShutdownTimeout` for in-flight scrapes. New code that needs a real drain derives the
timeout from `context.WithoutCancel(parentContext)` instead.

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

var nodesByStateGauge *prometheus.GaugeVec

func ConfigureNodesByStateGauge() {
    nodesByStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
        Namespace:   prometheusNamespace,
        Subsystem:   prometheusClusterSubsystem,
        Name:        "nodes_by_state",
        Help:        "Number of Swarm nodes grouped by role, availability, and status.",
        ConstLabels: nil,
    }, []string{labelNodeRole, labelNodeAvailability, labelNodeStatus})
    prometheus.MustRegister(nodesByStateGauge)
}
```

### Nil guard before `Configure…` has run

`UpdateNodesByStateFromSlice` and `UpdateReplicasStateGauge` call `Reset()` without this guard.
They predate the rule and are not a precedent.

```go
func ObservePollDuration(duration time.Duration) {
    if pollDurationHistogram == nil {
        return
    }

    pollDurationHistogram.Observe(duration.Seconds())
}
```

### Reset before re-emission

A gauge describing the current members of a dynamic set drops what is no longer there
(`UpdateNodesByStateFromSlice`):

```go
// Reset to avoid ghost series for statuses we no longer see.
nodesByStateGauge.Reset()
// then re-emit every current series
```

### Exhaustive zero emission

```go
var knownTaskStates = []string{
    string(swarm.TaskStateNew),
    string(swarm.TaskStateRunning),
    // ... every state
}

for _, state := range knownTaskStates {
    labels[labelState] = state
    gauge.With(labels).Set(counts[state]) // 0 for states with no tasks
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
