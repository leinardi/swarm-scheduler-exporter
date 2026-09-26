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

// installReplicasStateGauges installs a fresh unregistered replicas-state snapshot collector with
// the base labels only, restoring the original on cleanup.
func installReplicasStateGauges(t *testing.T) *replicasStateSnapshot {
	t.Helper()

	families := newReplicasStateSnapshot(nil)

	previous := replicasStateCollector
	replicasStateCollector = families

	t.Cleanup(func() { replicasStateCollector = previous })

	return families
}

// installNodesByStateGauge installs a fresh unregistered nodes-by-state snapshot collector,
// restoring the original on cleanup.
func installNodesByStateGauge(t *testing.T) *snapshotFamily {
	t.Helper()

	family := newNodesByStateFamily()

	previous := nodesByStateGauge
	nodesByStateGauge = family

	t.Cleanup(func() { nodesByStateGauge = previous })

	return family
}

// installContainersStateGauge installs a fresh unregistered container-state snapshot collector
// and enables container metrics, restoring the originals on cleanup.
func installContainersStateGauge(t *testing.T) *snapshotFamily {
	t.Helper()

	family := newContainersStateFamily()

	previous, previousEnabled := containersStateGauge, containersEnabled
	containersStateGauge, containersEnabled = family, true

	t.Cleanup(func() { containersStateGauge, containersEnabled = previous, previousEnabled })

	return family
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

// Family names of the snapshot collectors, as the tests read them back.
const (
	replicasStateFQName   = "swarm_task_replicas_state"
	runningReplicasFQName = "swarm_service_running_replicas"
	atDesiredFQName       = "swarm_service_at_desired"
	nodesByStateFQName    = "swarm_cluster_nodes_by_state"
	containerStateFQName  = "swarm_container_state"
)

// familySeries returns the series of family fqName among series gathered by gatherSeries.
func familySeries(series map[string]float64, fqName string) map[string]float64 {
	family := make(map[string]float64)

	for id, value := range series {
		if strings.HasPrefix(id, fqName+"{") {
			family[id] = value
		}
	}

	return family
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
