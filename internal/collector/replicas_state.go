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

// replicas_state exposes the gauge "swarm_service_replicas_state", which tracks
// the number of tasks per service per state (new, running, failed, etc.).
// This implementation counts only the "latest" task per (service,slot) for
// replicated services and per (service,nodeID) for global services, so that
// historical tasks from previous rollouts do not inflate counts.

import (
	"context"
	"fmt"
	"maps"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
)

const (
	// Capacity hint for the per-service state map.
	defaultStatesCapacity = 16

	// Limit for the number of service filters to attach to TaskList calls.
	// Prevents extremely large filter payloads; adjust as needed.
	maxServicesInTaskFilter = 10000
)

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

// runningReplicasGauge exposes the current number of running tasks per service.
var runningReplicasGauge *prometheus.GaugeVec

// atDesiredGauge exposes 1 if running_replicas == desired_replicas, else 0.
var atDesiredGauge *prometheus.GaugeVec

// taskCounter keeps a set of counters per Swarm task state for a given service.
type taskCounter struct {
	states map[string]float64
	labels prometheus.Labels
}

// serviceCounter organizes taskCounters keyed by ServiceID (unique, avoids
// collisions between stacks/services that share the same visible name).
type serviceCounter map[string]taskCounter

// latestKey identifies a deduplication group:
// - replicated services → (serviceID, slot)
// - global services     → (serviceID, nodeID).
type latestKey struct {
	serviceID string
	slot      int    // for replicated; 0 for global
	nodeID    string // for global; empty for replicated
}

// ConfigureReplicasStateGauge registers the "swarm_task_replicas_state" gauge
// with base labels (stack, service, service_mode, state) plus any custom labels.
func ConfigureReplicasStateGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
		"display_name",
		"state",
	}, getSanitizedCustomLabelNames()...)

	replicasStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusTaskSubsystem,
		Name:        "replicas_state",
		Help:        "Number of tasks per Swarm service segmented by task state (latest per slot).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(replicasStateGauge)

	// New: running replicas (no "state" label)
	runningBase := append([]string{
		"stack",
		"service",
		"service_mode",
		"display_name",
	}, getSanitizedCustomLabelNames()...)
	runningReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "running_replicas",
		Help:        "Current number of running tasks per Swarm service (latest per slot).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(runningBase))
	prometheus.MustRegister(runningReplicasGauge)

	// New: at_desired (0/1)
	atDesiredGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "at_desired",
		Help:        "Service is at desired replicas (1) or not (0).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(runningBase))
	prometheus.MustRegister(atDesiredGauge)
}

// PollReplicasState lists tasks and aggregates them by state per service,
// counting only the latest task per (service, slot) for replicated services
// and per (service, nodeID) for global services.
func PollReplicasState(
	parentContext context.Context,
	dockerClient *client.Client,
) (serviceCounter, error) {
	// Build a service-scoped filter to avoid pulling tasks from unrelated or removed services.
	serviceIDs := getAllServiceIDs()

	taskFilters := filters.NewArgs()

	if len(serviceIDs) > 0 {
		limit := min(len(serviceIDs), maxServicesInTaskFilter)

		for index := range limit {
			taskFilters.Add("service", serviceIDs[index])
		}
	}

	tasks, listErr := dockerClient.TaskList(parentContext, swarm.TaskListOptions{
		Filters: taskFilters,
	})
	if listErr != nil {
		return serviceCounter{}, fmt.Errorf("task list: %w", listErr)
	}

	// Step 1: choose the latest task per dedupe key.
	latestByKey := make(map[latestKey]*swarm.Task)

	for index := range tasks { // iterate by index to avoid copying
		task := &tasks[index]

		// Ensure we have metadata cached (and skip tasks for deleted services).
		_, labelErr := getServiceLabels(parentContext, dockerClient, task)
		if errdefs.IsNotFound(labelErr) {
			continue
		} else if labelErr != nil {
			return serviceCounter{}, fmt.Errorf("labels for service %s: %w", task.ServiceID, labelErr)
		}

		mode, found := getServiceModeCached(task.ServiceID)
		if !found {
			// Should not happen because getServiceLabels() above populates the cache,
			// but if it does, skip this task defensively.
			continue
		}

		var dedupeKey latestKey
		if mode == "replicated" {
			dedupeKey = latestKey{
				serviceID: task.ServiceID,
				slot:      task.Slot,
				nodeID:    "",
			}
		} else {
			// global services do not use slots; use NodeID instead
			dedupeKey = latestKey{
				serviceID: task.ServiceID,
				slot:      0,
				nodeID:    task.NodeID,
			}
		}

		if previousTask, exists := latestByKey[dedupeKey]; !exists ||
			newerThan(task, previousTask) {
			latestByKey[dedupeKey] = task
		}
	}

	// Step 2: aggregate chosen tasks per serviceID into states.
	replicasByService := make(serviceCounter)

	for key, task := range latestByKey {
		labels, labelErr := getServiceLabels(parentContext, dockerClient, task)
		if errdefs.IsNotFound(labelErr) {
			// Service disappeared between selection and labeling; ignore.
			continue
		} else if labelErr != nil {
			return serviceCounter{}, fmt.Errorf("labels for service %s: %w", task.ServiceID, labelErr)
		}

		// Ensure label keys are Prometheus-safe (values are passed through).
		labels = labelutil.SanitizeMetricLabels(labels)

		counter := replicasByService.get(key.serviceID, labels)
		counter.inc(string(task.Status.State))
		replicasByService[key.serviceID] = counter
	}

	return replicasByService, nil
}

