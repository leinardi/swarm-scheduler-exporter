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

	"github.com/moby/moby/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
)

func TestUpdateNodesByStateFromSlice_Aggregation(t *testing.T) {
	family := installNodesByStateGauge(t)

	nodes := []swarm.Node{
		{
			Spec: swarm.NodeSpec{
				Role:         swarm.NodeRoleWorker,
				Availability: swarm.NodeAvailabilityActive,
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		},
		{
			Spec: swarm.NodeSpec{
				Role:         swarm.NodeRoleWorker,
				Availability: swarm.NodeAvailabilityActive,
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		},
		{
			Spec: swarm.NodeSpec{
				Role:         swarm.NodeRoleManager,
				Availability: swarm.NodeAvailabilityDrain,
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateDown},
		},
	}

	UpdateNodesByStateFromSlice(nodes)

	gathered := gatherSeries(t, family)

	if got := len(gathered); got != 2 {
		t.Errorf("series = %d, want 2", got)
	}

	workerReady := gathered[nodeSeriesID("worker", "active", "ready")]
	if workerReady != 2 {
		t.Errorf("worker/active/ready = %v, want 2", workerReady)
	}

	mgrDown := gathered[nodeSeriesID("manager", "drain", "down")]
	if mgrDown != 1 {
		t.Errorf("manager/drain/down = %v, want 1", mgrDown)
	}
}

func TestUpdateNodesByStateFromSlice_ResetsBetweenCalls(t *testing.T) {
	family := installNodesByStateGauge(t)

	setA := []swarm.Node{
		{
			Spec: swarm.NodeSpec{
				Role:         swarm.NodeRoleWorker,
				Availability: swarm.NodeAvailabilityActive,
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		},
	}
	setB := []swarm.Node{
		{
			Spec: swarm.NodeSpec{
				Role:         swarm.NodeRoleManager,
				Availability: swarm.NodeAvailabilityActive,
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		},
	}

	UpdateNodesByStateFromSlice(setA)
	UpdateNodesByStateFromSlice(setB)

	// The publish of setB replaces setA's set: its worker series is gone.
	gathered := gatherSeries(t, family)

	if _, found := gathered[nodeSeriesID("worker", "active", "ready")]; found {
		t.Errorf("worker series from setA still present after setB: %v", gathered)
	}

	mgrCount, found := gathered[nodeSeriesID("manager", "active", "ready")]
	if !found || mgrCount != 1 {
		t.Errorf("manager count = %v (found %v), want 1", mgrCount, found)
	}
}

func TestUpdateNodesByStateFromSlice_Empty(t *testing.T) {
	family := installNodesByStateGauge(t)

	UpdateNodesByStateFromSlice(nil)
	// Nothing to publish — no panic, no series.
	count := len(gatherSeries(t, family))
	if count != 0 {
		t.Errorf("expected 0 series for empty nodes, got %d", count)
	}
}

// nodeSeriesID is the seriesID of a nodes_by_state series.
func nodeSeriesID(role, availability, status string) string {
	return seriesID(nodesByStateFQName, prometheus.Labels{
		labelNodeRole: role, labelNodeAvailability: availability, labelNodeStatus: status,
	})
}

func testNodes(count int, role swarm.NodeRole) []swarm.Node {
	nodes := make([]swarm.Node, count)
	for index := range nodes {
		nodes[index] = swarm.Node{
			Spec:   swarm.NodeSpec{Role: role, Availability: swarm.NodeAvailabilityActive},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		}
	}

	return nodes
}

// TestUpdateNodesByStateFromSlice_BuildDoesNotPublish checks that a scrape sees the whole
// previous set while the next one is being built (#72).
func TestUpdateNodesByStateFromSlice_BuildDoesNotPublish(t *testing.T) {
	family := installNodesByStateGauge(t)

	expectedA := map[string]float64{
		nodeSeriesID("worker", "active", "ready"): 3,
	}
	expectedB := map[string]float64{
		nodeSeriesID("manager", "active", "ready"): 2,
	}

	UpdateNodesByStateFromSlice(testNodes(3, swarm.NodeRoleWorker))
	assertGathered(t, family, expectedA)

	builder := buildNodesByState(family, testNodes(2, swarm.NodeRoleManager))
	assertGathered(t, family, expectedA)

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build B: %v", buildErr)
	}

	assertGathered(t, family, expectedA)

	family.publish(metrics)
	assertGathered(t, family, expectedB)
}
