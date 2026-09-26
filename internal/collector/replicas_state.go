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
// replicated services and replicated jobs, and per (service,nodeID) for global
// services and global jobs, so that historical tasks from previous rollouts do not
// inflate counts. For jobs, only tasks of the current job iteration are counted. The
// current task is the one Swarm still wants, preferring a running one, then the newest
// (see preferredTask).

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

// replicasStateCollector exposes the three families computed from one poll: replicas_state,
// running_replicas and at_desired. They share one snapshot, so a scrape never pairs the
// running_replicas of one poll with the at_desired of another.
var replicasStateCollector *replicasStateSnapshot

// replicasStateSnapshot is the snapshot collector of the replicas-state families, with their
// descriptors and the label names each was built with.
type replicasStateSnapshot struct {
	*snapshotCollector

	// stateDesc is swarm_task_replicas_state: tasks per service per task state.
	stateDesc *prometheus.Desc
	// runningDesc is swarm_service_running_replicas: running tasks per service.
	runningDesc *prometheus.Desc
	// atDesiredDesc is swarm_service_at_desired: 1 if running == desired replicas (for jobs: if
	// the job finished), else 0.
	atDesiredDesc *prometheus.Desc

	stateLabelNames   []string
	serviceLabelNames []string
}

var atDesiredLogState sync.Map // map[string]string (serviceID -> "running|desired")

// taskCounter keeps a set of counters per Swarm task state for a given service.
type taskCounter struct {
	states map[string]float64
	labels prometheus.Labels

	// wanted counts the counted tasks Swarm still wants (not retiredTask), and wantedComplete
	// those of them that are complete. A job is finished when they are equal; retired tasks
	// still show in states but take no part in that decision.
	wanted         float64
	wantedComplete float64

	// eligibleNodesCovered is set for global jobs only: every node that is eligible now has a
	// wanted, completed task of the current iteration.
	eligibleNodesCovered bool
}

// serviceCounter organizes taskCounters keyed by ServiceID (unique, avoids
// collisions between stacks/services that share the same visible name).
type serviceCounter map[string]taskCounter

// latestKey identifies a deduplication group:
// - replicated services and replicated jobs → (serviceID, slot)
// - global services and global jobs         → (serviceID, nodeID).
//
// Swarm gives each task of a replicated job its own slot in [0, TotalCompletions-1], and each
// task of a global job its own node, so the same keys hold for jobs.
type latestKey struct {
	serviceID string
	slot      int    // for replicated modes; 0 for global modes
	nodeID    string // for global modes; empty for replicated modes
}

// ConfigureReplicasStateGauge registers the "swarm_task_replicas_state",
// "swarm_service_running_replicas" and "swarm_service_at_desired" gauges with base labels
// (stack, service, service_mode, display_name; plus state for replicas_state) and any custom
// labels.
func ConfigureReplicasStateGauge() {
	replicasStateCollector = newReplicasStateSnapshot(getSanitizedCustomLabelNames())
	prometheus.MustRegister(replicasStateCollector)
}

