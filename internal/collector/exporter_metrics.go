package collector

import (
	"time"

	"github.com/prometheus/client_golang/prometheus"
)

var (
	pollDurationHistogram        prometheus.Histogram
	pollsTotalCounter            prometheus.Counter
	pollErrorsTotalCounter       prometheus.Counter
	eventsReconnectsTotalCounter prometheus.Counter
)

// ConfigureExporterOpsMetrics registers exporter self-observability metrics.
func ConfigureExporterOpsMetrics() {
	//nolint:exhaustruct // use default native-histogram fields per client_golang docs
	pollDurationHistogram = prometheus.NewHistogram(prometheus.HistogramOpts{
		Namespace: "swarm",
		Subsystem: "exporter",
		Name:      "poll_duration_seconds",
		Help:      "Duration of replicas-state polling, in seconds.",
		// Use Prometheus default buckets to avoid magic-number lints and to be generally useful.
		Buckets:     prometheus.DefBuckets,
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollDurationHistogram)

	pollsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   "swarm",
		Subsystem:   "exporter",
		Name:        "polls_total",
		Help:        "Total number of replicas-state polls attempted.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollsTotalCounter)

	pollErrorsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   "swarm",
		Subsystem:   "exporter",
		Name:        "poll_errors_total",
		Help:        "Total number of replicas-state polls that resulted in error.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollErrorsTotalCounter)

	eventsReconnectsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   "swarm",
		Subsystem:   "exporter",
		Name:        "events_reconnects_total",
		Help:        "Total number of event stream reconnects.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(eventsReconnectsTotalCounter)
}

// ObservePollDuration records a single poll duration.
func ObservePollDuration(d time.Duration) {
	if pollDurationHistogram == nil {
		return
	}

	pollDurationHistogram.Observe(d.Seconds())
}

// IncPolls increments the total polls counter.
func IncPolls() {
	if pollsTotalCounter != nil {
		pollsTotalCounter.Inc()
	}
}

// IncPollErrors increments the poll errors counter.
func IncPollErrors() {
	if pollErrorsTotalCounter != nil {
		pollErrorsTotalCounter.Inc()
	}
}

// IncEventReconnect increments the event reconnects counter.
func IncEventReconnect() {
	if eventsReconnectsTotalCounter != nil {
		eventsReconnectsTotalCounter.Inc()
	}
}
