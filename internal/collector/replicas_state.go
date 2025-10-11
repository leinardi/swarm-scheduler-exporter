package collector

// replicas_state exposes the gauge "swarm_service_replicas_state", which tracks
// the number of tasks per service per state (new, running, failed, etc.).
// This implementation counts only the "latest" task per (service,slot) for
// replicated services and per (service,nodeID) for global services, so that
// historical tasks from previous rollouts do not inflate counts.

import (
	"context"
	"fmt"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
)

// Capacity hint for the per-service state map.
const defaultStatesCapacity = 16

// knownTaskStates enumerates all Swarm task states we expose.
// We iterate this list during emission to ensure exhaustive output with zeros.
var knownTaskStates = []string{
	string(swarm.TaskStateNew),
	string(swarm.TaskStateAllocated),
	string(swarm.TaskStatePending),
	string(swarm.TaskStateAssigned),
	string(swarm.TaskStateAccepted),
	string(swarm.TaskStatePreparing),
	string(swarm.TaskStateReady),
	string(swarm.TaskStateStarting),
	string(swarm.TaskStateRunning),
	string(swarm.TaskStateComplete),
	string(swarm.TaskStateShutdown),
	string(swarm.TaskStateFailed),
	string(swarm.TaskStateRejected),
	string(swarm.TaskStateRemove),
	string(swarm.TaskStateOrphaned),
}

// replicasStateGauge is the gauge vector exported at /metrics.
var replicasStateGauge *prometheus.GaugeVec

// ConfigureReplicasStateGauge registers the "swarm_service_replicas_state" gauge
// with base labels (stack, service, service_mode, state) plus any custom labels.
func ConfigureReplicasStateGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
		"state",
	}, getSanitizedCustomLabelNames()...)

	replicasStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "",
		Subsystem:   "",
		Name:        "swarm_service_replicas_state",
		Help:        "State of service replicas",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(replicasStateGauge)
}

// taskCounter keeps a set of counters per Swarm task state for a given service.
type taskCounter struct {
	states map[string]float64
	labels prometheus.Labels
}

// inc increments the counter for a particular Swarm task state.
func (tctr taskCounter) inc(state string) {
	tctr.states[state]++
}

// serviceCounter organizes taskCounters keyed by ServiceID (unique, avoids
// collisions between stacks/services that share the same visible name).
type serviceCounter map[string]taskCounter

// get returns the taskCounter for ServiceID, creating it if necessary.
// labels must already contain stack/service/service_mode (+ custom labels).
func (sctr serviceCounter) get(serviceID string, labels prometheus.Labels) taskCounter {
	if _, ok := sctr[serviceID]; !ok {
		sctr[serviceID] = newTaskCounter(labels)
	}

	return sctr[serviceID]
}

// newTaskCounter initializes an empty state map; we only record states that are present
// during aggregation. Exhaustive emission (including zeros) is handled at publish-time.
func newTaskCounter(labels map[string]string) taskCounter {
	return taskCounter{
		labels: labels,
		states: make(map[string]float64, defaultStatesCapacity),
	}
}

// latestKey identifies a deduplication group:
// - replicated services → (serviceID, slot)
// - global services     → (serviceID, nodeID).
type latestKey struct {
	serviceID string
	slot      int    // for replicated; 0 for global
	nodeID    string // for global; empty for replicated
}

// newerThan returns true if candidate is strictly newer than current.
// First compare Status.Timestamp (if both non-zero), else fall back to Version.Index.
func newerThan(candidate, current *swarm.Task) bool {
	candidateTS := candidate.Status.Timestamp
	currentTS := current.Status.Timestamp

	// Prefer Status.Timestamp when present on both
	if !candidateTS.IsZero() && !currentTS.IsZero() {
		return candidateTS.After(currentTS)
	}

	// Fallback to Version.Index (monotonic increasing)
	return candidate.Version.Index > current.Version.Index
}