// newReplicasStateSnapshot builds the descriptors of the replicas-state families, with
// customLabelNames appended after the base labels, and a collector with nothing published.
func newReplicasStateSnapshot(customLabelNames []string) *replicasStateSnapshot {
	stateLabelNames := labelutil.SanitizeLabelNames(append([]string{
		labelStack,
		labelService,
		labelServiceMode,
		labelDisplayName,
		labelState,
	}, customLabelNames...))

	serviceLabelNames := labelutil.SanitizeLabelNames(append([]string{
		labelStack,
		labelService,
		labelServiceMode,
		labelDisplayName,
	}, customLabelNames...))

	stateDesc := prometheus.NewDesc(
		prometheus.BuildFQName(prometheusNamespace, prometheusTaskSubsystem, "replicas_state"),
		"Number of tasks per Swarm service segmented by task state "+
			"(current task per slot for replicated services and replicated jobs or node for global services "+
			"and global jobs; jobs count only their current iteration).",
		stateLabelNames,
		nil,
	)

	runningDesc := prometheus.NewDesc(
		prometheus.BuildFQName(prometheusNamespace, prometheusServiceSubsystem, "running_replicas"),
		"Current number of running tasks per Swarm service "+
			"(current task per slot for replicated services and replicated jobs or node for global services "+
			"and global jobs; jobs count only their current iteration).",
		serviceLabelNames,
		nil,
	)

	atDesiredDesc := prometheus.NewDesc(
		prometheus.BuildFQName(prometheusNamespace, prometheusServiceSubsystem, "at_desired"),
		"Service is at desired replicas (1) or not (0). "+
			"Jobs are at desired once finished: every task of the current iteration completed.",
		serviceLabelNames,
		nil,
	)

	return &replicasStateSnapshot{
		snapshotCollector: newSnapshotCollector(stateDesc, runningDesc, atDesiredDesc),
		stateDesc:         stateDesc,
		runningDesc:       runningDesc,
		atDesiredDesc:     atDesiredDesc,
		stateLabelNames:   stateLabelNames,
		serviceLabelNames: serviceLabelNames,
	}
}

// PollReplicasState lists tasks and aggregates them by state per service,
// counting only the current task per (service, slot) for replicated services and replicated jobs
// and per (service, nodeID) for global services and global jobs, as chosen by preferredTask.
// Job tasks from an older job iteration are skipped.
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

		dedupeKey, counted := dedupeKeyForTask(task)
		if !counted {
			continue
		}

		if previousTask, exists := latestByKey[dedupeKey]; !exists ||
			preferredTask(task, previousTask) {
			latestByKey[dedupeKey] = task
		}
	}

	// Step 2: aggregate chosen tasks per serviceID into states.
	replicasByService := make(serviceCounter)

	// completedNodesByService holds, per global job, the nodes with a wanted completed task. It
	// only lives for this poll.
	completedNodesByService := make(map[string]map[string]struct{})

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
		countWanted(&counter, task, completedNodesByService)
		replicasByService[key.serviceID] = counter
	}

	markEligibleNodesCovered(replicasByService, completedNodesByService)
	addServicesWithoutTasks(replicasByService, queriedServiceIDs)

	return replicasByService, nil
}

// dedupeKeyForTask returns the dedupe key task is counted under, and false when task is not
// counted at all: its service is not in the metadata cache, or it belongs to an earlier run of a
// job.
func dedupeKeyForTask(task *swarm.Task) (latestKey, bool) {
	metadata, found := getServiceMetadata(task.ServiceID)
	if !found {
		// Should not happen because getServiceLabels() populates the cache before this is
		// called, but if it does, skip this task defensively.
		return latestKey{}, false
	}

	// A job's earlier runs leave their tasks behind (Swarm marks them for removal); only the
	// current iteration describes the job now.
	if isJobMode(metadata.serviceMode) &&
		(task.JobIteration == nil || task.JobIteration.Index != metadata.jobIteration) {
		return latestKey{}, false
	}

	if metadata.serviceMode == serviceModeReplicated ||
		metadata.serviceMode == serviceModeReplicatedJob {
		return latestKey{
			serviceID: task.ServiceID,
			slot:      task.Slot,
			nodeID:    "",
		}, true
	}

	// global services and global jobs do not use slots; use NodeID instead
	return latestKey{
		serviceID: task.ServiceID,
		slot:      0,
		nodeID:    task.NodeID,
	}, true
}

// countWanted adds task to the wanted counts when Swarm still wants it, and records the node of
// a wanted completed global-job task in completedNodesByService.
func countWanted(
	counter *taskCounter,
	task *swarm.Task,
	completedNodesByService map[string]map[string]struct{},
) {
	if retiredTask(task) {
		return
	}

	counter.wanted++

	if task.Status.State != swarm.TaskStateComplete {
		return
	}

	counter.wantedComplete++

	if counter.labels[labelServiceMode] == serviceModeGlobalJob {
		addCompletedNode(completedNodesByService, task.ServiceID, task.NodeID)
	}
}

