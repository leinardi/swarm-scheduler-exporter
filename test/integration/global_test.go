//go:build integration

/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
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

package integration_test

import (
	"testing"
	"time"

	"github.com/moby/moby/api/types/swarm"
)

// TestGlobal_Constraints checks that a global service's desired count follows its placement
// constraints the way Swarm evaluates them, including when a node label changes under a running
// exporter (the node-event path). Swarm's own running count is asserted alongside as ground truth.
func TestGlobal_Constraints(t *testing.T) {
	const (
		stack    = "it-global"
		zoneKey  = "zone"
		zoneRule = "node.labels." + zoneKey
	)

	nodeCount := float64(len(cluster.Nodes))
	zoned := serviceKey{stack: stack, service: "zoned"}
	elsewhere := serviceKey{stack: stack, service: "elsewhere"}

	deployService(
		t,
		stack,
		zoned.service,
		&serviceOpts{global: true, constraints: []string{zoneRule + "==a"}},
	)
	// != against nodes without the label: a missing label compares as empty, so it matches.
	deployService(
		t,
		stack,
		elsewhere.service,
		&serviceOpts{global: true, constraints: []string{zoneRule + "!=a"}},
	)

	baseURL := startExporter(t, []serviceKey{zoned, elsewhere})

	eventually(t, 60*time.Second, metricsMatch(baseURL, stack,
		wantService(metricDesired, zoned, 0),
		wantService(metricRunning, zoned, 0),
		wantService(metricAtDesired, zoned, 1),
		wantService(metricDesired, elsewhere, nodeCount),
		wantService(metricRunning, elsewhere, nodeCount),
		wantService(metricAtDesired, elsewhere, 1),
	))

	// Any node will do; the last one is a worker whenever the cluster has one.
	zonedNode := cluster.Nodes[len(cluster.Nodes)-1]

	t.Cleanup(func() {
		ctx, cancel := teardownCtx()
		defer cancel()

		err := updateNodeSpec(
			ctx,
			zonedNode.SwarmNodeID,
			func(spec *swarm.NodeSpec) { delete(spec.Labels, zoneKey) },
		)
		if err != nil {
			t.Errorf("cleanup: %v", err)
		}

		err = waitNodesReadyActive(ctx)
		if err != nil {
			t.Errorf("cleanup: %v", err)
		}
	})

	// Upper case on the node, lower case in the constraints: Swarm compares label values
	// case-insensitively, for == and != alike.
	updateNode(t, zonedNode.SwarmNodeID, func(spec *swarm.NodeSpec) {
		if spec.Labels == nil {
			spec.Labels = map[string]string{}
		}

		spec.Labels[zoneKey] = "A"
	})

	eventually(t, 60*time.Second, metricsMatch(baseURL, stack,
		wantService(metricDesired, zoned, 1),
		wantService(metricRunning, zoned, 1),
		wantService(metricAtDesired, zoned, 1),
		wantService(metricDesired, elsewhere, nodeCount-1),
		wantService(metricRunning, elsewhere, nodeCount-1),
		wantService(metricAtDesired, elsewhere, 1),
	))
}
