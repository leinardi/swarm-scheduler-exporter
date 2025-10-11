package collector

// desired_replicas exposes the gauge "swarm_service_desired_replicas", which tracks
// the scheduler's desired replica count for each service. For replicated services,
// this is the configured replica count. For global services, it approximates the
// number of eligible nodes by evaluating placement constraints and node status.

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/events"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/sirupsen/logrus"
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

var ErrEventsStreamClosed = errors.New("events stream closed")

// ConfigureDesiredReplicasGauge registers the "swarm_service_desired_replicas" gauge
// with base labels (stack, service, service_mode) plus any user-specified custom labels.
func ConfigureDesiredReplicasGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
	}, getSanitizedCustomLabelNames()...)

	desiredReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "swarm",
		Subsystem:   "service",
		Name:        "desired_replicas",
		Help:        "Number of desired replicas for a Swarm service (replicated: configured replicas; global: eligible nodes).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)
}

// InitDesiredReplicasGauge seeds the gauge with current desired replica counts
// by listing services and nodes once at startup (or after node changes).
func InitDesiredReplicasGauge(ctx context.Context, cli *client.Client) error {
	// Seed service gauges
	services, err := cli.ServiceList(ctx, types.ServiceListOptions{
		Filters: filters.Args{},
		Status:  false,
	})
	if err != nil {
		return fmt.Errorf("service list: %w", err)
	}

	// Also seed nodes snapshot and nodes-by-state metric
	nodes, nodeErr := cli.NodeList(ctx, types.NodeListOptions{Filters: filters.Args{}})
	if nodeErr != nil {
		return fmt.Errorf("node list: %w", nodeErr)
	}

	setCachedNodes(nodes)
	UpdateNodesByStateFromSlice(nodes)

	// Reset service desired replicas vector so removed services are dropped.
	desiredReplicasGauge.Reset()

	for i := range services { // avoid copying large struct
		svc := &services[i]
		setServiceMetadata(svc.ID, buildMetadata(svc))
		updateServiceReplicasGauge(ctx, cli, svc, mustGetServiceMetadata(svc.ID))
	}

	return nil
}

// mustGetServiceMetadata loads metadata for serviceID; it logs a warning and
// returns an empty (but fully constructed) metadata instance if missing.
func mustGetServiceMetadata(serviceID string) serviceMetadata {
	metadata, ok := getServiceMetadata(serviceID)
	if !ok {
		// This should not happen in current flows; log and return empty labels instead of panicking.
		logrus.WithField("service_id", serviceID).Warn("metadata missing unexpectedly")

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
	ctx context.Context,
	cli *client.Client,
	svc *swarm.Service,
	metadata serviceMetadata,
) {
	if svc.Spec.Mode.Replicated != nil {
		setDesiredReplicasGauge(metadata, float64(*svc.Spec.Mode.Replicated.Replicas))

		return
	}

	// Attempt to use cached nodes if available.
	if nodes := getCachedNodes(); len(nodes) > 0 {
		eligible := countEligibleNodesForServiceFromNodes(nodes, svc)
		setDesiredReplicasGauge(metadata, float64(eligible))

		return
	}

	eligible, err := countEligibleNodesForService(ctx, cli, svc)
	if err != nil {
		logrus.WithError(err).
			Warn("countEligibleNodesForService failed; falling back to counting active nodes")
		// Fallback: count READY+active nodes ignoring constraints.
		allActive, fallbackErr := countActiveNodes(ctx, cli)
		if fallbackErr != nil {
			logrus.WithError(fallbackErr).Warn("countActiveNodes fallback failed")
		} else {
			setDesiredReplicasGauge(metadata, float64(allActive))
		}

		return
	}

	setDesiredReplicasGauge(metadata, float64(eligible))
}

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata serviceMetadata, val float64) {
	desiredReplicasGauge.With(labelsForMetadata(metadata)).Set(val)
}

// ListenSwarmEvents listens to Docker events for service and node changes.
// It maintains a resilient connection with capped exponential backoff,
// and uses a bounded worker pool to process events without unbounded goroutines.
func ListenSwarmEvents(ctx context.Context, cli *client.Client) error {
	filterArgs := filters.NewArgs()
	filterArgs.Add("type", "service")
	filterArgs.Add("type", "node")

	// Exponential backoff for reconnects.
	backoffDelay := backoffInitialDelay

	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("event listener stopping: %w", ctx.Err())
		default:
		}

		eventChan, errorChan := cli.Events(ctx, events.ListOptions{
			Since:   time.Now().Format(time.RFC3339),
			Filters: filterArgs,
			Until:   "",
		})

		// Mark event stream connected for health.
		MarkEventsConnected(time.Now())

		logrus.WithFields(logrus.Fields{
			"worker_count":   eventWorkerCount,
			"queue_capacity": eventQueueCapacity,
		}).Info("Event stream connected; starting dispatcher and workers")

		// Run the dispatcher + worker pool until the stream ends or errors.
		runErr := runEventPump(ctx, cli, eventChan, errorChan)
		if runErr == nil {
			// Normal exit (no error set by pump).
			return nil
		}

		// We will reconnect → count it.
		IncEventReconnect()

		// Log and backoff before reconnecting.
		logrus.WithError(runErr).Warnf("Event stream ended; reconnecting in %s", backoffDelay)

		// Wait for backoff or context cancellation.
		timer := time.NewTimer(backoffDelay)
		select {
		case <-ctx.Done():
			timer.Stop()

			return fmt.Errorf("event listener canceled during backoff: %w", ctx.Err())
		case <-timer.C:
		}

		// Exponential backoff with cap.
		next := time.Duration(int64(backoffDelay) * int64(backoffMultiplier))
		if next > backoffMaxDelay {
			next = backoffMaxDelay
		}

		backoffDelay = next
	}
}

