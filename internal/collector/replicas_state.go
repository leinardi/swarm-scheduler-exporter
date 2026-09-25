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
// This implementation counts only the current task per (service,slot) for
// replicated services and per (service,nodeID) for global services, so that
// historical tasks from previous rollouts do not inflate counts. The current task
// is the one Swarm still wants, preferring a running one, then the newest (see
// preferredTask).

import (
	"context"
	"fmt"
	"maps"
	"sync"

	"github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus"

	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
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

// retiredDesiredStates are the desired states of a task Swarm no longer wants running. They
// mirror swarmkit's "DesiredState > Completed" check in its restart supervisor: a task in one
// of them has been shut down or replaced, even if its node still reports it as running (a down
// node's tasks keep their last status for up to 24h).
var retiredDesiredStates = map[swarm.TaskState]struct{}{
	swarm.TaskStateShutdown: {},
	swarm.TaskStateFailed:   {},
	swarm.TaskStateRejected: {},
	swarm.TaskStateRemove:   {},
	swarm.TaskStateOrphaned: {},
}

// replicasStateGauge is the gauge vector exported at /metrics.
var replicasStateGauge *prometheus.GaugeVec

// runningReplicasGauge exposes the current number of running tasks per service.
var runningReplicasGauge *prometheus.GaugeVec

// atDesiredGauge exposes 1 if running_replicas == desired_replicas, else 0.
var atDesiredGauge *prometheus.GaugeVec

var atDesiredLogState sync.Map // map[string]string (serviceID -> "running|desired")

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
		labelStack,
		labelService,
		labelServiceMode,
		labelDisplayName,
		labelState,
	}, getSanitizedCustomLabelNames()...)

	replicasStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusTaskSubsystem,
		Name:      "replicas_state",
		Help: "Number of tasks per Swarm service segmented by task state " +
			"(current task per slot for replicated services or node for global services).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(replicasStateGauge)

	// New: running replicas (no "state" label)
	runningBase := append([]string{
		labelStack,
		labelService,
		labelServiceMode,
		labelDisplayName,
	}, getSanitizedCustomLabelNames()...)
	runningReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusServiceSubsystem,
		Name:      "running_replicas",
		Help: "Current number of running tasks per Swarm service " +
			"(current task per slot for replicated services or node for global services).",
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
// counting only the current task per (service, slot) for replicated services
// and per (service, nodeID) for global services, as chosen by preferredTask.
func PollReplicasState(
	parentContext context.Context,
	dockerClient DockerAPI,
) (serviceCounter, error) {
	// Build a service-scoped filter to avoid pulling tasks from unrelated or removed services.
	serviceIDs := getAllServiceIDs()

	// A nil client.Filters sends no filter; Add on it would panic, so allocate before adding.
	var (
		taskFilters       client.Filters
		queriedServiceIDs []string
	)

	if len(serviceIDs) > 0 {
		limit := min(len(serviceIDs), maxServicesInTaskFilter)
		queriedServiceIDs = serviceIDs[:limit]
		taskFilters = make(client.Filters).Add("service", queriedServiceIDs...)
	}

	taskListResult, listErr := dockerClient.TaskList(parentContext, client.TaskListOptions{
		Filters: taskFilters,
	})
	if listErr != nil {
		return serviceCounter{}, fmt.Errorf("task list: %w", listErr)
	}

	tasks := taskListResult.Items

	// Step 1: choose the current task per dedupe key.
	latestByKey := make(map[latestKey]*swarm.Task)

	for index := range tasks { // iterate by index to avoid copying
		task := &tasks[index]

		// Ensure we have metadata cached (and skip tasks for deleted services).
		_, labelErr := getServiceLabels(parentContext, dockerClient, task)
		if errdefs.IsNotFound(labelErr) {
			continue
		} else if labelErr != nil {
			return serviceCounter{}, fmt.Errorf(
				"labels for service %s: %w",
				task.ServiceID,
				labelErr,
			)
		}

		mode, found := getServiceModeCached(task.ServiceID)
		if !found {
			// Should not happen because getServiceLabels() above populates the cache,
			// but if it does, skip this task defensively.
			continue
		}

		var dedupeKey latestKey
		if mode == serviceModeReplicated {
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
			preferredTask(task, previousTask) {
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
			return serviceCounter{}, fmt.Errorf(
				"labels for service %s: %w",
				task.ServiceID,
				labelErr,
			)
		}

		// Ensure label keys are Prometheus-safe (values are passed through).
		labels = labelutil.SanitizeMetricLabels(labels)

		counter := replicasByService.get(key.serviceID, labels)
		counter.inc(string(task.Status.State))
		replicasByService[key.serviceID] = counter
	}

	addServicesWithoutTasks(replicasByService, queriedServiceIDs)

	return replicasByService, nil
}

// addServicesWithoutTasks gives every queried service that has no tasks an empty counter, so
// it still emits running_replicas=0, zeroed task states and an at_desired series. Without it a
// service that was never scheduled (a global service no node is eligible for, a replicated
// service whose tasks cannot be created) has no series at all, and an "at_desired == 0" alert
// never fires for it. Only services that were in the TaskList filter are added: one beyond
// maxServicesInTaskFilter was not queried, so its task count is unknown, not zero.
func addServicesWithoutTasks(replicasByService serviceCounter, queriedServiceIDs []string) {
	for _, serviceID := range queriedServiceIDs {
		if _, exists := replicasByService[serviceID]; exists {
			continue
		}

		metadata, found := getServiceMetadata(serviceID)
		if !found {
			// Removed while this poll was running.
			continue
		}

		replicasByService[serviceID] = newTaskCounter(labelsForMetadata(&metadata))
	}
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

			labels[labelState] = state

			value := taskCounterValue.states[state] // zero if missing
			replicasStateGauge.With(labels).Set(value)
		}

		if runningReplicasGauge != nil {
			running := taskCounterValue.states[string(swarm.TaskStateRunning)]
			runningReplicasGauge.With(baseLabels).Set(running)
		}

		setAtDesiredForService(serviceID, baseLabels, taskCounterValue)
	}
}

