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
// this is the configured replica count, and for replicated jobs the total number of
// completions. For global services and global jobs, it approximates the number of
// eligible nodes by evaluating placement constraints and node status.

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"

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
// For jobs and RestartPolicy.Condition=="none" services it is always 0.
var schedulableReplicasGauge *prometheus.GaugeVec

// Backoff for event-stream reconnects, node refreshes and resyncs.
const (
	backoffInitialDelay = 500 * time.Millisecond // first retry delay
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
		Namespace: prometheusNamespace,
		Subsystem: prometheusServiceSubsystem,
		Name:      "desired_replicas",
		Help: "Number of desired replicas for a Swarm service (replicated: configured replicas; " +
			"replicated-job: total completions; global and global-job: eligible nodes).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)

	schedulableReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusServiceSubsystem,
		Name:      "schedulable_replicas",
		Help: "Number of replicas that can currently be scheduled given node availability and placement constraints " +
			"(replicated: min(configured, eligible_nodes); global: eligible nodes; " +
			"jobs and restart-condition none services: 0).",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(schedulableReplicasGauge)
}

// waitBackoff waits for delay, or returns parentContext.Err() as soon as parentContext is done.
func waitBackoff(parentContext context.Context, delay time.Duration) error {
	timer := time.NewTimer(delay)
	defer timer.Stop()

	select {
	case <-parentContext.Done():
		return fmt.Errorf("backoff of %s interrupted: %w", delay, parentContext.Err())
	case <-timer.C:
		return nil
	}
}

// nextBackoff returns the delay that follows delay: doubled, capped at backoffMaxDelay.
func nextBackoff(delay time.Duration) time.Duration {
	return min(time.Duration(int64(delay)*int64(backoffMultiplier)), backoffMaxDelay)
}

// listServices lists every service, bounded by dockerRequestTimeout.
func listServices(parentContext context.Context, dockerClient DockerAPI) ([]swarm.Service, error) {
	callContext, cancel := withDockerTimeout(parentContext)
	defer cancel()

	listResult, listErr := dockerClient.ServiceList(callContext, client.ServiceListOptions{
		Filters: nil,
		Status:  false,
	})
	if listErr != nil {
		return nil, fmt.Errorf("service list: %w", listErr)
	}

	return listResult.Items, nil
}

// listNodes lists every node, bounded by dockerRequestTimeout.
func listNodes(parentContext context.Context, dockerClient DockerAPI) ([]swarm.Node, error) {
	callContext, cancel := withDockerTimeout(parentContext)
	defer cancel()

	listResult, listErr := dockerClient.NodeList(callContext, client.NodeListOptions{Filters: nil})
	if listErr != nil {
		return nil, fmt.Errorf("node list: %w", listErr)
	}

	return listResult.Items, nil
}

// inspectService inspects one service, bounded by dockerRequestTimeout.
func inspectService(
	parentContext context.Context,
	dockerClient DockerAPI,
	serviceID string,
) (swarm.Service, error) {
	callContext, cancel := withDockerTimeout(parentContext)
	defer cancel()

	inspectResult, inspectErr := dockerClient.ServiceInspect(
		callContext,
		serviceID,
		client.ServiceInspectOptions{InsertDefaults: false},
	)
	if inspectErr != nil {
		return swarm.Service{}, fmt.Errorf("service inspect %s: %w", serviceID, inspectErr)
	}

	return inspectResult.Service, nil
}

// ListenSwarmEvents follows the Docker event stream for service and node changes and hands every
// event to reconciler, which only marks what changed dirty, so the stream is always drained. It
// reconnects with capped exponential backoff. The first connection starts eventsSinceMargin
// before anchor, which the caller captures before the reconciler's first resync lists anything,
// so no change made while that resync runs is missed; a reconnect resumes eventsSinceMargin
// before the last event seen, and requests a resync once the new stream is open, since the daemon
// may no longer hold every event the listener missed (a restart, a trimmed history). It only
// returns once parentContext is done, and the error it returns always wraps parentContext.Err();
// it never returns nil.
func ListenSwarmEvents(
	parentContext context.Context,
	dockerClient DockerAPI,
	reconciler *Reconciler,
	anchor time.Time,
) error {
	filterArgs := make(client.Filters).Add("type", "service", "node")

	// Exponential backoff for reconnects.
	backoffDelay := backoffInitialDelay

	// Track where to resume from on reconnects.
	reconnectSince := anchor.Add(-eventsSinceMargin)

	for reconnect := false; ; reconnect = true {
		select {
		case <-parentContext.Done():
			return fmt.Errorf("event listener stopping: %w", parentContext.Err())
		default:
		}

		lastSeenEventTime, runErr := followEventStream(
			parentContext,
			dockerClient,
			reconciler,
			filterArgs,
			reconnectSince,
			reconnect,
		)

		// Reset backoff after a healthy stream that saw at least one event, and resume from it.
		if !lastSeenEventTime.IsZero() {
			backoffDelay = backoffInitialDelay
			reconnectSince = lastSeenEventTime.Add(-eventsSinceMargin)
		}

		// followEventStream always returns an error. When it ended because we are shutting down,
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
		waitErr := waitBackoff(parentContext, backoffDelay)
		if waitErr != nil {
			return fmt.Errorf("event listener canceled during backoff: %w", waitErr)
		}

		backoffDelay = nextBackoff(backoffDelay)
	}
}

