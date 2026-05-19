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
	"testing"

	"github.com/prometheus/client_golang/prometheus"
)

// baseServiceLabels returns the minimal label names used by desired/schedulable gauges.
func baseServiceLabels() []string {
	return []string{labelStack, labelService, labelServiceMode, labelDisplayName}
}

// installDesiredReplicasGauges installs unregistered local gauges for
// desiredReplicasGauge and schedulableReplicasGauge, restoring originals on cleanup.
func installDesiredReplicasGauges(t *testing.T) *prometheus.GaugeVec {
	t.Helper()

	labels := baseServiceLabels()
	desiredGauge := prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_desired_replicas", Help: "test"},
		labels,
	)
	schedulableGauge := prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_schedulable_replicas", Help: "test"},
		labels,
	)

	prevD, prevS := desiredReplicasGauge, schedulableReplicasGauge
	desiredReplicasGauge = desiredGauge
	schedulableReplicasGauge = schedulableGauge

	t.Cleanup(func() {
		desiredReplicasGauge = prevD
		schedulableReplicasGauge = prevS
	})

	return desiredGauge
}

// installReplicasStateGauges installs unregistered local gauges for the replicas-state family.
func installReplicasStateGauges(
	t *testing.T,
) (runningGauge, atDesiredGaugeVec *prometheus.GaugeVec) {
	t.Helper()

	base := baseServiceLabels()
	stateLabels := append(append([]string(nil), base...), labelState)

	rsg := prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_replicas_state", Help: "test"},
		stateLabels,
	)
	rrg := prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_running_replicas", Help: "test"},
		base,
	)
	adg := prometheus.NewGaugeVec(prometheus.GaugeOpts{Name: "test_at_desired", Help: "test"}, base)

	prevRSG, prevRRG, prevADG := replicasStateGauge, runningReplicasGauge, atDesiredGauge
	replicasStateGauge = rsg
	runningReplicasGauge = rrg
	atDesiredGauge = adg

	t.Cleanup(func() {
		replicasStateGauge = prevRSG
		runningReplicasGauge = prevRRG
		atDesiredGauge = prevADG
	})

	return rrg, adg
}

// installServiceUpdateGauges installs unregistered local gauges for the service-update family.
func installServiceUpdateGauges(
	t *testing.T,
) (updateStateGauge, startedTSGauge, completedTSGauge *prometheus.GaugeVec) {
	t.Helper()

	base := baseServiceLabels()
	stateLabels := append(append([]string(nil), base...), labelState)

	updateStateGauge = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_update_state_info", Help: "test"},
		stateLabels,
	)
	startedTSGauge = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_update_started_ts", Help: "test"},
		base,
	)
	completedTSGauge = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{Name: "test_update_completed_ts", Help: "test"},
		base,
	)

	prevSG, prevST, prevCT := serviceUpdateStateGauge, serviceUpdateStartedTimestamp, serviceUpdateCompletedTimestamp
	serviceUpdateStateGauge = updateStateGauge
	serviceUpdateStartedTimestamp = startedTSGauge
	serviceUpdateCompletedTimestamp = completedTSGauge

	t.Cleanup(func() {
		serviceUpdateStateGauge = prevSG
		serviceUpdateStartedTimestamp = prevST
		serviceUpdateCompletedTimestamp = prevCT
	})

	return updateStateGauge, startedTSGauge, completedTSGauge
}

// makeTestMetadata builds a minimal serviceMetadata suitable for gauge label tests.
func makeTestMetadata(stack, service, mode string) serviceMetadata {
	return serviceMetadata{
		stack:        stack,
		service:      service,
		serviceMode:  mode,
		customLabels: map[string]string{},
	}
}

// serviceLabels builds the prometheus.Labels for a service under test.
//
//nolint:unparam // mode supports all service modes; current tests exercise replicated only
func serviceLabels(stack, service, mode string) prometheus.Labels {
	return prometheus.Labels{
		labelStack:       stack,
		labelService:     service,
		labelServiceMode: mode,
		labelDisplayName: displayName(stack, service),
	}
}
