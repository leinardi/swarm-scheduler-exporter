package collector

import (
	"sync"

	"github.com/docker/docker/api/types/swarm"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/leinardi/swarm-tasks-exporter/internal/logger"
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

// serviceUpdateStateGauge is the “info-style” gauge (one label state=… set to 1, the rest 0).
var serviceUpdateStateGauge *prometheus.GaugeVec

// serviceUpdateStartedTs and serviceUpdateCompletedTs are unix timestamp gauges.
var (
	serviceUpdateStartedTs   *prometheus.GaugeVec
	serviceUpdateCompletedTs *prometheus.GaugeVec
)

var serviceUpdateMetricsInitWarnOnce sync.Once

// ConfigureServiceUpdateMetrics registers the update-state and timestamp metrics.
func ConfigureServiceUpdateMetrics() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
	}, getSanitizedCustomLabelNames()...)

	// info-style: one of these will be 1, the others 0 per service
	stateLabels := make([]string, 0, len(baseLabels)+1)
	stateLabels = append(stateLabels, baseLabels...)
	stateLabels = append(stateLabels, "state")

	serviceUpdateStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "swarm",
		Subsystem:   "service",
		Name:        "update_state_info",
		Help:        "Service update/rollback state indicator (1 for the current state, 0 otherwise).",
		ConstLabels: nil,
	}, stateLabels)
	prometheus.MustRegister(serviceUpdateStateGauge)

	serviceUpdateStartedTs = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "swarm",
		Subsystem:   "service",
		Name:        "update_started_timestamp_seconds",
		Help:        "Unix timestamp (seconds) when the current/last service update started (0 if unknown).",
		ConstLabels: nil,
	}, baseLabels)
	prometheus.MustRegister(serviceUpdateStartedTs)

	serviceUpdateCompletedTs = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "swarm",
		Subsystem:   "service",
		Name:        "update_completed_timestamp_seconds",
		Help:        "Unix timestamp (seconds) when the current/last service update completed (0 if unknown).",
		ConstLabels: nil,
	}, baseLabels)
	prometheus.MustRegister(serviceUpdateCompletedTs)
}

// updateStateFromSwarm maps swarm.UpdateState to our label strings.
func updateStateFromSwarm(s swarm.UpdateState) string {
	switch s {
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
	base := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
	}
	for key, value := range metadata.customLabels {
		// Warn once if a value looks high-cardinality.
		labelutil.MaybeWarnHighCardinality(key, value)
		base[key] = value
	}

	return labelutil.SanitizeMetricLabels(base)
}

// UpdateServiceUpdateMetricsForService emits the info-style state and timestamps for one service.
func UpdateServiceUpdateMetricsForService(svc *swarm.Service, metadata serviceMetadata) {
	if serviceUpdateStateGauge == nil || serviceUpdateStartedTs == nil ||
		serviceUpdateCompletedTs == nil {
		serviceUpdateMetricsInitWarnOnce.Do(func() {
			logger.L().Error("service update metrics not configured before use; skipping emission")
		})

		return
	}

	base := labelsForService(metadata)

	// Update state info gauge: exhaustive emission across known states.
	// Determine the “current” label value (or paused if nil/unknown).
	currentState := updateStateCompleted
	if svc.UpdateStatus != nil {
		currentState = updateStateFromSwarm(svc.UpdateStatus.State)
	}

	knownStates := []string{
		updateStateUpdating,
		updateStateCompleted,
		updateStatePaused,
		updateStateRollbackStarted,
		updateStateRollbackCompleted,
	}

	for i := range knownStates {
		state := knownStates[i]

		labels := make(prometheus.Labels, len(base)+1)
		for k, v := range base {
			labels[k] = v
		}

		labels["state"] = state

		val := 0.0
		if state == currentState {
			val = 1.0
		}

		serviceUpdateStateGauge.With(labels).Set(val)
	}

	// Timestamps: 0 if nil.
	var (
		startedSec   float64
		completedSec float64
	)

	if svc.UpdateStatus != nil {
		// NOTE: UpdateStatus.StartedAt / CompletedAt are *time.Time in current docker client.
		if svc.UpdateStatus.StartedAt != nil {
			startedSec = float64(svc.UpdateStatus.StartedAt.Unix())
		}

		if svc.UpdateStatus.CompletedAt != nil {
			completedSec = float64(svc.UpdateStatus.CompletedAt.Unix())
		}
	}

	serviceUpdateStartedTs.With(base).Set(startedSec)
	serviceUpdateCompletedTs.With(base).Set(completedSec)
}

// ClearServiceUpdateMetrics deletes all series for a removed service.
func ClearServiceUpdateMetrics(metadata serviceMetadata) {
	if serviceUpdateStateGauge == nil || serviceUpdateStartedTs == nil ||
		serviceUpdateCompletedTs == nil {
		serviceUpdateMetricsInitWarnOnce.Do(func() {
			logger.L().Error("service update metrics not configured before use; skipping clear")
		})

		return
	}

	base := labelsForService(metadata)

	knownStates := []string{
		updateStateUpdating,
		updateStateCompleted,
		updateStatePaused,
		updateStateRollbackStarted,
		updateStateRollbackCompleted,
	}

	// Delete state series (one per known state)
	for i := range knownStates {
		state := knownStates[i]

		labels := make(prometheus.Labels, len(base)+1)
		for k, v := range base {
			labels[k] = v
		}

		labels["state"] = state
		_ = serviceUpdateStateGauge.Delete(labels)
	}

	// Delete timestamp series
	_ = serviceUpdateStartedTs.Delete(base)
	_ = serviceUpdateCompletedTs.Delete(base)
}
