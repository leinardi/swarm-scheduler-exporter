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

// desired_replicas exposes the gauge "swarm_service_desired_replicas", which tracks
// the scheduler's desired replica count for each service. For replicated services,
// this is the configured replica count. For global services, it approximates the
// number of eligible nodes by evaluating placement constraints and node status.

import (
	"context"
	"errors"
	"fmt"
	"net"
	"runtime/debug"
	"strings"
	"sync"
	"time"

	"github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus"

	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

// desiredReplicasGauge is the gauge vector exported at /metrics.
var desiredReplicasGauge *prometheus.GaugeVec

// schedulableReplicasGauge tracks the number of replicas that can actually be
// scheduled right now given current node availability and placement constraints.
// For replicated services it equals min(configured, eligible_nodes).
// For global services it equals the eligible-node count (same as desired_replicas).
var schedulableReplicasGauge *prometheus.GaugeVec

// Event stream / worker pool configuration.
// These defaults keep memory bounded and provide good throughput.
const (
	eventWorkerCount    = 4                      // number of concurrent event workers
	eventQueueCapacity  = 256                    // buffered queue size for incoming events
	backoffInitialDelay = 500 * time.Millisecond // first reconnect delay
	backoffMaxDelay     = 30 * time.Second       // cap for exponential backoff
	backoffMultiplier   = 2                      // multiplier for exponential backoff
)

const (
	arch386     = "386"
	archAARCH64 = "aarch64"
	archAMD64   = "amd64"
	archARM     = "arm"
	archARM64   = "arm64"
	archX8664   = "x86_64"
)

// Placement-constraint operators and label key prefixes, as defined by swarmkit.
const (
	constraintOpEqual    = "=="
	constraintOpNotEqual = "!="
	nodeLabelsPrefix     = "node.labels."
	engineLabelsPrefix   = "engine.labels."
)

var ErrEventsStreamClosed = errors.New("events stream closed")

// ConfigureDesiredReplicasGauge registers the "swarm_service_desired_replicas" and
// "swarm_service_schedulable_replicas" gauges with base labels (stack, service,
// service_mode, display_name) plus any user-specified custom labels.
func ConfigureDesiredReplicasGauge() {
	baseLabels := append([]string{
		labelStack,
		labelService,
		labelServiceMode,
		labelDisplayName,
	}, getSanitizedCustomLabelNames()...)

	desiredReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "desired_replicas",
		Help:        "Number of desired replicas for a Swarm service (replicated: configured replicas; global: eligible nodes).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)

	schedulableReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusServiceSubsystem,
		Name:      "schedulable_replicas",
		Help: "Number of replicas that can currently be scheduled given node availability and placement constraints " +
			"(replicated: min(configured, eligible_nodes); global: eligible nodes).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(schedulableReplicasGauge)
}

// InitDesiredReplicasGauge seeds the gauge with current desired replica counts
// by listing services and nodes once, at startup.
// It returns a time anchor captured immediately before the first API call,
// which should be used as the initial "Since" value when starting the events stream.
func InitDesiredReplicasGauge(
	parentContext context.Context,
	dockerClient DockerAPI,
) (time.Time, error) {
	// Capture an anchor *before* we read the world, so any concurrent changes
	// during seeding will still be caught by the event stream started with this "since".
	initialSinceAnchor := time.Now()

	// Seed service gauges
	serviceListResult, serviceListErr := dockerClient.ServiceList(
		parentContext,
		client.ServiceListOptions{
			Filters: nil,
			Status:  false,
		},
	)
	if serviceListErr != nil {
		return time.Time{}, fmt.Errorf("service list: %w", serviceListErr)
	}

	services := serviceListResult.Items

	// Also seed nodes snapshot and nodes-by-state metric
	nodeListResult, nodeListErr := dockerClient.NodeList(
		parentContext,
		client.NodeListOptions{Filters: nil},
	)
	if nodeListErr != nil {
		return time.Time{}, fmt.Errorf("node list: %w", nodeListErr)
	}

	nodes := nodeListResult.Items

	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes)

	// No Reset before seeding: this runs once, at startup, on empty vectors, and a Reset here
	// would let a scrape see the families empty. Removed services are dropped by the Delete
	// calls in processEvent.
	for index := range services { // avoid copying large struct
		service := &services[index]
		builtMetadata := buildMetadata(service)
		setServiceMetadata(service.ID, &builtMetadata)
		metadata := mustGetServiceMetadata(service.ID)
		updateServiceReplicasGauge(
			parentContext,
			dockerClient,
			service,
			&metadata,
		)
		UpdateServiceUpdateMetricsForService(service, &metadata)
	}

	return initialSinceAnchor, nil
}

