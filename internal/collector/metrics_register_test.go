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

func TestConfigureDesiredReplicasGauge_RegistersWithoutPanic(t *testing.T) {
	ConfigureDesiredReplicasGauge()
	t.Cleanup(func() {
		prometheus.Unregister(desiredReplicasGauge)
		prometheus.Unregister(schedulableReplicasGauge)
	})
}

func TestConfigureReplicasStateGauge_RegistersWithoutPanic(t *testing.T) {
	ConfigureReplicasStateGauge()
	t.Cleanup(func() {
		prometheus.Unregister(replicasStateGauge)
		prometheus.Unregister(runningReplicasGauge)
		prometheus.Unregister(atDesiredGauge)
	})
}

func TestConfigureNodesByStateGauge_RegistersWithoutPanic(t *testing.T) {
	ConfigureNodesByStateGauge()
	t.Cleanup(func() {
		prometheus.Unregister(nodesByStateGauge)
	})
}

func TestConfigureContainersStateGauge_RegistersWithoutPanic(t *testing.T) {
	containersStateGauge = nil // reset guard so configure actually runs

	ConfigureContainersStateGauge()
	t.Cleanup(func() {
		prometheus.Unregister(containersStateGauge)
		containersStateGauge = nil
	})
}

func TestConfigureServiceUpdateMetrics_RegistersWithoutPanic(t *testing.T) {
	ConfigureServiceUpdateMetrics()
	t.Cleanup(func() {
		prometheus.Unregister(serviceUpdateStateGauge)
		prometheus.Unregister(serviceUpdateStartedTimestamp)
		prometheus.Unregister(serviceUpdateCompletedTimestamp)
	})
}

func TestConfigureHealthGauges_RegistersWithoutPanic(t *testing.T) {
	ConfigureHealthGauges("v1.0.0", "abc1234", "2025-01-01")
	t.Cleanup(func() {
		prometheus.Unregister(exporterHealthGauge)
		prometheus.Unregister(buildInfoGauge)
	})
}

func TestConfigureExporterOpsMetrics_RegistersWithoutPanic(t *testing.T) {
	ConfigureExporterOpsMetrics()
	t.Cleanup(func() {
		prometheus.Unregister(pollDurationHistogram)
		prometheus.Unregister(pollsTotalCounter)
		prometheus.Unregister(pollErrorsTotalCounter)
		prometheus.Unregister(eventsReconnectsTotalCounter)
	})
}
