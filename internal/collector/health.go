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
	lastPollSuccessUnixNano   atomic.Int64
	lastEventsConnectUnixNano atomic.Int64

	// Prometheus health metrics.
	exporterHealthGauge prometheus.GaugeFunc
	buildInfoGauge      *prometheus.GaugeVec
)

// ConfigureHealthGauges registers the health and build info metrics. The health gauge is
// evaluated on every scrape with the same check as /healthz, so it turns 0 when polls stop
// succeeding, even if the poller is stuck or never started.
func ConfigureHealthGauges(version, commit, date string, pollDelay time.Duration) {
	exporterHealthGauge = newHealthGauge(pollDelay)
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

// newHealthGauge returns swarm_exporter_health, reporting HealthSnapshot at scrape time.
//
//nolint:ireturn // prometheus.NewGaugeFunc returns this interface; its implementation is unexported
func newHealthGauge(pollDelay time.Duration) prometheus.GaugeFunc {
	return prometheus.NewGaugeFunc(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusExporterSubsystem,
		Name:        "health",
		Help:        "Exporter health status: 1=healthy, 0=unhealthy.",
		ConstLabels: nil,
	}, func() float64 {
		healthy, _ := HealthSnapshot(pollDelay, time.Now())
		if healthy {
			return 1
		}

		return 0
	})
}

// MarkPollOK records the time of the latest successful replicas-state publish. Callers pass the
// time the publish completed, not the time the poll started, so a slow poll does not look fresher
// than it is.
func MarkPollOK(now time.Time) {
	lastPollSuccessUnixNano.Store(now.UnixNano())
}

// MarkEventsConnected records the time at which the event stream connected (or reconnected).
func MarkEventsConnected(now time.Time) {
	lastEventsConnectUnixNano.Store(now.UnixNano())
}

// HealthSnapshot returns whether the exporter is healthy and a human reason.
// Healthy if:
//   - we have at least one successful poll, and
//   - that poll is not older than max(3*pollDelay, 30s).
func HealthSnapshot(pollDelay time.Duration, now time.Time) (healthy bool, reason string) {
	lastPollUnixNano := lastPollSuccessUnixNano.Load()
	if lastPollUnixNano == 0 {
		return false, "no successful poll yet"
	}

	lastPoll := time.Unix(0, lastPollUnixNano)

	// Staleness threshold: more lenient of the two
	minWindow := 30 * time.Second

	window := max(3*pollDelay, minWindow)

	if now.Sub(lastPoll) > window {
		return false, "last poll too old"
	}

	return true, ""
}
