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
	pollDurationHistogram = prometheus.NewHistogram(prometheus.HistogramOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusExporterSubsystem,
		Name:      "poll_duration_seconds",
		Help:      "Duration of replicas-state polling, in seconds.",
		// Use Prometheus default buckets to avoid magic-number lints and to be generally useful.
		Buckets:     prometheus.DefBuckets,
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollDurationHistogram)

	pollsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "polls_total",
		Help:        "Total number of replicas-state polls attempted.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollsTotalCounter)

	pollErrorsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "poll_errors_total",
		Help:        "Total number of replicas-state polls that resulted in error.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollErrorsTotalCounter)

	eventsReconnectsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "events_reconnects_total",
		Help:        "Total number of event stream reconnects.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(eventsReconnectsTotalCounter)
}

// ObservePollDuration records a single poll duration.
func ObservePollDuration(duration time.Duration) {
	if pollDurationHistogram == nil {
		return
	}

	pollDurationHistogram.Observe(duration.Seconds())
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
