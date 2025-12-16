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
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "health",
		Help:        "Exporter health status: 1=healthy, 0=unhealthy.",
		ConstLabels: nil,
	})
	prometheus.MustRegister(exporterHealthGauge)

	buildInfoGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "build_info",
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

	window := max(3*pollDelay, minWindow)

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
