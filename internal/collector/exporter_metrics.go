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
	"sync/atomic"
	"time"

	"github.com/prometheus/client_golang/prometheus"
)

var (
	pollDurationHistogram        prometheus.Histogram
	pollsTotalCounter            prometheus.Counter
	pollErrorsTotalCounter       prometheus.Counter
	eventsReconnectsTotalCounter prometheus.Counter
	eventsDroppedTotalCounter    prometheus.Counter
	pollRejectionsTotalCounter   prometheus.Counter
	resyncsTotalCounter          *prometheus.CounterVec

	lastPollSuccessTimestampGauge   prometheus.GaugeFunc
	lastResyncSuccessTimestampGauge prometheus.GaugeFunc
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

	eventsDroppedTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "events_dropped_total",
		Help:        "Total number of Swarm events dropped because they carried no actor ID.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(eventsDroppedTotalCounter)

	pollRejectionsTotalCounter = prometheus.NewCounter(prometheus.CounterOpts{
		Namespace: prometheusNamespace,
		Subsystem: prometheusExporterSubsystem,
		Name:      "poll_rejections_total",
		Help: "Total number of per-service poll results not published because the service changed " +
			"while its tasks were being listed.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(pollRejectionsTotalCounter)

	resyncsTotalCounter = newResyncsCounter()
	prometheus.MustRegister(resyncsTotalCounter)

	lastPollSuccessTimestampGauge = newUnixNanoTimestampGauge(
		"last_poll_success_timestamp_seconds",
		"Unix time of the latest successfully published replicas-state poll; 0 if none yet.",
		&lastPollSuccessUnixNano,
	)
	prometheus.MustRegister(lastPollSuccessTimestampGauge)

	lastResyncSuccessTimestampGauge = newUnixNanoTimestampGauge(
		"last_resync_success_timestamp_seconds",
		"Unix time of the latest completed resync (service and node list); 0 if none yet.",
		&lastResyncSuccessUnixNano,
	)
	prometheus.MustRegister(lastResyncSuccessTimestampGauge)
}

// newResyncsCounter returns swarm_exporter_resyncs_total with both result series created at 0,
// so increase() and "never failed" alerts work before the first resync.
func newResyncsCounter() *prometheus.CounterVec {
	counter := prometheus.NewCounterVec(prometheus.CounterOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "resyncs_total",
		Help:        "Total number of resyncs (service and node list) by result: success or failure.",
		ConstLabels: nil,
	}, []string{labelResult})

	counter.WithLabelValues(resyncResultSuccess).Add(0)
	counter.WithLabelValues(resyncResultFailure).Add(0)

	return counter
}

// newUnixNanoTimestampGauge returns a swarm_exporter gauge named name that reports unixNano, read
// at scrape time, in seconds; 0 stays 0 ("never").
//
//nolint:ireturn // prometheus.NewGaugeFunc returns this interface; its implementation is unexported
func newUnixNanoTimestampGauge(name, help string, unixNano *atomic.Int64) prometheus.GaugeFunc {
	return prometheus.NewGaugeFunc(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        name,
		Help:        help,
		ConstLabels: nil,
	}, func() float64 {
		return float64(unixNano.Load()) / float64(time.Second)
	})
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

// IncEventsDropped increments the dropped events counter.
func IncEventsDropped() {
	if eventsDroppedTotalCounter != nil {
		eventsDroppedTotalCounter.Inc()
	}
}

// IncPollRejections increments the poll rejections counter.
func IncPollRejections() {
	if pollRejectionsTotalCounter != nil {
		pollRejectionsTotalCounter.Inc()
	}
}

// IncResync counts one resync with result resyncResultSuccess or resyncResultFailure.
func IncResync(result string) {
	if resyncsTotalCounter != nil {
		resyncsTotalCounter.WithLabelValues(result).Inc()
	}
}