// followEventStream opens one event-stream connection and dispatches its events until it ends.
// The connection gets its own context, with no deadline: the stream is long-lived, so the
// request deadline of the other calls would cut it. It is canceled when the connection ends,
// which releases the connection before a reconnect, and with parentContext on shutdown. On a
// reconnect it requests a resync after opening the stream, so the resync and the replay from
// since overlap and together cover whatever happened while the listener was disconnected.
func followEventStream(
	parentContext context.Context,
	dockerClient DockerAPI,
	reconciler *Reconciler,
	filterArgs client.Filters,
	since time.Time,
	reconnect bool,
) (time.Time, error) {
	streamContext, cancelStream := context.WithCancel(parentContext)
	defer cancelStream()

	eventsResult := dockerClient.Events(streamContext, client.EventsListOptions{
		// RFC 3339 with nanoseconds: the client turns it into "<seconds>.<nanoseconds>", so the
		// margin is not rounded away.
		Since:   since.UTC().Format(time.RFC3339Nano),
		Filters: filterArgs,
		Until:   "",
	})

	// Mark event stream connected for health.
	MarkEventsConnected(time.Now())

	logger.L().Info("event stream connected", "since", since.Format(time.RFC3339Nano))

	if reconnect {
		reconciler.requestResync(time.Now(), "event stream reconnected")
	}

	return dispatchEvents(parentContext, reconciler, eventsResult.Messages, eventsResult.Err)
}

// dispatchEvents hands every event to reconciler until the stream ends or the context is
// canceled. It returns the timestamp of the last event seen, which is used to resume the stream
// on reconnect without missing changes.
func dispatchEvents(
	parentContext context.Context,
	reconciler *Reconciler,
	eventChannel <-chan events.Message,
	errorChannel <-chan error,
) (time.Time, error) {
	var lastSeenEventTime time.Time

	for {
		select {
		case <-parentContext.Done():
			return lastSeenEventTime, fmt.Errorf(
				"event pump context canceled: %w",
				parentContext.Err(),
			)

		case streamErr := <-errorChannel:
			// A canceled stream can end with a closed-body error instead of context.Canceled:
			// report the cancellation, so shutdown is not taken for a stream failure.
			if parentContext.Err() != nil {
				return lastSeenEventTime, fmt.Errorf(
					"event pump context canceled: %w",
					parentContext.Err(),
				)
			}

			// Stream error—trigger reconnect at the caller.
			return lastSeenEventTime, fmt.Errorf("events stream error: %w", streamErr)

		case eventMessage, ok := <-eventChannel:
			if !ok {
				if parentContext.Err() != nil {
					return lastSeenEventTime, fmt.Errorf(
						"event pump context canceled: %w",
						parentContext.Err(),
					)
				}

				// Channel closed by Docker client—treat as EOF and reconnect.
				return lastSeenEventTime, fmt.Errorf(
					"events stream closed: %w",
					ErrEventsStreamClosed,
				)
			}

			// Update last seen event time from Docker's event timestamps.
			// Prefer TimeNano when present; fall back to Time (seconds).
			if eventMessage.TimeNano > 0 {
				lastSeenEventTime = time.Unix(0, eventMessage.TimeNano)
			} else if eventMessage.Time > 0 {
				lastSeenEventTime = time.Unix(eventMessage.Time, 0)
			}

			reconciler.enqueueEvent(&eventMessage)
		}
	}
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

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata *serviceMetadata, value float64) {
	desiredReplicasGauge.With(labelsForMetadata(metadata)).Set(value)
}

// setSchedulableReplicasGauge writes the schedulable replicas gauge value.
// Services with RestartPolicy.Condition=="none" (one-shot/cronjob) and job services
// (replicated-job, global-job) are forced to 0 because the scheduler is not expected
// to keep tasks running for them: a job's tasks run to completion and exit. Using the
// raw placement-eligibility value would produce constant false-positive alerts of the
// form running_replicas < schedulable_replicas.
func setSchedulableReplicasGauge(metadata *serviceMetadata, value float64) {
	if metadata.restartConditionNone || isJobMode(metadata.serviceMode) {
		value = 0
	}

	schedulableReplicasGauge.With(labelsForMetadata(metadata)).Set(value)
}

//
// ---- Helpers for global desired replicas accuracy ----
//

// countEligibleNodes returns how many nodes of a snapshot a task with placement could be placed on.
func countEligibleNodes(nodes []swarm.Node, placement *swarm.Placement) int {
	eligibleCount := 0

	for index := range nodes {
		if nodeEligible(&nodes[index], placement) {
			eligibleCount++
		}
	}

	return eligibleCount
}

// nodeEligible reports whether a task with the given placement could be placed on node: the
// node is schedulable, matches one of the required platforms (when any), and meets every
// placement constraint. A nil placement only requires a schedulable node.
func nodeEligible(node *swarm.Node, placement *swarm.Placement) bool {
	if !isNodeSchedulable(node) {
		return false
	}

	if placement == nil {
		return true
	}

	if len(placement.Platforms) > 0 && !platformMatches(node, placement.Platforms) {
		return false
	}

	return constraintsMatch(node, placement.Constraints)
}

// placementFromMetadata rebuilds the placement cached in metadata, for evaluating eligibility
// without the service spec. It returns nil when the service has neither constraints nor
// platforms.
func placementFromMetadata(metadata *serviceMetadata) *swarm.Placement {
	if len(metadata.constraints) == 0 && len(metadata.platforms) == 0 {
		return nil
	}

	return &swarm.Placement{
		Constraints: metadata.constraints,
		Platforms:   metadata.platforms,
	}
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
