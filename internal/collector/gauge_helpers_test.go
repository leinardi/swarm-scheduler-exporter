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
	"fmt"
	"sort"
	"strings"
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
func serviceLabels(stack, service, mode string) prometheus.Labels {
	return prometheus.Labels{
		labelStack:       stack,
		labelService:     service,
		labelServiceMode: mode,
		labelDisplayName: displayName(stack, service),
	}
}

// gatherSeries registers collector on a throwaway pedantic registry, which also fails a Collect
// that emits a metric its Describe did not announce, and returns every gathered series keyed by
// seriesID. Any gather error fails the test.
func gatherSeries(t *testing.T, collector prometheus.Collector) map[string]float64 {
	t.Helper()

	registry := prometheus.NewPedanticRegistry()

	registerErr := registry.Register(collector)
	if registerErr != nil {
		t.Fatalf("register collector: %v", registerErr)
	}

	families, gatherErr := registry.Gather()
	if gatherErr != nil {
		t.Fatalf("gather: %v", gatherErr)
	}

	series := make(map[string]float64)

	for _, family := range families {
		for _, metric := range family.GetMetric() {
			labels := prometheus.Labels{}
			for _, labelPair := range metric.GetLabel() {
				labels[labelPair.GetName()] = labelPair.GetValue()
			}

			series[seriesID(family.GetName(), labels)] = metric.GetGauge().GetValue()
		}
	}

	return series
}

// snapshotValue returns the value of the series of family fqName with exactly labels, as gathered
// from collector by gatherSeries, and whether that series exists.
func snapshotValue(
	t *testing.T,
	collector prometheus.Collector,
	fqName string,
	labels prometheus.Labels,
) (float64, bool) {
	t.Helper()

	value, found := gatherSeries(t, collector)[seriesID(fqName, labels)]

	return value, found
}

// seriesID formats a series as fqName{name="value",...} with label names sorted and values
// quoted, so two different series never share an ID.
func seriesID(fqName string, labels prometheus.Labels) string {
	names := make([]string, 0, len(labels))
	for name := range labels {
		names = append(names, name)
	}

	sort.Strings(names)

	pairs := make([]string, 0, len(names))
	for _, name := range names {
		pairs = append(pairs, fmt.Sprintf("%s=%q", name, labels[name]))
	}

	return fqName + "{" + strings.Join(pairs, ",") + "}"
}