// ListenSwarmEvents listens to Docker events for service and node changes.
// It maintains a resilient connection with capped exponential backoff,
// and uses a bounded worker pool to process events without unbounded goroutines.
// The stream will include events "since" the given time anchor, so that no changes
// are missed between the initial seeding and the first stream connection.
// It only returns once parentContext is done, and the error it returns always wraps
// parentContext.Err(); it never returns nil.
func ListenSwarmEvents(
	parentContext context.Context,
	dockerClient DockerAPI,
	initialSince time.Time,
) error {
	filterArgs := make(client.Filters).Add("type", "service", "node")

	// Exponential backoff for reconnects.
	backoffDelay := backoffInitialDelay

	// Track where to resume from on reconnects.
	reconnectSince := initialSince

	for {
		select {
		case <-parentContext.Done():
			return fmt.Errorf("event listener stopping: %w", parentContext.Err())
		default:
		}

		eventsResult := dockerClient.Events(parentContext, client.EventsListOptions{
			Since:   reconnectSince.Format(time.RFC3339), // include events since our last anchor
			Filters: filterArgs,
			Until:   "",
		})

		// Mark event stream connected for health.
		MarkEventsConnected(time.Now())

		logger.L().Info("event stream connected; starting dispatcher and workers",
			"worker_count", eventWorkerCount,
			"queue_capacity", eventQueueCapacity,
			"since", reconnectSince.Format(time.RFC3339Nano),
		)

		// Run the dispatcher + worker pool until the stream ends or errors.
		lastSeenEventTime, runErr := runEventPump(
			parentContext,
			dockerClient,
			eventsResult.Messages,
			eventsResult.Err,
		)

		// Reset backoff after a healthy stream that saw at least one event
		if !lastSeenEventTime.IsZero() {
			// We processed at least one event → consider the connection healthy.
			// Reset backoff so the next transient failure won’t be penalized.
			if backoffDelay != backoffInitialDelay {
				logger.L().Debug("resetting events backoff to initial after healthy stream",
					"previous_backoff", backoffDelay,
					"initial_backoff", backoffInitialDelay,
				)
			}

			backoffDelay = backoffInitialDelay
		}
		// -------------------------------------------------------------------------------

		// Update the resume point for the next connection.
		if !lastSeenEventTime.IsZero() {
			reconnectSince = lastSeenEventTime.Add(-500 * time.Millisecond)
		}

		// runEventPump always returns an error. When it ended because we are shutting down,
		// stop here: counting a reconnect and logging "will reconnect" would be wrong.
		if parentContext.Err() != nil {
			return fmt.Errorf("event listener stopping: %w", parentContext.Err())
		}

		// We will reconnect → count it.
		IncEventReconnect()

		// Log and backoff before reconnecting.
		logger.L().Warn("event stream ended; will reconnect",
			"err", runErr,
			"backoff", backoffDelay,
			"next_since", reconnectSince.Format(time.RFC3339Nano),
		)

		// Wait for backoff or context cancellation.
		timer := time.NewTimer(backoffDelay)
		select {
		case <-parentContext.Done():
			timer.Stop()

			return fmt.Errorf("event listener canceled during backoff: %w", parentContext.Err())
		case <-timer.C:
		}

		// Exponential backoff with cap.
		nextBackoff := min(
			time.Duration(int64(backoffDelay)*int64(backoffMultiplier)),
			backoffMaxDelay,
		)

		backoffDelay = nextBackoff
	}
}