// UpdateReplicasStateGauge writes the aggregated state counters into the
// "swarm_service_replicas_state" gauge. It resets the vector first so series
// for services that disappeared are removed. For each service present in the
// current snapshot, it emits ALL known states, setting 0 where absent.
func UpdateReplicasStateGauge(counterByService serviceCounter) {
	// Drop all previous label sets for these vectors.
	replicasStateGauge.Reset()

	if runningReplicasGauge != nil {
		runningReplicasGauge.Reset()
	}

	if atDesiredGauge != nil {
		atDesiredGauge.Reset()
	}

	for serviceID, taskCounterValue := range counterByService {
		baseLabels := labelutil.SanitizeMetricLabels(taskCounterValue.labels)

		// Emit exhaustive per-state series.
		for _, state := range knownTaskStates {
			labels := prometheus.Labels{}
			maps.Copy(labels, baseLabels)

			labels["state"] = state

			value := taskCounterValue.states[state] // zero if missing
			replicasStateGauge.With(labels).Set(value)
		}

		if runningReplicasGauge != nil {
			running := taskCounterValue.states[string(swarm.TaskStateRunning)]
			runningReplicasGauge.With(baseLabels).Set(running)
		}

		if atDesiredGauge != nil {
			desired, ok := getServiceDesiredReplicas(serviceID)
			// If unknown desired (should be rare), treat as not at desired.
			atDesired := 0.0

			if ok {
				// Both are floats but represent integer counts; direct equality is fine.
				running := taskCounterValue.states[string(swarm.TaskStateRunning)]
				if running == desired {
					atDesired = 1.0
				}
			}

			atDesiredGauge.With(baseLabels).Set(atDesired)
		}
	}
}

// inc increments the counter for a particular Swarm task state.
func (counter taskCounter) inc(state string) {
	counter.states[state]++
}

// get returns the taskCounter for ServiceID, creating it if necessary.
// labels must already contain stack/service/service_mode (+ custom labels).
func (byService serviceCounter) get(serviceID string, labels prometheus.Labels) taskCounter {
	if _, ok := byService[serviceID]; !ok {
		byService[serviceID] = newTaskCounter(labels)
	}

	return byService[serviceID]
}

// newTaskCounter initializes an empty state map; we only record states that are present
// during aggregation. Exhaustive emission (including zeros) is handled at publish-time.
func newTaskCounter(labels map[string]string) taskCounter {
	return taskCounter{
		labels: labels,
		states: make(map[string]float64, defaultStatesCapacity),
	}
}

// newerThan returns true if candidate is strictly newer than current.
// First compare Status.Timestamp (if both non-zero), else fall back to Version.Index.
func newerThan(candidate, current *swarm.Task) bool {
	candidateTimestamp := candidate.Status.Timestamp
	currentTimestamp := current.Status.Timestamp

	// Prefer Status.Timestamp when present on both
	if !candidateTimestamp.IsZero() && !currentTimestamp.IsZero() {
		return candidateTimestamp.After(currentTimestamp)
	}

	// Fallback to Version.Index (monotonic increasing)
	return candidate.Version.Index > current.Version.Index
}

// getServiceLabels returns label values for a task's parent service,
// populating the local metadata cache if necessary.
func getServiceLabels(
	parentContext context.Context,
	dockerClient *client.Client,
	task *swarm.Task,
) (prometheus.Labels, error) {
	serviceID := task.ServiceID

	// Fast path: metadata present
	if metadata, ok := getServiceMetadata(serviceID); ok {
		labelSet := prometheus.Labels{
			"stack":        metadata.stack,
			"service":      metadata.service,
			"service_mode": metadata.serviceMode,
			"display_name": displayName(metadata.stack, metadata.service),
		}
		maps.Copy(labelSet, metadata.customLabels)

		return labelSet, nil
	}

	// Slow path: inspect and cache
	service, _, inspectErr := dockerClient.ServiceInspectWithRaw(
		parentContext,
		serviceID,
		swarm.ServiceInspectOptions{
			InsertDefaults: false,
		},
	)
	if inspectErr != nil {
		return map[string]string{}, fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
	}

	metadata := buildMetadata(&service)
	setServiceMetadata(serviceID, metadata)

	labelSet := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
		"display_name": displayName(metadata.stack, metadata.service),
	}
	for key, value := range metadata.customLabels {
		labelSet[key] = value
		labelutil.MaybeWarnHighCardinality(key, value)
	}

	return labelSet, nil
}
