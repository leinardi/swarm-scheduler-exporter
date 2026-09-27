/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

package collector

// reconciler owns the exporter's view of Swarm. One goroutine (Reconciler.Run) makes every write
// to the service metadata cache, the nodes cache, the desired / schedulable / update-state
// families and the nodes-by-state family. The event dispatcher only marks keys dirty, under a
// mutex and without blocking, so the Docker event stream is always drained; the reconciler then
// inspects what changed. A full resync (ServiceList + NodeList) seeds the caches at startup,
// backs the event stream up periodically, and recovers whatever the dirty set had to drop.

import (
	"context"
	"maps"
	"runtime/debug"
	"sync"
	"sync/atomic"
	"time"

	"github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"

	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

const (
	// pendingKeyCap bounds the dirty service keys. Past it the set is dropped and a resync
	// requested instead: a resync costs one ServiceList, whatever the number of changes.
	pendingKeyCap = 4096

	// serviceKeysPerCycle bounds the service inspects of one reconcile cycle, so due node
	// refreshes and resyncs are not starved by a burst of service events.
	serviceKeysPerCycle = 32

	// periodicResyncInterval is how often a full resync runs even when no event asked for one.
	// It covers what the event stream cannot: a silent stream, clock skew between the exporter
	// and the daemon, and events the daemon no longer has when the stream reconnects.
	periodicResyncInterval = 5 * time.Minute

	// eventsSinceMargin is subtracted from every event-stream anchor. Replaying an event only
	// marks a key dirty again, which is idempotent; missing one is not.
	eventsSinceMargin = 500 * time.Millisecond
)

// serviceRetryDelays are the waits before retrying a service whose inspect failed with an error
// other than not-found. A failure with no delay left drops the key and requests a resync, which
// will pick the service up with everything else.
var serviceRetryDelays = []time.Duration{500 * time.Millisecond, time.Second, 2 * time.Second}

// activeReconciler is the reconciler HealthSnapshot reports on; Run installs it.
var activeReconciler atomic.Pointer[Reconciler]

// pendingKey is the retry state of one dirty service key.
type pendingKey struct {
	attempts  int
	notBefore time.Time
}

// dueKey is a dirty service key taken for reconciling, with the failures it already had.
type dueKey struct {
	serviceID string
	attempts  int
}

// Reconciler reconciles the caches and the per-service families with Docker. Create it with
// NewReconciler, feed it events through ListenSwarmEvents, and run it with Run.
type Reconciler struct {
	dockerClient DockerAPI

	// Policy, fixed by NewReconciler; tests shorten it before Run.
	pendingCap     int
	retryDelays    []time.Duration
	backoffInitial time.Duration
	backoffMax     time.Duration
	resyncInterval time.Duration

	// wake has room for one signal: an event that arrives while one is pending needs no other.
	wake chan struct{}

	// snapshotRequests and applyRequests carry the poll protocol (reconciler_poll.go).
	snapshotRequests chan snapshotRequest
	applyRequests    chan applyRequest

	// pollRequestHook runs when a poll request is accepted, before it is answered; nil outside
	// tests, which use it to order a cancellation against the answer.
	pollRequestHook func()

	// Loop-owned poll state: what the last poll published per service, and how many polls in a
	// row each service was rejected.
	lastPublished  serviceCounter
	pollRejections map[string]int
	// ready is closed once the first resync completed.
	ready     chan struct{}
	readyOnce sync.Once

	// mu guards everything below. The generations and the dirty set share it, so marking a key
	// dirty and invalidating its generation happen together. mu may be held while taking
	// metadataMu, never the other way round.
	mu sync.Mutex

	// generationCounter hands out generation values, so a service removed and seen again never
	// reuses one.
	generationCounter uint64
	serviceGeneration map[string]uint64
	nodeGeneration    uint64
	epoch             uint64

	resyncRequested        uint64
	resyncCompleted        uint64
	resyncOutstandingSince time.Time
	resyncNotBefore        time.Time
	resyncBackoff          time.Duration
	nextPeriodicResync     time.Time

	// pendingSet and pendingOrder hold the same keys: a key is appended to pendingOrder only when
	// it is newly inserted into pendingSet, and removed from both when taken.
	pendingSet   map[string]*pendingKey
	pendingOrder []string

	nodesDirty     bool
	nodesNotBefore time.Time
	nodesBackoff   time.Duration
}

// NewReconciler returns a reconciler with its first resync already requested.
func NewReconciler(dockerClient DockerAPI) *Reconciler {
	return &Reconciler{
		dockerClient:           dockerClient,
		pendingCap:             pendingKeyCap,
		retryDelays:            serviceRetryDelays,
		backoffInitial:         backoffInitialDelay,
		backoffMax:             backoffMaxDelay,
		resyncInterval:         periodicResyncInterval,
		wake:                   make(chan struct{}, 1),
		snapshotRequests:       make(chan snapshotRequest),
		applyRequests:          make(chan applyRequest),
		lastPublished:          make(serviceCounter),
		pollRejections:         make(map[string]int),
		ready:                  make(chan struct{}),
		serviceGeneration:      make(map[string]uint64),
		resyncRequested:        1,
		resyncOutstandingSince: time.Now(),
		resyncBackoff:          backoffInitialDelay,
		pendingSet:             make(map[string]*pendingKey),
		pendingOrder:           make([]string, 0),
		nodesBackoff:           backoffInitialDelay,
	}
}

// Ready returns a channel closed once the first resync completed: from then on the caches hold
// every service and node.
func (r *Reconciler) Ready() <-chan struct{} {
	return r.ready
}

// Run reconciles until ctx is done. It never sleeps: it waits on one timer armed for the
// earliest scheduled work, on new events, and on ctx.
func (r *Reconciler) Run(ctx context.Context) {
	activeReconciler.Store(r)

	timer := time.NewTimer(time.Hour)
	timer.Stop()

	defer timer.Stop()

	for ctx.Err() == nil {
		r.cycle(ctx)

		delay, scheduled := r.nextWakeDelay(time.Now())
		if scheduled && delay <= 0 {
			continue
		}

		if scheduled {
			timer.Reset(delay)
		}

		select {
		case <-ctx.Done():
			return
		case <-r.wake:
		case <-timer.C:
		case request := <-r.snapshotRequests:
			r.serveSnapshotRequest(ctx, request)
		case request := <-r.applyRequests:
			r.serveApplyRequest(ctx, request)
		}

		timer.Stop()
	}
}

// cycle runs one round of due work, in fairness order: waiting poll requests, the nodes, a
// resync, then at most serviceKeysPerCycle service keys.
func (r *Reconciler) cycle(ctx context.Context) {
	r.servePendingPollRequests(ctx)

	now := time.Now()

	if r.periodicResyncDue(now) {
		r.requestResync(now, "periodic")
	}

	if r.nodesDue(now) {
		r.recoverStep("nodes", func() { r.reconcileNodes(ctx) })
	}

	if r.resyncDue(now) {
		r.recoverStep("resync", func() { r.resync(ctx) })
	}

	for _, key := range r.takeDueKeys(now) {
		if ctx.Err() != nil {
			return
		}

		r.recoverStep("service "+key.serviceID, func() { r.reconcileService(ctx, key) })
	}
}

// recoverStep runs one reconcile step, so a panic on unexpected Docker data costs that step, not
// the process. The step may have stopped halfway, so a resync is requested to repair the caches.
func (r *Reconciler) recoverStep(step string, run func()) {
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
		r.requestResyncAfterPanic(time.Now())
	}()

	run()
}