func setAtDesiredForService(
	serviceID string,
	baseLabels prometheus.Labels,
	taskCounterValue taskCounter,
) {
	if atDesiredGauge == nil {
		return
	}

	running := taskCounterValue.states[string(swarm.TaskStateRunning)]

	desired, foundDesired := getServiceDesiredReplicas(serviceID)
	if !foundDesired {
		logger.L().Warn("desired replicas missing from cache",
			"service_id", serviceID,
			labelStack, baseLabels[labelStack],
			labelService, baseLabels[labelService],
			"mode", baseLabels[labelServiceMode],
			"running", running,
		)

		atDesiredGauge.With(baseLabels).Set(0)

		return
	}

	atDesired := 0.0
	if running == desired {
		atDesired = 1.0
	} else {
		logAtDesiredMismatchOncePerChange(serviceID, baseLabels, running, desired)
	}

	atDesiredGauge.With(baseLabels).Set(atDesired)
}

func logAtDesiredMismatchOncePerChange(
	serviceID string,
	baseLabels prometheus.Labels,
	running, desired float64,
) {
	key := fmt.Sprintf("%.0f|%.0f", running, desired)

	prev, loaded := atDesiredLogState.LoadOrStore(serviceID, key)
	if loaded && prev == key {
		return // same situation as last time -> no log
	}

	logger.L().Debug("at_desired mismatch",
		"service_id", serviceID,
		labelStack, baseLabels[labelStack],
		labelService, baseLabels[labelService],
		"mode", baseLabels[labelServiceMode],
		"running", running,
		"desired", desired,
	)
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

// preferredTask returns true if candidate should replace current as the task counted for a
// dedupe key. The first rule that tells them apart decides:
//  1. a task Swarm still wants beats a retired one: after a failed start-first update the
//     newer, failed task is retired while the older one keeps serving, and a stale task on a
//     down node is retired while its replacement starts;
//  2. a running task beats one that is not: during a start-first update the old task keeps
//     serving until its replacement is running;
//  3. the newer task wins (newerThan).
//
// Each rule is symmetric, so the choice does not depend on the order Docker lists tasks in.
func preferredTask(candidate, current *swarm.Task) bool {
	candidateRetired := retiredTask(candidate)
	if candidateRetired != retiredTask(current) {
		return !candidateRetired
	}

	candidateRunning := candidate.Status.State == swarm.TaskStateRunning
	if candidateRunning != (current.Status.State == swarm.TaskStateRunning) {
		return candidateRunning
	}

	return newerThan(candidate, current)
}

// retiredTask reports whether Swarm no longer wants task running (see retiredDesiredStates).
// An empty desired state counts as wanted.
func retiredTask(task *swarm.Task) bool {
	_, retired := retiredDesiredStates[task.DesiredState]

	return retired
}

// newerThan returns true if candidate is strictly newer than current.
// Prefer task CreatedAt (creation time) so the newest task attempt wins rather than the
// most recently updated status (which can be a late Shutdown).
// Fall back to Status.Timestamp, then Version.Index as a last resort.
func newerThan(candidate, current *swarm.Task) bool {
	// 1) Task creation time (stable across status updates).
	if candidate.CreatedAt.After(current.CreatedAt) {
		return true
	}

	if candidate.CreatedAt.Before(current.CreatedAt) {
		return false
	}

	// 2) Status timestamp (best-effort if creation time is missing/equal).
	candidateTimestamp := candidate.Status.Timestamp
	currentTimestamp := current.Status.Timestamp

	// Prefer Status.Timestamp when present on both
	if !candidateTimestamp.IsZero() && !currentTimestamp.IsZero() {
		return candidateTimestamp.After(currentTimestamp)
	}

	// 3) Fallback to Version.Index (monotonic increasing for a task object)
	return candidate.Version.Index > current.Version.Index
}

// getServiceLabels returns label values for a task's parent service,
// populating the local metadata cache if necessary.
func getServiceLabels(
	parentContext context.Context,
	dockerClient DockerAPI,
	task *swarm.Task,
) (prometheus.Labels, error) {
	serviceID := task.ServiceID

	// Fast path: metadata present
	if metadata, ok := getServiceMetadata(serviceID); ok {
		labelSet := prometheus.Labels{
			labelStack:       metadata.stack,
			labelService:     metadata.service,
			labelServiceMode: metadata.serviceMode,
			labelDisplayName: displayName(metadata.stack, metadata.service),
		}
		maps.Copy(labelSet, metadata.customLabels)

		return labelSet, nil
	}

	// Slow path: inspect and cache
	inspectResult, inspectErr := dockerClient.ServiceInspect(
		parentContext,
		serviceID,
		client.ServiceInspectOptions{
			InsertDefaults: false,
		},
	)
	if inspectErr != nil {
		return map[string]string{}, fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
	}

	service := inspectResult.Service

	metadata := buildMetadata(&service)
	setServiceMetadata(serviceID, &metadata)

	labelSet := prometheus.Labels{
		labelStack:       metadata.stack,
		labelService:     metadata.service,
		labelServiceMode: metadata.serviceMode,
		labelDisplayName: displayName(metadata.stack, metadata.service),
	}
	for key, value := range metadata.customLabels {
		labelSet[key] = value
		labelutil.MaybeWarnHighCardinality(key, value)
	}

	return labelSet, nil
}