// runEventPump wires a bounded queue and a fixed pool of workers to process events.
// It returns when the stream errors/closes or when the context is canceled.
// The returned time is the timestamp of the last event that was dequeued
// (and therefore eligible for processing).
func runEventPump(
	parentContext context.Context,
	dockerClient DockerAPI,
	eventChannel <-chan events.Message,
	errorChannel <-chan error,
) (time.Time, error) {
	// Bounded queue so we never spawn unbounded goroutines.
	jobsChannel := make(chan events.Message, eventQueueCapacity)

	var workerGroup sync.WaitGroup
	workerGroup.Add(eventWorkerCount)

	// Start workers.
	startEventWorkers(parentContext, dockerClient, jobsChannel, eventWorkerCount, &workerGroup)

	// Run dispatcher.
	var lastSeenEventTime time.Time

	dispatcherErr := dispatchEvents(
		parentContext,
		jobsChannel,
		eventChannel,
		errorChannel,
		&lastSeenEventTime,
	)

	// Stop accepting new jobs and wait for workers to finish.
	close(jobsChannel)
	workerGroup.Wait()

	return lastSeenEventTime, dispatcherErr
}

// dispatchEvents fans in Docker events into jobs; returns when stream ends or context cancels.
// It also tracks the timestamp of the last event observed, which is used to resume the stream
// on reconnect without missing changes.
func dispatchEvents(
	parentContext context.Context,
	jobsChannel chan<- events.Message,
	eventChannel <-chan events.Message,
	errorChannel <-chan error,
	lastSeenEventTime *time.Time,
) error {
	var dispatcherErr error

dispatchLoop:
	for {
		select {
		case <-parentContext.Done():
			dispatcherErr = fmt.Errorf("event pump context canceled: %w", parentContext.Err())

			break dispatchLoop

		case streamErr := <-errorChannel:
			// A canceled stream can end with a closed-body error instead of context.Canceled:
			// report the cancellation, so shutdown is not taken for a stream failure.
			if parentContext.Err() != nil {
				dispatcherErr = fmt.Errorf("event pump context canceled: %w", parentContext.Err())

				break dispatchLoop
			}

			// Stream error—trigger reconnect at the caller.
			dispatcherErr = fmt.Errorf("events stream error: %w", streamErr)

			break dispatchLoop

		case eventMessage, ok := <-eventChannel:
			if !ok {
				if parentContext.Err() != nil {
					dispatcherErr = fmt.Errorf("event pump context canceled: %w", parentContext.Err())

					break dispatchLoop
				}

				// Channel closed by Docker client—treat as EOF and reconnect.
				dispatcherErr = fmt.Errorf("events stream closed: %w", ErrEventsStreamClosed)

				break dispatchLoop
			}

			// Update last seen event time from Docker's event timestamps.
			// Prefer TimeNano when present; fall back to Time (seconds).
			if eventMessage.TimeNano > 0 {
				*lastSeenEventTime = time.Unix(0, eventMessage.TimeNano)
			} else if eventMessage.Time > 0 {
				*lastSeenEventTime = time.Unix(eventMessage.Time, 0)
			}

			// Enqueue respecting context.
			select {
			case <-parentContext.Done():
				dispatcherErr = fmt.Errorf("event pump context canceled while enqueueing: %w", parentContext.Err())

				break dispatchLoop
			case jobsChannel <- eventMessage:
			}
		}
	}

	return dispatcherErr
}

// startEventWorkers launches a fixed-size pool consuming from jobs.
func startEventWorkers(
	parentContext context.Context,
	dockerClient DockerAPI,
	jobsChannel <-chan events.Message,
	workerCount int,
	workerGroup *sync.WaitGroup,
) {
	for workerIndex := range workerCount {
		go workerLoop(workerIndex, parentContext, dockerClient, jobsChannel, workerGroup)
	}
}