// signal wakes Run without blocking.
func (r *Reconciler) signal() {
	select {
	case r.wake <- struct{}{}:
	default:
	}
}

// --- Inputs ---

// enqueueEvent marks what evt changed dirty. It never blocks and never calls Docker, so the
// dispatcher keeps draining the stream whatever the reconciler is doing.
func (r *Reconciler) enqueueEvent(evt *events.Message) {
	if evt.Actor.ID == "" {
		IncEventsDropped()
		logger.L().Debug("event without actor ID dropped", "type", evt.Type, "action", evt.Action)

		return
	}

	now := time.Now()

	switch evt.Type { //nolint:exhaustive // the stream is filtered to service and node events
	case events.NodeEventType:
		r.markNodesDirty()
	case events.ServiceEventType:
		r.markServiceDirty(evt.Actor.ID, now)
	default:
		return
	}

	r.signal()
}

// markNodesDirty flags the nodes for a refresh and invalidates the node generation. A pending
// backoff is kept: new events must not turn a failing NodeList into a tight loop.
func (r *Reconciler) markNodesDirty() {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.nodeGeneration++
	r.nodesDirty = true
}

// markServiceDirty invalidates serviceID's generation and queues it, due now. A queued key's
// retry state is reset: a fresh event supersedes a retry.
func (r *Reconciler) markServiceDirty(serviceID string, now time.Time) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.bumpServiceGenerationLocked(serviceID)

	if pending, queued := r.pendingSet[serviceID]; queued {
		pending.attempts = 0
		pending.notBefore = now

		return
	}

	if len(r.pendingSet) >= r.pendingCap {
		r.overflowLocked(now)

		return
	}

	r.pendingSet[serviceID] = &pendingKey{attempts: 0, notBefore: now}
	r.pendingOrder = append(r.pendingOrder, serviceID)
}