// runEventPump wires a bounded queue and a fixed pool of workers to process events.
// It returns when the stream errors/closes or when the context is canceled.
func runEventPump(
	ctx context.Context,
	cli *client.Client,
	eventChan <-chan events.Message,
	errorChan <-chan error,
) error {
	// Bounded queue so we never spawn unbounded goroutines.
	jobs := make(chan events.Message, eventQueueCapacity)

	var workerGroup sync.WaitGroup
	workerGroup.Add(eventWorkerCount)

	// Start workers.
	for workerIndex := range eventWorkerCount {
		go func(idx int) {
			defer workerGroup.Done()

			for {
				select {
				case <-ctx.Done():
					return
				case msg, ok := <-jobs:
					if !ok {
						// Dispatcher closed the queue—drain done.
						return
					}

					// Take address of a local copy to avoid pointer-to-loop-var issues.
					localMsg := msg

					err := processEvent(ctx, cli, &localMsg)
					if err != nil {
						logrus.WithFields(logrus.Fields{
							"worker": idx,
						}).WithError(err).Error("Error processing event")
					}
				}
			}
		}(workerIndex)
	}

	// Dispatcher: fan-in docker events to the jobs queue; handle errors and shutdown.
	var dispatcherErr error

dispatchLoop:
	for {
		select {
		case <-ctx.Done():
			dispatcherErr = fmt.Errorf("event pump context canceled: %w", ctx.Err())

			break dispatchLoop

		case err := <-errorChan:
			// Stream error—trigger reconnect at the caller.
			dispatcherErr = fmt.Errorf("events stream error: %w", err)

			break dispatchLoop

		case msg, ok := <-eventChan:
			if !ok {
				// Channel closed by Docker client—treat as EOF and reconnect.
				dispatcherErr = fmt.Errorf("events stream closed: %w", ErrEventsStreamClosed)

				break dispatchLoop
			}

			// Non-blocking enqueue with respect to context.
			select {
			case <-ctx.Done():
				dispatcherErr = fmt.Errorf("event pump context canceled while enqueueing: %w", ctx.Err())

				break dispatchLoop
			case jobs <- msg:
			}
		}
	}

	// Stop accepting new jobs and wait for workers to finish.
	close(jobs)
	workerGroup.Wait()

	return dispatcherErr
}