// workerLoop consumes events from jobs until context is canceled or the channel closes.
func workerLoop(
	workerID int,
	parentContext context.Context,
	dockerClient DockerAPI,
	jobsChannel <-chan events.Message,
	workerGroup *sync.WaitGroup,
) {
	defer workerGroup.Done()

	for {
		select {
		case <-parentContext.Done():
			return
		case eventMessage, ok := <-jobsChannel:
			if !ok {
				// Dispatcher closed the queue—drain done.
				return
			}

			// Take address of a local copy to avoid pointer-to-loop-var issues.
			localMessage := eventMessage
			processEventMessage(workerID, parentContext, dockerClient, &localMessage)
		}
	}
}

// processEventMessage handles a single event with panic recovery and logging.
func processEventMessage(
	workerID int,
	parentContext context.Context,
	dockerClient DockerAPI,
	eventMsg *events.Message,
) {
	defer func() {
		if recovered := recover(); recovered != nil {
			logger.L().Error("event worker recovered from panic",
				"worker", workerID,
				"panic", recovered,
				"evt.type", eventMsg.Type,
				"evt.action", eventMsg.Action,
				"actor.id", eventMsg.Actor.ID,
				"stack", string(debug.Stack()),
			)
		}
	}()

	processErr := processEvent(parentContext, dockerClient, eventMsg)
	if processErr != nil {
		logger.L().Error("error processing event", "worker", workerID, "err", processErr)
	}
}

// processEvent handles node and service events. For nodes, only create/remove
// matters for desired replica approximation. For services, remove deletes the series;
// all other actions trigger a fresh inspect to update the cache and gauge.
func processEvent(
	parentContext context.Context,
	dockerClient DockerAPI,
	evt *events.Message,
) error {
	if evt.Type == "node" {
		switch evt.Action { //nolint:exhaustive // only create/remove affect desired replica approximation here
		case events.ActionCreate, events.ActionRemove, events.ActionUpdate:
			// Node topology/schedulability changed → refresh nodes, recompute only globals.
			refreshErr := refreshNodesAndRecomputeGlobals(parentContext, dockerClient)
			if refreshErr != nil {
				logger.L().Warn("refresh nodes and recompute globals", "err", refreshErr)
			}
		default:
			// Ignore other node events (e.g., updates) for this gauge.
		}

		return nil
	}

	serviceID := evt.Actor.ID

	switch evt.Action { //nolint:exhaustive // for services we only handle remove vs others
	case events.ActionRemove:
		metadata, ok := getServiceMetadata(serviceID)
		if !ok {
			return ErrNoCachedMetadata
		}
		// Delete the series entirely to avoid stale zero-valued metrics.
		_ = desiredReplicasGauge.Delete(labelsForMetadata(&metadata))
		_ = schedulableReplicasGauge.Delete(labelsForMetadata(&metadata))
		ClearServiceUpdateMetrics(&metadata)
		deleteServiceMetadata(serviceID)

		atDesiredLogState.Delete(serviceID)

		return nil
	default:
		// treat other service actions as update
	}

	inspectResult, inspectErr := dockerClient.ServiceInspect(
		parentContext,
		serviceID,
		client.ServiceInspectOptions{
			InsertDefaults: false,
		},
	)
	if inspectErr != nil {
		return fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
	}

	service := inspectResult.Service

	builtMetadata := buildMetadata(&service)
	setServiceMetadata(serviceID, &builtMetadata)
	metadata := mustGetServiceMetadata(serviceID)
	updateServiceReplicasGauge(
		parentContext,
		dockerClient,
		&service,
		&metadata,
	)
	UpdateServiceUpdateMetricsForService(&service, &metadata)

	return nil
}

// mustGetServiceMetadata loads metadata for serviceID; it logs a warning and
// returns an empty (but fully constructed) metadata instance if missing.
func mustGetServiceMetadata(serviceID string) serviceMetadata {
	metadata, ok := getServiceMetadata(serviceID)
	if !ok {
		// This should not happen in current flows; log and return empty labels instead of panicking.
		logger.L().Warn("metadata missing unexpectedly", "service_id", serviceID)

		return serviceMetadata{
			stack:        "",
			service:      "",
			serviceMode:  "",
			customLabels: map[string]string{},
		}
	}

	return metadata
}

