package collector

import (
	"sync/atomic"
	"time"

	"github.com/prometheus/client_golang/prometheus"
)

const (
	// MinimumPollDelaySecs is enforced by main; kept here for clarity if reused.
	MinimumPollDelaySecs = 1
)

var (
	// UnixNano timestamps (0 means "never").
	lastPollSuccessUnixNano   int64
	lastEventsConnectUnixNano int64

	// Prometheus health metrics.
	exporterHealthGauge prometheus.Gauge
	buildInfoGauge      *prometheus.GaugeVec
)

// ConfigureHealthGauges registers the health and build info metrics.
func ConfigureHealthGauges(version, commit, date string) {
	exporterHealthGauge = prometheus.NewGauge(prometheus.GaugeOpts{
		Namespace:   "",
		Subsystem:   "",
		Name:        "swarm_exporter_health",
		Help:        "Exporter health status: 1=healthy, 0=unhealthy.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(exporterHealthGauge)

	buildInfoGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "",
		Subsystem:   "",
		Name:        "swarm_exporter_build_info",
		Help:        "Build information for this exporter.",
		ConstLabels: nil,
	}, []string{"version", "commit", "date"})
	prometheus.MustRegister(buildInfoGauge)

	// Set build info to 1 with labels.
	buildInfoGauge.WithLabelValues(version, commit, date).Set(1)
}

// MarkPollOK records the time of the latest successful replicas-state publish.
func MarkPollOK(now time.Time) {
	atomic.StoreInt64(&lastPollSuccessUnixNano, now.UnixNano())
}

// MarkEventsConnected records the time at which the event stream connected (or reconnected).
func MarkEventsConnected(now time.Time) {
	atomic.StoreInt64(&lastEventsConnectUnixNano, now.UnixNano())
}

// HealthSnapshot returns whether the exporter is healthy and a human reason.
// Healthy if:
//   - we have at least one successful poll, and
//   - that poll is not older than max(3*pollDelay, 30s).
func HealthSnapshot(pollDelay time.Duration, now time.Time) (healthy bool, reason string) {
	lastPoll := time.Unix(0, atomic.LoadInt64(&lastPollSuccessUnixNano))
	if lastPoll.IsZero() {
		return false, "no successful poll yet"
	}

	// Staleness threshold: more lenient of the two
	minWindow := 30 * time.Second

	window := 3 * pollDelay
	if window < minWindow {
		window = minWindow
	}

	if now.Sub(lastPoll) > window {
		return false, "last poll too old"
	}

	return true, ""
}

// SetExporterHealth sets the health gauge to 1 or 0.
func SetExporterHealth(healthy bool) {
	if exporterHealthGauge == nil {
		return
	}

	if healthy {
		exporterHealthGauge.Set(1)
	} else {
		exporterHealthGauge.Set(0)
	}
}