// addCompletedNode records that nodeID has a wanted completed task of global job serviceID.
func addCompletedNode(
	completedNodesByService map[string]map[string]struct{},
	serviceID, nodeID string,
) {
	completedNodes, ok := completedNodesByService[serviceID]
	if !ok {
		completedNodes = make(map[string]struct{})
		completedNodesByService[serviceID] = completedNodes
	}

	completedNodes[nodeID] = struct{}{}
}

// markEligibleNodesCovered sets eligibleNodesCovered on every global-job counter, checking the
// nodes eligible now, from one node snapshot, against the nodes where the job has a wanted
// completed task. Comparing node identities rather than counts matters: when node A has
// completed and then becomes ineligible while node B becomes eligible, A's completion must not
// stand in for B, whose task Swarm only creates on its next reconcile. The work is bounded by
// nodes times global jobs and reads caches only.
func markEligibleNodesCovered(
	replicasByService serviceCounter,
	completedNodesByService map[string]map[string]struct{},
) {
	var (
		nodes       []swarm.Node
		nodesLoaded bool
	)

	for serviceID, counter := range replicasByService {
		if counter.labels[labelServiceMode] != serviceModeGlobalJob {
			continue
		}

		metadata, found := getServiceMetadata(serviceID)
		if !found {
			// Removed while this poll was running; leave it uncovered.
			continue
		}

		// One snapshot per poll, taken only when there is a global job to check.
		if !nodesLoaded {
			nodes = getCachedNodes()
			nodesLoaded = true
		}

		counter.eligibleNodesCovered = eligibleNodesCovered(
			nodes,
			placementFromMetadata(&metadata),
			completedNodesByService[serviceID],
		)
		replicasByService[serviceID] = counter
	}
}

// eligibleNodesCovered reports whether every node of nodes that is eligible for placement is in
// completedNodes. An empty snapshot (nodes not seeded yet) reports false, so a global job is not
// taken for finished before the exporter knows the nodes.
func eligibleNodesCovered(
	nodes []swarm.Node,
	placement *swarm.Placement,
	completedNodes map[string]struct{},
) bool {
	if len(nodes) == 0 {
		return false
	}

	for index := range nodes {
		node := &nodes[index]
		if !nodeEligible(node, placement) {
			continue
		}

		if _, completed := completedNodes[node.ID]; !completed {
			return false
		}
	}

	return true
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

// UpdateReplicasStateGauge publishes the aggregated state counters as the replicas_state,
// running_replicas and at_desired families. The whole set is built first and replaces the
// previous one in a single swap, so services that disappeared are dropped without a scrape ever
// seeing the families empty or half rebuilt. If the build fails, the previous set stays.
func UpdateReplicasStateGauge(counterByService serviceCounter) {
	if replicasStateCollector == nil {
		return
	}

	metrics, buildErr := buildReplicasState(replicasStateCollector, counterByService).build()
	if buildErr != nil {
		logger.L().Error("build replicas state snapshot; keeping previous", "err", buildErr)

		return
	}

	replicasStateCollector.publish(metrics)
}

// buildReplicasState records the series of every service in counterByService, for all three
// families, without publishing them. For each service it emits ALL known states, setting 0
// where absent.
func buildReplicasState(
	families *replicasStateSnapshot,
	counterByService serviceCounter,
) *snapshotBuilder {
	builder := newSnapshotBuilder()

	for serviceID, taskCounterValue := range counterByService {
		baseLabels := labelutil.SanitizeMetricLabels(taskCounterValue.labels)

		// Emit exhaustive per-state series.
		for _, state := range knownTaskStates {
			labels := prometheus.Labels{}
			maps.Copy(labels, baseLabels)

			labels[labelState] = state

			value := taskCounterValue.states[state] // zero if missing
			builder.set(families.stateDesc, families.stateLabelNames, labels, value)
		}

		running := taskCounterValue.states[string(swarm.TaskStateRunning)]
		builder.set(families.runningDesc, families.serviceLabelNames, baseLabels, running)

		setAtDesiredForService(builder, families, serviceID, baseLabels, taskCounterValue)
	}

	return builder
}

func setAtDesiredForService(
	builder *snapshotBuilder,
	families *replicasStateSnapshot,
	serviceID string,
	baseLabels prometheus.Labels,
	taskCounterValue taskCounter,
) {
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

		builder.set(families.atDesiredDesc, families.serviceLabelNames, baseLabels, 0)

		return
	}

	mode := baseLabels[labelServiceMode]

	reached := running == desired
	if isJobMode(mode) {
		reached = jobAtDesired(mode, &taskCounterValue, desired)
	}

	atDesired := 0.0
	if reached {
		atDesired = 1.0
	} else {
		logAtDesiredMismatchOncePerChange(
			serviceID,
			baseLabels,
			running,
			desired,
			&taskCounterValue,
		)
	}

	builder.set(families.atDesiredDesc, families.serviceLabelNames, baseLabels, atDesired)
}