// overflowLocked drops the dirty set and requests a resync, which will see every change the set
// held. Fresh allocations release the old backing arrays, and the generations of services that
// are neither cached nor queued go too (the epoch bump already invalidates every poll), so memory
// stays bounded by the cap plus the cache across any number of overflows, even while resyncs fail.
func (r *Reconciler) overflowLocked(now time.Time) {
	logger.L().
		Warn("dirty service keys over the cap; dropping them for a resync", "cap", r.pendingCap)

	r.pendingSet = make(map[string]*pendingKey)
	r.pendingOrder = make([]string, 0)
	r.pruneServiceGenerationsLocked(getAllServiceMetadata())
	r.requestResyncLocked(now)
}

// takeDueKeys removes and returns, in FIFO order, up to serviceKeysPerCycle keys whose notBefore
// has passed. Keys wait until the first resync completed: before it there is no node snapshot to
// compute their counts from.
func (r *Reconciler) takeDueKeys(now time.Time) []dueKey {
	r.mu.Lock()
	defer r.mu.Unlock()

	if r.resyncCompleted == 0 || len(r.pendingOrder) == 0 {
		return nil
	}

	var taken []dueKey

	remaining := r.pendingOrder[:0]

	for _, serviceID := range r.pendingOrder {
		pending := r.pendingSet[serviceID]
		if len(taken) < serviceKeysPerCycle && !pending.notBefore.After(now) {
			taken = append(taken, dueKey{serviceID: serviceID, attempts: pending.attempts})
			delete(r.pendingSet, serviceID)

			continue
		}

		remaining = append(remaining, serviceID)
	}

	r.pendingOrder = remaining

	return taken
}

// --- Scheduling ---

func (r *Reconciler) nodesDue(now time.Time) bool {
	r.mu.Lock()
	defer r.mu.Unlock()

	return r.resyncCompleted > 0 && r.nodesDirty && !r.nodesNotBefore.After(now)
}

func (r *Reconciler) resyncDue(now time.Time) bool {
	r.mu.Lock()
	defer r.mu.Unlock()

	return r.resyncRequested > r.resyncCompleted && !r.resyncNotBefore.After(now)
}

func (r *Reconciler) periodicResyncDue(now time.Time) bool {
	r.mu.Lock()
	defer r.mu.Unlock()

	return r.resyncCompleted > 0 && r.resyncRequested == r.resyncCompleted &&
		!r.nextPeriodicResync.After(now)
}

// requestResyncAfterPanic requests a resync and schedules it after the resync backoff, like a
// failed one. Bad Docker data that made a step panic usually makes the resync panic too, and
// without the backoff Run would retry it in a tight loop.
func (r *Reconciler) requestResyncAfterPanic(now time.Time) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.requestResyncLocked(now)
	r.scheduleResyncRetryLocked(now)
}

