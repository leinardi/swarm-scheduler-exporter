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