// jobAtDesired reports whether a job has finished, from the tasks of its current iteration.
// Only tasks Swarm still wants count: Swarm shuts down (DesiredState=Shutdown) a global job's
// running task when its node goes down or is drained (swarmkit jobs/global/reconciler.go), and
// the restart supervisor does the same to a failed task before replacing it, so a retired task
// left in the total would keep a finished job at 0 forever. A slot whose restarts are exhausted
// is left with a retired failed task and no replacement, so wantedComplete stays below the
// total completions and the replicated job stays at 0.
func jobAtDesired(mode string, counter *taskCounter, desired float64) bool {
	// Every wanted task is complete: none is still pending, running, or failed awaiting a
	// restart. Without it, in a global job a down node's completed task plus a new node's
	// unfinished task could add up to desired.
	allWantedComplete := counter.wantedComplete == counter.wanted

	if mode == serviceModeReplicatedJob {
		// desired (total completions) is at least 1, so this also requires a task.
		return allWantedComplete && counter.wantedComplete >= desired
	}

	// Global job. wanted > 0 rather than desired > 0: a global job with no eligible node never
	// runs and stays at 0, while a finished one whose last eligible node is later drained or
	// goes down keeps its completed tasks and still reports 1 with desired at 0. Covering every
	// eligible node by identity implies wantedComplete >= desired.
	return counter.wanted > 0 && allWantedComplete && counter.eligibleNodesCovered
}

func logAtDesiredMismatchOncePerChange(
	serviceID string,
	baseLabels prometheus.Labels,
	running, desired float64,
	counter *taskCounter,
) {
	key := fmt.Sprintf("%.0f|%.0f", running, desired)
	fields := []any{
		"service_id", serviceID,
		labelStack, baseLabels[labelStack],
		labelService, baseLabels[labelService],
		"mode", baseLabels[labelServiceMode],
		"running", running,
		"desired", desired,
	}

	if isJobMode(baseLabels[labelServiceMode]) {
		key += fmt.Sprintf(
			"|%.0f|%.0f|%t",
			counter.wanted,
			counter.wantedComplete,
			counter.eligibleNodesCovered,
		)
		fields = append(fields,
			"wanted", counter.wanted,
			"wanted_complete", counter.wantedComplete,
			"eligible_nodes_covered", counter.eligibleNodesCovered,
		)
	}

	prev, loaded := atDesiredLogState.LoadOrStore(serviceID, key)
	if loaded && prev == key {
		return // same situation as last time -> no log
	}

	logger.L().Debug("at_desired mismatch", fields...)
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