// labelsForMetadata builds a sanitized label set (same keys used for With/Delete).
func labelsForMetadata(metadata *serviceMetadata) prometheus.Labels {
	labels := prometheus.Labels{
		labelStack:       metadata.stack,
		labelService:     metadata.service,
		labelServiceMode: metadata.serviceMode,
		labelDisplayName: displayName(metadata.stack, metadata.service),
	}
	for key, value := range metadata.customLabels {
		labels[key] = value
		// Warn once if a value looks high-cardinality.
		labelutil.MaybeWarnHighCardinality(key, value)
	}

	return labelutil.SanitizeMetricLabels(labels)
}

// updateServiceReplicasGauge sets the per-service desired and schedulable replica values.
// For replicated services, desired = configured replicas, schedulable = min(configured, eligible_nodes).
// For global services, both desired and schedulable equal the eligible-node count.
func updateServiceReplicasGauge(
	parentContext context.Context,
	dockerClient DockerAPI,
	service *swarm.Service,
	metadata *serviceMetadata,
) {
	if service.Spec.Mode.Replicated != nil {
		desired := float64(*service.Spec.Mode.Replicated.Replicas)
		setServiceDesiredReplicas(service.ID, desired)
		setDesiredReplicasGauge(metadata, desired)

		// Compute schedulable: min(desired, eligible nodes given constraints+availability).
		// Fall back to desired when no node snapshot is available (startup race).
		schedulable := desired

		if nodes := getCachedNodes(); len(nodes) > 0 {
			eligible := float64(countEligibleNodesForServiceFromNodes(nodes, service))
			schedulable = min(desired, eligible)
		}

		setSchedulableReplicasGauge(metadata, schedulable)

		return
	}

	// Global service: both desired and schedulable equal the eligible-node count.

	// Attempt to use cached nodes if available.
	if nodes := getCachedNodes(); len(nodes) > 0 {
		eligible := float64(countEligibleNodesForServiceFromNodes(nodes, service))
		setServiceDesiredReplicas(service.ID, eligible)
		setDesiredReplicasGauge(metadata, eligible)
		setSchedulableReplicasGauge(metadata, eligible)

		return
	}

	eligible, eligibleErr := countEligibleNodesForService(parentContext, dockerClient, service)
	if eligibleErr != nil {
		logger.L().
			Warn("countEligibleNodesForService failed; falling back to counting active nodes", "err", eligibleErr)

		// Fallback: count READY+active nodes ignoring constraints.
		activeCount, fallbackErr := countActiveNodes(parentContext, dockerClient)
		if fallbackErr != nil {
			logger.L().Warn("countActiveNodes fallback failed", "err", fallbackErr)

			return
		}

		desired := float64(activeCount)
		setServiceDesiredReplicas(service.ID, desired)
		setDesiredReplicasGauge(metadata, desired)
		setSchedulableReplicasGauge(metadata, desired)

		return
	}

	desired := float64(eligible)
	setServiceDesiredReplicas(service.ID, desired)
	setDesiredReplicasGauge(metadata, desired)
	setSchedulableReplicasGauge(metadata, desired)
}

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata *serviceMetadata, value float64) {
	desiredReplicasGauge.With(labelsForMetadata(metadata)).Set(value)
}

// setSchedulableReplicasGauge writes the schedulable replicas gauge value.
// Services with RestartPolicy.Condition=="none" (one-shot/cronjob) are forced
// to 0 because the scheduler is not expected to keep tasks running for them
// — using the raw placement-eligibility value would produce constant
// false-positive alerts of the form running_replicas < schedulable_replicas.
func setSchedulableReplicasGauge(metadata *serviceMetadata, value float64) {
	if metadata.restartConditionNone {
		value = 0
	}

	schedulableReplicasGauge.With(labelsForMetadata(metadata)).Set(value)
}

//
// ---- Helpers for global desired replicas accuracy ----
//

