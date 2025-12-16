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
	"context"
	"fmt"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	"github.com/prometheus/client_golang/prometheus"
)

var nodesByStateGauge *prometheus.GaugeVec

// ConfigureNodesByStateGauge registers swarm_cluster_nodes_by_state.
func ConfigureNodesByStateGauge() {
	nodesByStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   prometheusClusterSubsystem,
		Name:        "nodes_by_state",
		Help:        "Number of Swarm nodes grouped by role, availability, and status.",
		ConstLabels: nil,
	}, []string{"role", "availability", "status"})
	prometheus.MustRegister(nodesByStateGauge)
}

// UpdateNodesByState refreshes the nodes list from Docker and updates the gauge.
func UpdateNodesByState(ctx context.Context, cli *client.Client) error {
	nodes, listErr := cli.NodeList(ctx, types.NodeListOptions{Filters: filters.Args{}})
	if listErr != nil {
		return fmt.Errorf("node list: %w", listErr)
	}

	setCachedNodes(nodes) // keep the cache fresh for other computations
	UpdateNodesByStateFromSlice(nodes)

	return nil
}

// UpdateNodesByStateFromSlice updates the gauge using a pre-fetched snapshot.
func UpdateNodesByStateFromSlice(nodes []swarm.Node) {
	// Reset to avoid ghost series for statuses we no longer see.
	nodesByStateGauge.Reset()

	// Aggregate counts: role × availability × status
	type key struct {
		role         string
		availability string
		status       string
	}

	counts := make(map[key]float64)

	for i := range nodes {
		node := &nodes[i]
		role := string(node.Spec.Role)                 // manager|worker
		availability := string(node.Spec.Availability) // active|pause|drain
		status := string(node.Status.State)            // ready|down|...
		counts[key{role: role, availability: availability, status: status}]++
	}

	// Write out the series
	for k, v := range counts {
		nodesByStateGauge.With(prometheus.Labels{
			"role":         k.role,
			"availability": k.availability,
			"status":       k.status,
		}).Set(v)
	}
}