// scheduleResyncRetryLocked delays the next resync by the current backoff and grows it.
func (r *Reconciler) scheduleResyncRetryLocked(now time.Time) {
	r.resyncNotBefore = now.Add(r.resyncBackoff)
	r.resyncBackoff = min(r.resyncBackoff*backoffMultiplier, r.backoffMax)
}

// requestResync asks for a full resync and wakes Run, since the caller may be another goroutine
// (the event listener) and Run may have nothing scheduled; reason is logged.
func (r *Reconciler) requestResync(now time.Time, reason string) {
	r.mu.Lock()
	logger.L().Debug("resync requested", "reason", reason)
	r.requestResyncLocked(now)
	r.mu.Unlock()

	r.signal()
}

// requestResyncLocked bumps the epoch, so a poll computed before the request is not published,
// and records when the resync started being outstanding.
func (r *Reconciler) requestResyncLocked(now time.Time) {
	if r.resyncRequested == r.resyncCompleted {
		r.resyncOutstandingSince = now
	}

	r.resyncRequested++
	r.epoch++
}

// nextWakeDelay returns how long Run may wait before the earliest scheduled work, and false when
// nothing is scheduled (only an event can create work then).
func (r *Reconciler) nextWakeDelay(now time.Time) (time.Duration, bool) {
	r.mu.Lock()
	defer r.mu.Unlock()

	var (
		earliest  time.Time
		scheduled bool
	)

	consider := func(at time.Time) {
		if !scheduled || at.Before(earliest) {
			earliest = at
			scheduled = true
		}
	}

	if r.resyncRequested > r.resyncCompleted {
		consider(r.resyncNotBefore)
	}

	if r.resyncCompleted > 0 {
		if r.resyncRequested == r.resyncCompleted {
			consider(r.nextPeriodicResync)
		}

		if r.nodesDirty {
			consider(r.nodesNotBefore)
		}

		for _, pending := range r.pendingSet {
			consider(pending.notBefore)
		}
	}

	return earliest.Sub(now), scheduled
}

// --- Generations ---

func (r *Reconciler) bumpServiceGeneration(serviceID string) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.bumpServiceGenerationLocked(serviceID)
}

func (r *Reconciler) bumpServiceGenerationLocked(serviceID string) {
	r.generationCounter++
	r.serviceGeneration[serviceID] = r.generationCounter
}

func (r *Reconciler) forgetServiceGeneration(serviceID string) {
	r.mu.Lock()
	defer r.mu.Unlock()

	if _, queued := r.pendingSet[serviceID]; queued {
		// An event arrived meanwhile; its key still needs the generation it bumped.
		return
	}

	delete(r.serviceGeneration, serviceID)
}

// pruneServiceGenerations drops the generation of every service that is neither cached nor
// queued, so the map does not keep the IDs of services the dirty set dropped on overflow.
func (r *Reconciler) pruneServiceGenerations(cached map[string]serviceMetadata) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.pruneServiceGenerationsLocked(cached)
}

func (r *Reconciler) pruneServiceGenerationsLocked(cached map[string]serviceMetadata) {
	for serviceID := range r.serviceGeneration {
		_, isCached := cached[serviceID]
		_, isQueued := r.pendingSet[serviceID]

		if !isCached && !isQueued {
			delete(r.serviceGeneration, serviceID)
		}
	}
}

// --- Reconcile steps ---

// reconcileService inspects one dirty service and applies the result: the new spec, its removal
// when Docker no longer has it, or a delayed retry on any other failure.
func (r *Reconciler) reconcileService(ctx context.Context, key dueKey) {
	service, inspectErr := inspectService(ctx, r.dockerClient, key.serviceID)

	switch {
	case inspectErr == nil:
		r.applyService(&service, getCachedNodes())
	case errdefs.IsNotFound(inspectErr):
		r.removeService(key.serviceID)
	case ctx.Err() != nil:
		// Shutting down: the key is not worth keeping.
	default:
		r.retryService(key, inspectErr)
	}
}