// countActiveNodes returns the number of nodes that are READY and Availability=active.
func countActiveNodes(parentContext context.Context, dockerClient DockerAPI) (int, error) {
	listResult, listErr := dockerClient.NodeList(
		parentContext,
		client.NodeListOptions{Filters: nil},
	)
	if listErr != nil {
		return 0, fmt.Errorf("node list: %w", listErr)
	}

	nodes := listResult.Items
	activeCount := 0

	for index := range nodes {
		node := &nodes[index]
		if isNodeSchedulable(node) {
			activeCount++
		}
	}

	return activeCount, nil
}

// countEligibleNodesForService returns the number of nodes where a GLOBAL service
// would place tasks, based on node schedulability + placement constraints + platforms.
func countEligibleNodesForService(
	parentContext context.Context,
	dockerClient DockerAPI,
	service *swarm.Service,
) (int, error) {
	listResult, listErr := dockerClient.NodeList(
		parentContext,
		client.NodeListOptions{Filters: nil},
	)
	if listErr != nil {
		return 0, fmt.Errorf("node list: %w", listErr)
	}

	nodes := listResult.Items

	// Precompute constraint predicates
	var constraints []string
	if service.Spec.TaskTemplate.Placement != nil &&
		len(service.Spec.TaskTemplate.Placement.Constraints) > 0 {
		constraints = service.Spec.TaskTemplate.Placement.Constraints
	}

	var platforms []swarm.Platform
	if service.Spec.TaskTemplate.Placement != nil &&
		len(service.Spec.TaskTemplate.Placement.Platforms) > 0 {
		platforms = service.Spec.TaskTemplate.Placement.Platforms
	}

	eligibleCount := 0

	for index := range nodes {
		node := &nodes[index]
		if !isNodeSchedulable(node) {
			continue
		}

		if len(platforms) > 0 && !platformMatches(node, platforms) {
			continue
		}

		if !constraintsMatch(node, constraints) {
			continue
		}

		eligibleCount++
	}

	return eligibleCount, nil
}

// countEligibleNodesForServiceFromNodes returns eligible nodes count using a provided snapshot.
// It mirrors countEligibleNodesForService but avoids NodeList round-trips.
func countEligibleNodesForServiceFromNodes(nodes []swarm.Node, service *swarm.Service) int {
	var constraints []string
	if service.Spec.TaskTemplate.Placement != nil &&
		len(service.Spec.TaskTemplate.Placement.Constraints) > 0 {
		constraints = service.Spec.TaskTemplate.Placement.Constraints
	}

	var platforms []swarm.Platform
	if service.Spec.TaskTemplate.Placement != nil &&
		len(service.Spec.TaskTemplate.Placement.Platforms) > 0 {
		platforms = service.Spec.TaskTemplate.Placement.Platforms
	}

	eligibleCount := 0

	for index := range nodes {
		node := &nodes[index]
		if !isNodeSchedulable(node) {
			continue
		}

		if len(platforms) > 0 && !platformMatches(node, platforms) {
			continue
		}

		if !constraintsMatch(node, constraints) {
			continue
		}

		eligibleCount++
	}

	return eligibleCount
}

// isNodeSchedulable applies basic Swarm scheduling preconditions:
// - Node Status is READY
// - Node Availability is active (not paused/drain).
func isNodeSchedulable(node *swarm.Node) bool {
	if node == nil {
		return false
	}

	if node.Status.State != swarm.NodeStateReady {
		return false
	}

	if node.Spec.Availability != swarm.NodeAvailabilityActive {
		return false
	}

	return true
}

// normalizeArch maps common kernel-style architecture names (as reported by
// Docker nodes via uname -m) to the Docker manifest convention used in service
// placement Platforms (GOARCH names). Without this, a manager whose node
// reports "x86_64" never matches a required platform of "amd64".
func normalizeArch(arch string) string {
	switch strings.ToLower(arch) {
	case archX8664, "x86-64", archAMD64:
		return archAMD64
	case archAARCH64, archARM64:
		return archARM64
	case "i386", "i686", arch386:
		return arch386
	case "armv7l", "armv7", "armhf", archARM:
		return archARM
	case "ppc64le":
		return "ppc64le"
	case "s390x":
		return "s390x"
	case "riscv64":
		return "riscv64"
	default:
		return strings.ToLower(arch)
	}
}

