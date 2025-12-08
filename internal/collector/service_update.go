package collector

import (
	"maps"
	"sync"

	"github.com/docker/docker/api/types/swarm"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
	"github.com/prometheus/client_golang/prometheus"
)

// Known service update state strings we’ll expose.
const (
	updateStateUpdating          = "updating"
	updateStateCompleted         = "completed"
	updateStatePaused            = "paused"
	updateStateRollbackStarted   = "rollback_started"
	updateStateRollbackCompleted = "rollback_completed"
)

// knownStates is the exhaustive list of update/rollback states we export.
var knownStates = []string{
	updateStateUpdating,
	updateStateCompleted,
	updateStatePaused,
	updateStateRollbackStarted,
	updateStateRollbackCompleted,
}

// serviceUpdateStateGauge is the “info-style” gauge (one label state=… set to 1, the rest 0).
var serviceUpdateStateGauge *prometheus.GaugeVec

// serviceUpdateStartedTimestamp and serviceUpdateCompletedTimestamp are unix timestamp gauges.
var (
	serviceUpdateStartedTimestamp   *prometheus.GaugeVec
	serviceUpdateCompletedTimestamp *prometheus.GaugeVec
)

var serviceUpdateMetricsInitWarnOnce sync.Once

// ConfigureServiceUpdateMetrics registers the update-state and timestamp metrics.
func ConfigureServiceUpdateMetrics() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
		"display_name",
	}, getSanitizedCustomLabelNames()...)

	// info-style: one of these will be 1, the others 0 per service
	stateLabels := make([]string, 0, len(baseLabels)+1)
	stateLabels = append(stateLabels, baseLabels...)
	stateLabels = append(stateLabels, "state")

	serviceUpdateStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "update_state_info",
		Help:        "Service update/rollback state indicator (1 for the current state, 0 otherwise).",
		ConstLabels: nil,
	}, stateLabels)
	prometheus.MustRegister(serviceUpdateStateGauge)

	serviceUpdateStartedTimestamp = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "update_started_timestamp_seconds",
		Help:        "Unix timestamp (seconds) when the current/last service update started (0 if unknown).",
		ConstLabels: nil,
	}, baseLabels)
	prometheus.MustRegister(serviceUpdateStartedTimestamp)

	serviceUpdateCompletedTimestamp = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusServiceSubsystem,
		Name:        "update_completed_timestamp_seconds",
		Help:        "Unix timestamp (seconds) when the current/last service update completed (0 if unknown).",
		ConstLabels: nil,
	}, baseLabels)
	prometheus.MustRegister(serviceUpdateCompletedTimestamp)
}

// updateStateFromSwarm maps swarm.UpdateState to our label strings.
func updateStateFromSwarm(state swarm.UpdateState) string {
	switch state {
	case swarm.UpdateStateUpdating:
		return updateStateUpdating
	case swarm.UpdateStateCompleted:
		return updateStateCompleted
	case swarm.UpdateStatePaused, swarm.UpdateStateRollbackPaused:
		return updateStatePaused
	case swarm.UpdateStateRollbackStarted:
		return updateStateRollbackStarted
	case swarm.UpdateStateRollbackCompleted:
		return updateStateRollbackCompleted
	default:
		// Treat unknowns as paused (neutral / non-advancing).
		return updateStatePaused
	}
}

// labelsForService builds the sanitized label set for a service from cached metadata.
func labelsForService(metadata serviceMetadata) prometheus.Labels {
	baseLabels := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
		"display_name": displayName(metadata.stack, metadata.service),
	}
	for key, value := range metadata.customLabels {
		// Warn once if a value looks high-cardinality.
		labelutil.MaybeWarnHighCardinality(key, value)
		baseLabels[key] = value
	}

	return labelutil.SanitizeMetricLabels(baseLabels)
}

// UpdateServiceUpdateMetricsForService emits the info-style state and timestamps for one service.
func UpdateServiceUpdateMetricsForService(service *swarm.Service, metadata serviceMetadata) {
	if serviceUpdateStateGauge == nil || serviceUpdateStartedTimestamp == nil ||
		serviceUpdateCompletedTimestamp == nil {
		serviceUpdateMetricsInitWarnOnce.Do(func() {
			logger.L().Error("service update metrics not configured before use; skipping emission")
		})

		return
	}

	baseLabels := labelsForService(metadata)

	// Update state info gauge: exhaustive emission across known states.
	// Determine the “current” label value (or completed default; paused if unknown).
	currentState := updateStateCompleted
	if service.UpdateStatus != nil {
		currentState = updateStateFromSwarm(service.UpdateStatus.State)
	}

	for index := range knownStates {
		state := knownStates[index]
		labelsWithState := cloneLabelsWithState(baseLabels, state)

		value := 0.0
		if state == currentState {
			value = 1.0
		}

		serviceUpdateStateGauge.With(labelsWithState).Set(value)
	}

	// Timestamps: 0 if nil.
	var (
		startedSeconds   float64
		completedSeconds float64
	)

	if service.UpdateStatus != nil {
		// NOTE: UpdateStatus.StartedAt / CompletedAt are *time.Time in current docker client.
		if service.UpdateStatus.StartedAt != nil {
			startedSeconds = float64(service.UpdateStatus.StartedAt.Unix())
		}

		if service.UpdateStatus.CompletedAt != nil {
			completedSeconds = float64(service.UpdateStatus.CompletedAt.Unix())
		}
	}

	serviceUpdateStartedTimestamp.With(baseLabels).Set(startedSeconds)
	serviceUpdateCompletedTimestamp.With(baseLabels).Set(completedSeconds)
}

// ClearServiceUpdateMetrics deletes all series for a removed service.
func ClearServiceUpdateMetrics(metadata serviceMetadata) {
	if serviceUpdateStateGauge == nil || serviceUpdateStartedTimestamp == nil ||
		serviceUpdateCompletedTimestamp == nil {
		serviceUpdateMetricsInitWarnOnce.Do(func() {
			logger.L().Error("service update metrics not configured before use; skipping clear")
		})

		return
	}

	baseLabels := labelsForService(metadata)

	// Delete state series (one per known state)
	for index := range knownStates {
		state := knownStates[index]
		labelsWithState := cloneLabelsWithState(baseLabels, state)
		_ = serviceUpdateStateGauge.Delete(labelsWithState)
	}

	// Delete timestamp series
	_ = serviceUpdateStartedTimestamp.Delete(baseLabels)
	_ = serviceUpdateCompletedTimestamp.Delete(baseLabels)
}

// cloneLabelsWithState returns a copy of baseLabels with the "state" label set.
func cloneLabelsWithState(baseLabels prometheus.Labels, state string) prometheus.Labels {
	labels := make(prometheus.Labels, len(baseLabels)+1)
	maps.Copy(labels, baseLabels)

	labels["state"] = state

	return labels
}