// retryService requeues a key whose inspect failed, with the next retry delay, or drops it and
// requests a resync once the delays are used up. A fresh event queued meanwhile wins.
func (r *Reconciler) retryService(key dueKey, inspectErr error) {
	now := time.Now()

	r.mu.Lock()
	defer r.mu.Unlock()

	if _, queued := r.pendingSet[key.serviceID]; queued {
		return
	}

	if key.attempts >= len(r.retryDelays) {
		logger.L().Warn("service inspect keeps failing; leaving it to a resync",
			"service_id", key.serviceID, "attempts", key.attempts+1, "err", inspectErr)
		r.requestResyncLocked(now)

		return
	}

	delay := r.retryDelays[key.attempts]
	logger.L().Debug("service inspect failed; will retry",
		"service_id", key.serviceID, "backoff", delay, "err", inspectErr)

	if len(r.pendingSet) >= r.pendingCap {
		r.overflowLocked(now)

		return
	}

	r.pendingSet[key.serviceID] = &pendingKey{attempts: key.attempts + 1, notBefore: now.Add(delay)}
	r.pendingOrder = append(r.pendingOrder, key.serviceID)
}

// applyService stores the metadata of service and publishes its per-service families, with counts
// computed from nodes. When the label identity changed (a renamed service, a moved stack, a
// changed custom label value), the series under the old identity are deleted first: they would
// otherwise stay forever.
func (r *Reconciler) applyService(service *swarm.Service, nodes []swarm.Node) {
	metadata := buildMetadata(service)

	previous, cached := getServiceMetadata(service.ID)
	if cached && !maps.Equal(labelsForMetadata(&previous), labelsForMetadata(&metadata)) {
		deleteServiceSeries(&previous)
	}

	desired, schedulable := replicaCounts(&metadata, nodes)
	metadata.desiredReplicas = desired

	replaceServiceMetadata(service.ID, &metadata)
	setDesiredReplicasGauge(&metadata, desired)
	setSchedulableReplicasGauge(&metadata, schedulable)
	UpdateServiceUpdateMetricsForService(service, &metadata)

	r.bumpServiceGeneration(service.ID)
}

// removeService deletes the series and the cache entry of a service Docker no longer has.
func (r *Reconciler) removeService(serviceID string) {
	metadata, cached := getServiceMetadata(serviceID)
	if cached {
		deleteServiceSeries(&metadata)
		deleteServiceMetadata(serviceID)
	}

	atDesiredLogState.Delete(serviceID)
	r.forgetServiceGeneration(serviceID)
}

// reconcileNodes refreshes the node snapshot and recomputes every service's counts from it and
// the cached placement. On failure nothing changes, the flag stays set, and the next attempt
// waits for a growing backoff.
func (r *Reconciler) reconcileNodes(ctx context.Context) {
	r.mu.Lock()
	// Cleared before the call: a node event that arrives during it sets the flag again.
	r.nodesDirty = false
	r.mu.Unlock()

	nodes, listErr := listNodes(ctx, r.dockerClient)
	if listErr != nil {
		if ctx.Err() != nil {
			return
		}

		r.mu.Lock()
		r.nodesDirty = true
		r.nodesNotBefore = time.Now().Add(r.nodesBackoff)
		logger.L().
			Warn("node refresh failed; will retry", "err", listErr, "backoff", r.nodesBackoff)
		r.nodesBackoff = min(r.nodesBackoff*backoffMultiplier, r.backoffMax)
		r.mu.Unlock()

		return
	}

	r.applyNodes(nodes)

	r.mu.Lock()
	r.nodesBackoff = r.backoffInitial
	r.mu.Unlock()
}

// applyNodes replaces the node snapshot, recomputes every cached service's counts from it and
// bumps the node generation.
func (r *Reconciler) applyNodes(nodes []swarm.Node) {
	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes)

	cached := getAllServiceMetadata()

	for serviceID := range cached {
		metadata := cached[serviceID]
		desired, schedulable := replicaCounts(&metadata, nodes)
		setServiceDesiredReplicas(serviceID, desired)
		setDesiredReplicasGauge(&metadata, desired)
		setSchedulableReplicasGauge(&metadata, schedulable)
	}

	r.mu.Lock()
	r.nodeGeneration++
	r.mu.Unlock()
}