// platformMatches returns true if node.Description.Platform matches any required platform.
func platformMatches(node *swarm.Node, required []swarm.Platform) bool {
	nodeOS := strings.ToLower(node.Description.Platform.OS)
	nodeArch := normalizeArch(node.Description.Platform.Architecture)

	for index := range required {
		requiredPlatform := &required[index]
		requiredOS := strings.ToLower(requiredPlatform.OS)
		requiredArch := normalizeArch(requiredPlatform.Architecture)

		osOK := (requiredOS == "" || requiredOS == nodeOS)
		archOK := (requiredArch == "" || requiredArch == nodeArch)

		if osOK && archOK {
			return true
		}
	}

	return false
}

// constraintsMatch evaluates Swarm placement constraints with the same semantics as
// swarmkit's constraint.NodeMatches, so the eligible-node count agrees with the scheduler.
// Supported keys (== and !=):
//
//	node.id == <value>
//	node.hostname == <value>
//	node.role == manager|worker
//	node.platform.os == <value>
//	node.platform.arch == <value>
//	node.ip == <ip>|<cidr>
//	node.labels.<k> == <value>
//	engine.labels.<k> == <value>
//
// Keys and values compare case-insensitively (label names after the prefix stay
// case-sensitive), and a missing label compares as the empty string, so
// "node.labels.gpu != true" matches unlabeled nodes. node.platform.arch compares the
// raw architecture the node reports (e.g. "x86_64"), not the normalized one used by
// platformMatches, because that is what Swarm does. Any unknown key or malformed
// constraint returns false (conservative).
func constraintsMatch(node *swarm.Node, constraints []string) bool {
	for index := range constraints {
		key, operator, expected, ok := parseConstraint(constraints[index])
		if !ok || !nodeMatchesConstraint(node, key, operator, expected) {
			return false
		}
	}

	return true
}

// parseConstraint splits "key op value" the way swarmkit does: operators are tried in
// order (== before !=) and the expression is split on the first one found.
func parseConstraint(constraintExpr string) (key, operator, expected string, ok bool) {
	for _, candidateOperator := range []string{constraintOpEqual, constraintOpNotEqual} {
		if !strings.Contains(constraintExpr, candidateOperator) {
			continue
		}

		parts := strings.SplitN(constraintExpr, candidateOperator, 2)
		key = strings.TrimSpace(parts[0])
		expected = strings.TrimSpace(parts[1])

		if key == "" || expected == "" {
			return "", "", "", false
		}

		return key, candidateOperator, expected, true
	}

	return "", "", "", false
}

// nodeMatchesConstraint evaluates one parsed constraint against a node, following the
// key dispatch of swarmkit's constraint.NodeMatches.
func nodeMatchesConstraint(node *swarm.Node, key, operator, expected string) bool {
	switch {
	case strings.EqualFold(key, "node.id"):
		return matchConstraintValue(operator, expected, node.ID)
	case strings.EqualFold(key, "node.hostname"):
		return matchConstraintValue(operator, expected, node.Description.Hostname)
	case strings.EqualFold(key, "node.ip"):
		return matchNodeIPConstraint(operator, expected, node.Status.Addr)
	case strings.EqualFold(key, "node.role"):
		return matchConstraintValue(operator, expected, string(node.Spec.Role))
	case strings.EqualFold(key, "node.platform.os"):
		return matchConstraintValue(operator, expected, node.Description.Platform.OS)
	case strings.EqualFold(key, "node.platform.arch"):
		return matchConstraintValue(operator, expected, node.Description.Platform.Architecture)
	case hasConstraintKeyPrefix(key, nodeLabelsPrefix):
		// A nil map yields "", which is how Swarm treats a missing label.
		return matchConstraintValue(
			operator,
			expected,
			node.Spec.Labels[key[len(nodeLabelsPrefix):]],
		)
	case hasConstraintKeyPrefix(key, engineLabelsPrefix):
		return matchConstraintValue(
			operator,
			expected,
			node.Description.Engine.Labels[key[len(engineLabelsPrefix):]],
		)
	default:
		return false
	}
}

