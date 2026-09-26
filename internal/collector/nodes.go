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

	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus"

	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

const (
	labelNodeRole         = "role"
	labelNodeAvailability = "availability"
	labelNodeStatus       = "status"
)

var nodesByStateGauge *snapshotFamily

// ConfigureNodesByStateGauge registers swarm_cluster_nodes_by_state.
func ConfigureNodesByStateGauge() {
	nodesByStateGauge = newNodesByStateFamily()
	prometheus.MustRegister(nodesByStateGauge)
}

// newNodesByStateFamily returns the swarm_cluster_nodes_by_state collector with nothing published.
func newNodesByStateFamily() *snapshotFamily {
	return newSnapshotFamily(
		prometheus.BuildFQName(prometheusNamespace, prometheusClusterSubsystem, "nodes_by_state"),
		"Number of Swarm nodes grouped by role, availability, and status.",
		[]string{labelNodeRole, labelNodeAvailability, labelNodeStatus},
	)
}

// UpdateNodesByState refreshes the nodes list from Docker and updates the gauge.
func UpdateNodesByState(ctx context.Context, cli DockerAPI) error {
	listResult, listErr := cli.NodeList(ctx, client.NodeListOptions{Filters: nil})
	if listErr != nil {
		return fmt.Errorf("node list: %w", listErr)
	}

	nodes := listResult.Items

	setCachedNodes(nodes) // keep the cache fresh for other computations
	UpdateNodesByStateFromSlice(nodes)

	return nil
}

// UpdateNodesByStateFromSlice publishes the gauge from a pre-fetched snapshot. The whole set is
// built first and replaces the previous one in a single swap: a scrape never sees it half
// rebuilt, and concurrent refreshes from the event workers each publish a complete set (the last
// one wins) instead of interleaving their writes. If the build fails, the previous set stays.
func UpdateNodesByStateFromSlice(nodes []swarm.Node) {
	if nodesByStateGauge == nil {
		return
	}

	metrics, buildErr := buildNodesByState(nodesByStateGauge, nodes).build()
	if buildErr != nil {
		logger.L().Error("build nodes by state snapshot; keeping previous", "err", buildErr)

		return
	}

	nodesByStateGauge.publish(metrics)
}

// buildNodesByState records the node count per role, availability and status without
// publishing it. Combinations with no node get no series, so statuses no longer seen disappear.
func buildNodesByState(family *snapshotFamily, nodes []swarm.Node) *snapshotBuilder {
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

	builder := newSnapshotBuilder()

	for k, v := range counts {
		builder.set(family.desc, family.labelNames, prometheus.Labels{
			labelNodeRole:         k.role,
			labelNodeAvailability: k.availability,
			labelNodeStatus:       k.status,
		}, v)
	}

	return builder
}