// PollReplicasState lists tasks and aggregates them by state per service,
// counting only the latest task per (service, slot) for replicated services
// and per (service, nodeID) for global services.
func PollReplicasState(ctx context.Context, cli *client.Client) (serviceCounter, error) {
	tasks, err := cli.TaskList(ctx, types.TaskListOptions{
		Filters: filters.Args{},
	})
	if err != nil {
		return serviceCounter{}, fmt.Errorf("task list: %w", err)
	}

	// Step 1: choose the latest task per dedupe key.
	latestByKey := make(map[latestKey]*swarm.Task)

	for i := range tasks { // iterate by index to avoid copying
		task := &tasks[i]

		// Ensure we have metadata cached (and skip tasks for deleted services).
		_, labelErr := getServiceLabels(ctx, cli, task)
		if client.IsErrNotFound(labelErr) {
			continue
		} else if labelErr != nil {
			return serviceCounter{}, fmt.Errorf("labels for service %s: %w", task.ServiceID, labelErr)
		}

		mode, ok := getServiceModeCached(task.ServiceID)
		if !ok {
			// Should not happen because getServiceLabels() above populates the cache,
			// but if it does, skip this task defensively.
			continue
		}

		var key latestKey

		if mode == "replicated" {
			key = latestKey{
				serviceID: task.ServiceID,
				slot:      task.Slot,
				nodeID:    "",
			}
		} else {
			// global services do not use slots; use NodeID instead
			key = latestKey{
				serviceID: task.ServiceID,
				slot:      0,
				nodeID:    task.NodeID,
			}
		}

		if previousTask, exists := latestByKey[key]; !exists || newerThan(task, previousTask) {
			latestByKey[key] = task
		}
	}

	// Step 2: aggregate chosen tasks per serviceID into states.
	replicas := make(serviceCounter)

	for key, task := range latestByKey {
		labels, labelErr := getServiceLabels(ctx, cli, task)
		if client.IsErrNotFound(labelErr) {
			// Service disappeared between selection and labeling; ignore.
			continue
		} else if labelErr != nil {
			return serviceCounter{}, fmt.Errorf("labels for service %s: %w", task.ServiceID, labelErr)
		}

		// Ensure label keys are Prometheus-safe (values are passed through).
		labels = labelutil.SanitizeMetricLabels(labels)

		counter := replicas.get(key.serviceID, labels)
		counter.inc(string(task.Status.State))
		replicas[key.serviceID] = counter
	}

	return replicas, nil
}

// UpdateReplicasStateGauge writes the aggregated state counters into the
// "swarm_service_replicas_state" gauge. It resets the vector first so series
// for services that disappeared are removed. For each service present in the
// current snapshot, it emits ALL known states, setting 0 where absent.
func UpdateReplicasStateGauge(counterByService serviceCounter) {
	// Drop all previous label sets for this metric vector.
	replicasStateGauge.Reset()

	for _, taskCtr := range counterByService {
		baseLabels := labelutil.SanitizeMetricLabels(taskCtr.labels)

		for _, state := range knownTaskStates {
			labels := prometheus.Labels{}
			for k, v := range baseLabels {
				labels[k] = v
			}

			labels["state"] = state

			value := taskCtr.states[state] // zero if missing
			replicasStateGauge.With(labels).Set(value)
		}
	}
}

// getServiceLabels returns label values for a task's parent service,
// populating the local metadata cache if necessary.
func getServiceLabels(
	ctx context.Context,
	cli *client.Client,
	task *swarm.Task,
) (prometheus.Labels, error) {
	serviceID := task.ServiceID

	// Fast path: metadata present
	if metadata, ok := getServiceMetadata(serviceID); ok {
		labelSet := prometheus.Labels{
			"stack":        metadata.stack,
			"service":      metadata.service,
			"service_mode": metadata.serviceMode,
		}
		for key, value := range metadata.customLabels {
			labelSet[key] = value
		}

		return labelSet, nil
	}

	// Slow path: inspect and cache
	service, _, err := cli.ServiceInspectWithRaw(ctx, serviceID, types.ServiceInspectOptions{
		InsertDefaults: false,
	})
	if err != nil {
		return map[string]string{}, fmt.Errorf("service inspect %s: %w", serviceID, err)
	}

	metadata := buildMetadata(&service)
	setServiceMetadata(serviceID, metadata)

	labelSet := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
	}
	for key, value := range metadata.customLabels {
		labelSet[key] = value
		labelutil.MaybeWarnHighCardinality(key, value)
	}

	return labelSet, nil
}