// hasConstraintKeyPrefix reports whether key is prefix followed by a non-empty label name.
// The length guard matches swarmkit: a bare "node.labels." is an unknown key, not a
// lookup of the empty label, which would make "node.labels. != x" match every node.
func hasConstraintKeyPrefix(key, prefix string) bool {
	return len(key) > len(prefix) && strings.EqualFold(key[:len(prefix)], prefix)
}

// matchConstraintValue compares case-insensitively and inverts the result for !=,
// like swarmkit's Constraint.Match.
func matchConstraintValue(operator, expected, candidate string) bool {
	matched := strings.EqualFold(expected, candidate)
	if operator == constraintOpNotEqual {
		return !matched
	}

	return matched
}

// matchNodeIPConstraint compares the node address against a single IP or a CIDR subnet,
// like swarmkit. A value that is neither never matches, whatever the operator.
func matchNodeIPConstraint(operator, expected, nodeAddr string) bool {
	nodeIP := net.ParseIP(nodeAddr)

	var matched bool

	expectedIP := net.ParseIP(expected)
	if expectedIP != nil {
		matched = expectedIP.Equal(nodeIP)
	} else {
		_, subnet, cidrErr := net.ParseCIDR(expected)
		if cidrErr != nil {
			return false
		}

		matched = subnet.Contains(nodeIP)
	}

	if operator == constraintOpNotEqual {
		return !matched
	}

	return matched
}

// refreshNodesAndRecomputeGlobals refreshes the nodes cache once and recomputes
// desired replicas for all global-mode services using the cached nodes.
// It avoids a full metric Reset or ServiceList.
func refreshNodesAndRecomputeGlobals(
	parentContext context.Context,
	dockerClient DockerAPI,
) error {
	listResult, listErr := dockerClient.NodeList(
		parentContext,
		client.NodeListOptions{Filters: nil},
	)
	if listErr != nil {
		return fmt.Errorf("node list: %w", listErr)
	}

	nodes := listResult.Items

	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes) // <— update the cluster metric here

	globalIDs := getNodeDependentServiceIDs()
	for index := range globalIDs {
		serviceID := globalIDs[index]

		// We need the current service spec to properly evaluate constraints/platforms.
		inspectResult, inspectErr := dockerClient.ServiceInspect(
			parentContext,
			serviceID,
			client.ServiceInspectOptions{
				InsertDefaults: false,
			},
		)
		if inspectErr != nil {
			// If service disappeared during the window, skip.
			if errdefs.IsNotFound(inspectErr) {
				continue
			}

			return fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
		}

		metadata, ok := getServiceMetadata(serviceID)
		if !ok {
			// Should be rare; skip with a warning.
			logger.L().Warn("metadata missing during global recompute", "service_id", serviceID)

			continue
		}

		service := &inspectResult.Service
		eligible := float64(countEligibleNodesForServiceFromNodes(nodes, service))
		setServiceDesiredReplicas(service.ID, eligible)
		setDesiredReplicasGauge(&metadata, eligible)
		setSchedulableReplicasGauge(&metadata, eligible) // same as desired for globals
	}

	// Recompute schedulable_replicas for replicated services using cached constraints.
	// This keeps the gauge accurate when a constrained node changes availability.
	replicatedMetadata := getReplicatedServiceMetadata()
	for index := range replicatedMetadata {
		metadata := replicatedMetadata[index]

		stub := &swarm.Service{}
		if len(metadata.constraints) > 0 || len(metadata.platforms) > 0 {
			stub.Spec.TaskTemplate.Placement = &swarm.Placement{
				Constraints: metadata.constraints,
				Platforms:   metadata.platforms,
			}
		}

		eligible := float64(countEligibleNodesForServiceFromNodes(nodes, stub))
		schedulable := min(metadata.configuredReplicas, eligible)
		setSchedulableReplicasGauge(&metadata, schedulable)
	}

	return nil
}