// resync lists every service and node and applies both. Nothing is written unless both lists
// succeeded; a failure only schedules the next attempt, after a growing backoff, so a daemon
// that keeps failing is not hammered. Requests raised while the pass runs (an overflow, a retry
// running out) are left outstanding for another pass, and so are the keys marked dirty meanwhile.
func (r *Reconciler) resync(ctx context.Context) {
	passStart := time.Now()

	r.mu.Lock()
	target := r.resyncRequested
	r.mu.Unlock()

	services, nodes, listErr := listServicesAndNodes(ctx, r.dockerClient)
	if listErr != nil {
		if ctx.Err() != nil {
			return
		}

		r.mu.Lock()
		logger.L().Warn("resync failed; will retry", "err", listErr, "backoff", r.resyncBackoff)
		r.scheduleResyncRetryLocked(time.Now())
		r.mu.Unlock()

		return
	}

	r.applyNodes(nodes)

	listed := make(map[string]struct{}, len(services))

	for index := range services {
		service := &services[index]
		listed[service.ID] = struct{}{}
		r.applyService(service, nodes)
	}

	for serviceID := range getAllServiceMetadata() {
		if _, stillListed := listed[serviceID]; !stillListed {
			r.removeService(serviceID)
		}
	}

	r.pruneServiceGenerations(getAllServiceMetadata())
	r.completeResync(target, passStart)
}

// completeResync records that every request up to target is served.
func (r *Reconciler) completeResync(target uint64, passStart time.Time) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.resyncCompleted = max(r.resyncCompleted, target)
	r.resyncBackoff = r.backoffInitial
	r.resyncNotBefore = time.Time{}
	r.nextPeriodicResync = time.Now().Add(r.resyncInterval)

	if r.resyncRequested == r.resyncCompleted {
		r.resyncOutstandingSince = time.Time{}
	} else {
		// Raised during this pass, so no earlier than its start.
		r.resyncOutstandingSince = passStart
	}

	r.readyOnce.Do(func() { close(r.ready) })
}

// resyncState returns the resync counters and since when a resync has been outstanding (zero
// when none is).
func (r *Reconciler) resyncState() (requested, completed uint64, outstandingSince time.Time) {
	r.mu.Lock()
	defer r.mu.Unlock()

	return r.resyncRequested, r.resyncCompleted, r.resyncOutstandingSince
}

// --- Helpers ---

// listServicesAndNodes lists both, and fails if either fails.
func listServicesAndNodes(
	ctx context.Context,
	dockerClient DockerAPI,
) ([]swarm.Service, []swarm.Node, error) {
	services, serviceListErr := listServices(ctx, dockerClient)
	if serviceListErr != nil {
		return nil, nil, serviceListErr
	}

	nodes, nodeListErr := listNodes(ctx, dockerClient)
	if nodeListErr != nil {
		return nil, nil, nodeListErr
	}

	return services, nodes, nil
}

// replicaCounts computes a service's desired and schedulable replicas from its metadata and a
// node snapshot. For replicated services, desired is the configured replicas and schedulable
// min(configured, eligible nodes); for replicated jobs, desired is the total completions. For
// global services and global jobs both are the eligible-node count. setSchedulableReplicasGauge
// forces schedulable to 0 for jobs and restart-condition none services.
func replicaCounts(metadata *serviceMetadata, nodes []swarm.Node) (desired, schedulable float64) {
	eligible := float64(countEligibleNodes(nodes, placementFromMetadata(metadata)))

	switch metadata.serviceMode {
	case serviceModeReplicated:
		return metadata.configuredReplicas, min(metadata.configuredReplicas, eligible)
	case serviceModeReplicatedJob:
		return metadata.configuredReplicas, metadata.configuredReplicas
	default:
		return eligible, eligible
	}
}

// deleteServiceSeries deletes every per-service series written under metadata's label identity.
func deleteServiceSeries(metadata *serviceMetadata) {
	labels := labelsForMetadata(metadata)
	_ = desiredReplicasGauge.Delete(labels)
	_ = schedulableReplicasGauge.Delete(labels)

	ClearServiceUpdateMetrics(metadata)
}