// processEvent handles node and service events. For nodes, only create/remove
// matters for desired replica approximation. For services, remove deletes the series;
// all other actions trigger a fresh inspect to update the cache and gauge.
func processEvent(ctx context.Context, cli *client.Client, evt *events.Message) error {
	if evt.Type == "node" {
		switch evt.Action { //nolint:exhaustive // only create/remove affect desired replica approximation here
		case events.ActionCreate, events.ActionRemove, events.ActionUpdate:
			// Node topology/schedulability changed → refresh nodes, recompute only globals.
			refErr := refreshNodesAndRecomputeGlobals(ctx, cli)
			if refErr != nil {
				logrus.WithError(refErr).Warn("refresh nodes and recompute globals")
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

		deleteServiceMetadata(serviceID)

		return nil
	default:
		// treat other service actions as update
	}

	service, _, err := cli.ServiceInspectWithRaw(ctx, serviceID, types.ServiceInspectOptions{
		InsertDefaults: false,
	})
	if err != nil {
		return fmt.Errorf("service inspect %s: %w", serviceID, err)
	}

	setServiceMetadata(serviceID, buildMetadata(&service))
	updateServiceReplicasGauge(ctx, cli, &service, mustGetServiceMetadata(serviceID))

	return nil
}

//
// ---- Helpers for global desired replicas accuracy ----
//

// countActiveNodes returns the number of nodes that are READY and Availability=active.
func countActiveNodes(ctx context.Context, cli *client.Client) (int, error) {
	nodes, err := cli.NodeList(ctx, types.NodeListOptions{Filters: filters.Args{}})
	if err != nil {
		return 0, fmt.Errorf("node list: %w", err)
	}

	active := 0

	for i := range nodes {
		node := &nodes[i]
		if isNodeSchedulable(node) {
			active++
		}
	}

	return active, nil
}

// countEligibleNodesForService returns the number of nodes where a GLOBAL service
// would place tasks, based on node schedulability + placement constraints + platforms.
func countEligibleNodesForService(
	ctx context.Context,
	cli *client.Client,
	svc *swarm.Service,
) (int, error) {
	nodes, err := cli.NodeList(ctx, types.NodeListOptions{Filters: filters.Args{}})
	if err != nil {
		return 0, fmt.Errorf("node list: %w", err)
	}

	// Precompute constraint predicates
	var constraints []string
	if svc.Spec.TaskTemplate.Placement != nil &&
		len(svc.Spec.TaskTemplate.Placement.Constraints) > 0 {
		constraints = svc.Spec.TaskTemplate.Placement.Constraints
	}

	var platforms []swarm.Platform
	if svc.Spec.TaskTemplate.Placement != nil &&
		len(svc.Spec.TaskTemplate.Placement.Platforms) > 0 {
		platforms = svc.Spec.TaskTemplate.Placement.Platforms
	}

	eligible := 0

	for i := range nodes {
		node := &nodes[i]
		if !isNodeSchedulable(node) {
			continue
		}

		if len(platforms) > 0 && !platformMatches(node, platforms) {
			continue
		}

		if !constraintsMatch(node, constraints) {
			continue
		}

		eligible++
	}

	return eligible, nil
}

// countEligibleNodesForServiceFromNodes returns eligible nodes count using a provided snapshot.
// It mirrors countEligibleNodesForService but avoids NodeList round-trips.
func countEligibleNodesForServiceFromNodes(nodes []swarm.Node, svc *swarm.Service) int {
	var constraints []string
	if svc.Spec.TaskTemplate.Placement != nil &&
		len(svc.Spec.TaskTemplate.Placement.Constraints) > 0 {
		constraints = svc.Spec.TaskTemplate.Placement.Constraints
	}

	var platforms []swarm.Platform
	if svc.Spec.TaskTemplate.Placement != nil &&
		len(svc.Spec.TaskTemplate.Placement.Platforms) > 0 {
		platforms = svc.Spec.TaskTemplate.Placement.Platforms
	}

	eligible := 0

	for i := range nodes {
		node := &nodes[i]
		if !isNodeSchedulable(node) {
			continue
		}

		if len(platforms) > 0 && !platformMatches(node, platforms) {
			continue
		}

		if !constraintsMatch(node, constraints) {
			continue
		}

		eligible++
	}

	return eligible
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

	for i := range required {
		req := &required[i]
		reqOS := strings.ToLower(req.OS)
		reqArch := strings.ToLower(req.Architecture)

		osOK := (reqOS == "" || reqOS == nodeOS)

		archOK := (reqArch == "" || reqArch == nodeArch)
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

	for i := range constraints {
		constraintExpr := strings.TrimSpace(constraints[i])

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

		nodeValue, ok := attributes[leftKey]
		if !ok {
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

// Capacity hint for the node attribute map; avoids magic numbers.
const defaultNodeAttrCapacity = 32

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
	for k, v := range node.Spec.Labels {
		attributes["node.labels."+k] = v
	}

	// Engine labels
	if node.Description.Engine.Labels != nil {
		for k, v := range node.Description.Engine.Labels {
			attributes["engine.labels."+k] = v
		}
	}

	return attributes
}

// refreshNodesAndRecomputeGlobals refreshes the nodes cache once and recomputes
// desired replicas for all global-mode services using the cached nodes.
// It avoids a full metric Reset or ServiceList.
func refreshNodesAndRecomputeGlobals(ctx context.Context, cli *client.Client) error {
	nodes, listErr := cli.NodeList(ctx, types.NodeListOptions{Filters: filters.Args{}})
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
	for i := range globalIDs {
		serviceID := globalIDs[i]

		// We need the current service spec to properly evaluate constraints/platforms.
		svc, _, inspErr := cli.ServiceInspectWithRaw(ctx, serviceID, types.ServiceInspectOptions{
			InsertDefaults: false,
		})
		if inspErr != nil {
			// If service disappeared during the window, skip.
			if client.IsErrNotFound(inspErr) {
				continue
			}

			return fmt.Errorf("service inspect %s: %w", serviceID, inspErr)
		}

		metadata, ok := getServiceMetadata(serviceID)
		if !ok {
			// Should be rare; skip with a warning.
			logrus.WithField("service_id", serviceID).
				Warn("metadata missing during global recompute")

			continue
		}

		eligible := countEligibleNodesForServiceFromNodes(nodes, &svc)
		setDesiredReplicasGauge(metadata, float64(eligible))
	}

	return nil
}
