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
	"runtime/debug"
	"strings"
	"sync"
	"time"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/events"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
	"github.com/prometheus/client_golang/prometheus"
)

// desiredReplicasGauge is the gauge vector exported at /metrics.
var desiredReplicasGauge *prometheus.GaugeVec

// Event stream / worker pool configuration.
// These defaults keep memory bounded and provide good throughput.
const (
	eventWorkerCount    = 4                      // number of concurrent event workers
	eventQueueCapacity  = 256                    // buffered queue size for incoming events
	backoffInitialDelay = 500 * time.Millisecond // first reconnect delay
	backoffMaxDelay     = 30 * time.Second       // cap for exponential backoff
	backoffMultiplier   = 2                      // multiplier for exponential backoff
)

// Capacity hint for the node attribute map; avoids magic numbers.
const defaultNodeAttrCapacity = 32

var ErrEventsStreamClosed = errors.New("events stream closed")

// ConfigureDesiredReplicasGauge registers the "swarm_service_desired_replicas" gauge
// with base labels (stack, service, service_mode) plus any user-specified custom labels.
func ConfigureDesiredReplicasGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
		"display_name",
	}, getSanitizedCustomLabelNames()...)

	desiredReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "desired_replicas",
		Help:        "Number of desired replicas for a Swarm service (replicated: configured replicas; global: eligible nodes).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)
}

// InitDesiredReplicasGauge seeds the gauge with current desired replica counts
// by listing services and nodes once at startup (or after node changes).
// It returns a time anchor captured immediately before the first API call,
// which should be used as the initial "Since" value when starting the events stream.
func InitDesiredReplicasGauge(
	parentContext context.Context,
	dockerClient *client.Client,
) (time.Time, error) {
	// Capture an anchor *before* we read the world, so any concurrent changes
	// during seeding will still be caught by the event stream started with this "since".
	initialSinceAnchor := time.Now()

	// Seed service gauges
	services, serviceListErr := dockerClient.ServiceList(parentContext, swarm.ServiceListOptions{
		Filters: filters.Args{},
		Status:  false,
	})
	if serviceListErr != nil {
		return time.Time{}, fmt.Errorf("service list: %w", serviceListErr)
	}

	// Also seed nodes snapshot and nodes-by-state metric
	nodes, nodeListErr := dockerClient.NodeList(
		parentContext,
		swarm.NodeListOptions{Filters: filters.Args{}},
	)
	if nodeListErr != nil {
		return time.Time{}, fmt.Errorf("node list: %w", nodeListErr)
	}

	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes)

	// Reset service desired replicas vector so removed services are dropped.
	desiredReplicasGauge.Reset()

	for index := range services { // avoid copying large struct
		service := &services[index]
		setServiceMetadata(service.ID, buildMetadata(service))
		updateServiceReplicasGauge(
			parentContext,
			dockerClient,
			service,
			mustGetServiceMetadata(service.ID),
		)
		UpdateServiceUpdateMetricsForService(service, mustGetServiceMetadata(service.ID))
	}

	return initialSinceAnchor, nil
}

