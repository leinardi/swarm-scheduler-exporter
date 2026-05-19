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

	"github.com/docker/docker/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

// installLocalNodeGauge sets nodesByStateGauge to a fresh, unregistered GaugeVec
// for the duration of the test, then restores the original on cleanup.
func installLocalNodeGauge(t *testing.T) *prometheus.GaugeVec {
	t.Helper()

	g := prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Name: "test_swarm_cluster_nodes_by_state",
		Help: "test",
	}, []string{"role", "availability", "status"})
	prev := nodesByStateGauge
	nodesByStateGauge = g

	t.Cleanup(func() { nodesByStateGauge = prev })

	return g
}

func TestUpdateNodesByStateFromSlice_Aggregation(t *testing.T) {
	g := installLocalNodeGauge(t)

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

	workerReady := testutil.ToFloat64(g.With(prometheus.Labels{
		"role": "worker", "availability": "active", "status": "ready",
	}))
	if workerReady != 2 {
		t.Errorf("worker/active/ready = %v, want 2", workerReady)
	}

	mgrDown := testutil.ToFloat64(g.With(prometheus.Labels{
		"role": "manager", "availability": "drain", "status": "down",
	}))
	if mgrDown != 1 {
		t.Errorf("manager/drain/down = %v, want 1", mgrDown)
	}
}

func TestUpdateNodesByStateFromSlice_ResetsBetweenCalls(t *testing.T) {
	g := installLocalNodeGauge(t)

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

	// worker entry from setA should be gone after setB (Reset was called).
	workerCount := testutil.ToFloat64(g.With(prometheus.Labels{
		"role": "worker", "availability": "active", "status": "ready",
	}))
	if workerCount != 0 {
		t.Errorf("worker count should be 0 after reset, got %v", workerCount)
	}

	mgrCount := testutil.ToFloat64(g.With(prometheus.Labels{
		"role": "manager", "availability": "active", "status": "ready",
	}))
	if mgrCount != 1 {
		t.Errorf("manager count should be 1, got %v", mgrCount)
	}
}

func TestUpdateNodesByStateFromSlice_Empty(t *testing.T) {
	g := installLocalNodeGauge(t)

	UpdateNodesByStateFromSlice(nil)
	// Gauge should exist but have nothing — no panic, metric count = 0.
	count := testutil.CollectAndCount(g)
	if count != 0 {
		t.Errorf("expected 0 series for empty nodes, got %d", count)
	}
}