// ListenSwarmEvents listens to Docker events for service and node changes.
// It maintains a resilient connection with capped exponential backoff,
// and uses a bounded worker pool to process events without unbounded goroutines.
// The stream will include events "since" the given time anchor, so that no changes
// are missed between the initial seeding and the first stream connection.
func ListenSwarmEvents(
	parentContext context.Context,
	dockerClient *client.Client,
	initialSince time.Time,
) error {
	filterArgs := filters.NewArgs()
	filterArgs.Add("type", "service")
	filterArgs.Add("type", "node")

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

		eventChannel, errorChannel := dockerClient.Events(parentContext, events.ListOptions{
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
			eventChannel,
			errorChannel,
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
		} else {
			reconnectSince = time.Now().Add(-500 * time.Millisecond)
		}

		if runErr == nil {
			// Normal exit (no error set by pump).
			return nil
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
	dockerClient *client.Client,
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
			// Stream error—trigger reconnect at the caller.
			dispatcherErr = fmt.Errorf("events stream error: %w", streamErr)

			break dispatchLoop

		case eventMessage, ok := <-eventChannel:
			if !ok {
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
	dockerClient *client.Client,
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
	dockerClient *client.Client,
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
	dockerClient *client.Client,
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
	dockerClient *client.Client,
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
		_ = desiredReplicasGauge.Delete(labelsForMetadata(metadata))
		ClearServiceUpdateMetrics(metadata)
		deleteServiceMetadata(serviceID)

		return nil
	default:
		// treat other service actions as update
	}

	service, _, inspectErr := dockerClient.ServiceInspectWithRaw(
		parentContext,
		serviceID,
		swarm.ServiceInspectOptions{
			InsertDefaults: false,
		},
	)
	if inspectErr != nil {
		return fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
	}

	setServiceMetadata(serviceID, buildMetadata(&service))
	updateServiceReplicasGauge(
		parentContext,
		dockerClient,
		&service,
		mustGetServiceMetadata(serviceID),
	)
	UpdateServiceUpdateMetricsForService(&service, mustGetServiceMetadata(serviceID))

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
func labelsForMetadata(metadata serviceMetadata) prometheus.Labels {
	labels := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
		"display_name": displayName(metadata.stack, metadata.service),
	}
	for key, value := range metadata.customLabels {
		labels[key] = value
		// Warn once if a value looks high-cardinality.
		labelutil.MaybeWarnHighCardinality(key, value)
	}

	return labelutil.SanitizeMetricLabels(labels)
}

// updateServiceReplicasGauge sets the per-service desired replicas value
// based on the service mode and configured replica count.
// For global services, it counts eligible nodes using placement constraints.
func updateServiceReplicasGauge(
	parentContext context.Context,
	dockerClient *client.Client,
	service *swarm.Service,
	metadata serviceMetadata,
) {
	if service.Spec.Mode.Replicated != nil {
		desired := float64(*service.Spec.Mode.Replicated.Replicas)
		setServiceDesiredReplicas(service.ID, desired)
		setDesiredReplicasGauge(metadata, desired)

		return
	}

	// Attempt to use cached nodes if available.
	if nodes := getCachedNodes(); len(nodes) > 0 {
		eligible := float64(countEligibleNodesForServiceFromNodes(nodes, service))
		setServiceDesiredReplicas(service.ID, eligible)
		setDesiredReplicasGauge(metadata, eligible)

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

		return
	}

	desired := float64(eligible)
	setServiceDesiredReplicas(service.ID, desired)
	setDesiredReplicasGauge(metadata, desired)
}

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata serviceMetadata, value float64) {
	desiredReplicasGauge.With(labelsForMetadata(metadata)).Set(value)
}

//
// ---- Helpers for global desired replicas accuracy ----
//

// countActiveNodes returns the number of nodes that are READY and Availability=active.
func countActiveNodes(parentContext context.Context, dockerClient *client.Client) (int, error) {
	nodes, listErr := dockerClient.NodeList(
		parentContext,
		swarm.NodeListOptions{Filters: filters.Args{}},
	)
	if listErr != nil {
		return 0, fmt.Errorf("node list: %w", listErr)
	}

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
	dockerClient *client.Client,
	service *swarm.Service,
) (int, error) {
	nodes, listErr := dockerClient.NodeList(
		parentContext,
		swarm.NodeListOptions{Filters: filters.Args{}},
	)
	if listErr != nil {
		return 0, fmt.Errorf("node list: %w", listErr)
	}

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

// platformMatches returns true if node.Description.Platform matches any required platform.
func platformMatches(node *swarm.Node, required []swarm.Platform) bool {
	nodeOS := ""
	nodeArch := ""

	if node.Description.Platform.OS != "" {
		nodeOS = strings.ToLower(node.Description.Platform.OS)
	}

	if node.Description.Platform.Architecture != "" {
		nodeArch = strings.ToLower(node.Description.Platform.Architecture)
	}

	for index := range required {
		requiredPlatform := &required[index]
		requiredOS := strings.ToLower(requiredPlatform.OS)
		requiredArch := strings.ToLower(requiredPlatform.Architecture)

		osOK := (requiredOS == "" || requiredOS == nodeOS)
		archOK := (requiredArch == "" || requiredArch == nodeArch)

		if osOK && archOK {
			return true
		}
	}

	return false
}

// constraintsMatch evaluates a subset of Swarm placement constraints commonly used in practice.
// Supported forms (== and !=):
//
//	node.role == manager|worker
//	node.hostname == <value>
//	node.id == <value>
//	node.platform.os == <value>
//	node.platform.arch == <value>
//	node.labels.<k> == <value>
//	engine.labels.<k> == <value>
//
// Any unsupported or malformed constraint returns false (conservative).
func constraintsMatch(node *swarm.Node, constraints []string) bool {
	if len(constraints) == 0 {
		return true
	}

	attributes := nodeAttributes(node)

	for index := range constraints {
		constraintExpr := strings.TrimSpace(constraints[index])

		var operator string
		if strings.Contains(constraintExpr, "!=") {
			operator = "!="
		} else if strings.Contains(constraintExpr, "==") {
			operator = "=="
		} else {
			// Unsupported operator
			return false
		}

		parts := strings.SplitN(constraintExpr, operator, 2)
		if len(parts) != 2 {
			return false
		}

		leftKey := strings.TrimSpace(parts[0])
		expectedValue := strings.TrimSpace(parts[1])

		nodeValue, exists := attributes[leftKey]
		if !exists {
			// If the attribute is missing, constraint fails.
			return false
		}

		switch operator {
		case "==":
			if nodeValue != expectedValue {
				return false
			}
		case "!=":
			if nodeValue == expectedValue {
				return false
			}
		default:
			return false
		}
	}

	return true
}

// nodeAttributes flattens a node into a string map keyed by the constraint left-hand side.
func nodeAttributes(node *swarm.Node) map[string]string {
	attributes := make(
		map[string]string,
		defaultNodeAttrCapacity,
	) // small map; keys are fixed + labels

	attributes["node.id"] = node.ID
	attributes["node.hostname"] = node.Description.Hostname
	attributes["node.role"] = strings.ToLower(string(node.Spec.Role))

	// Platform
	if node.Description.Platform.OS != "" {
		attributes["node.platform.os"] = strings.ToLower(node.Description.Platform.OS)
	}

	if node.Description.Platform.Architecture != "" {
		attributes["node.platform.arch"] = strings.ToLower(node.Description.Platform.Architecture)
	}

	// Node labels
	for key, value := range node.Spec.Labels {
		attributes["node.labels."+key] = value
	}

	// Engine labels
	if node.Description.Engine.Labels != nil {
		for key, value := range node.Description.Engine.Labels {
			attributes["engine.labels."+key] = value
		}
	}

	return attributes
}

// refreshNodesAndRecomputeGlobals refreshes the nodes cache once and recomputes
// desired replicas for all global-mode services using the cached nodes.
// It avoids a full metric Reset or ServiceList.
func refreshNodesAndRecomputeGlobals(
	parentContext context.Context,
	dockerClient *client.Client,
) error {
	nodes, listErr := dockerClient.NodeList(
		parentContext,
		swarm.NodeListOptions{Filters: filters.Args{}},
	)
	if listErr != nil {
		return fmt.Errorf("node list: %w", listErr)
	}

	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes) // <— update the cluster metric here

	globalIDs := getGlobalServiceIDs()
	if len(globalIDs) == 0 {
		return nil
	}

	// Recompute desired replicas only for global services using the snapshot.
	for index := range globalIDs {
		serviceID := globalIDs[index]

		// We need the current service spec to properly evaluate constraints/platforms.
		service, _, inspectErr := dockerClient.ServiceInspectWithRaw(
			parentContext,
			serviceID,
			swarm.ServiceInspectOptions{
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

		eligible := float64(countEligibleNodesForServiceFromNodes(nodes, &service))
		setServiceDesiredReplicas(service.ID, eligible)
		setDesiredReplicasGauge(metadata, eligible)
	}

	return nil
}
