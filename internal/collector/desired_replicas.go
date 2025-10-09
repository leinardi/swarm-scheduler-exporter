package collector

// desired_replicas exposes the gauge "swarm_service_desired_replicas", which tracks
// the scheduler's desired replica count for each service. For replicated services,
// this is the configured replica count. For global services, it approximates the
// number of eligible nodes by counting nodes (note: constraints/drain are not
// evaluated here; see README for caveats).

import (
	"context"
	"fmt"
	"time"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/events"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/sirupsen/logrus"
)

var (
	// desiredReplicasGauge is the gauge vector exported at /metrics.
	desiredReplicasGauge *prometheus.GaugeVec
	// nodeCount is used to approximate desired replicas for global services.
	nodeCount = 0
)

// ConfigureDesiredReplicasGauge registers the "swarm_service_desired_replicas" gauge
// with base labels (stack, service, service_mode) plus any user-specified custom labels.
func ConfigureDesiredReplicasGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
	}, customLabels...)

	desiredReplicasGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "",
		Subsystem:   "",
		Name:        "swarm_service_desired_replicas",
		Help:        "Number of desired replicas for swarm services",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)
}

// InitDesiredReplicasGauge seeds the gauge with current desired replica counts
// by listing services and nodes once at startup.
func InitDesiredReplicasGauge(ctx context.Context, cli *client.Client) error {
	services, err := cli.ServiceList(ctx, types.ServiceListOptions{
		Filters: filters.Args{},
		Status:  false,
	})
	if err != nil {
		return fmt.Errorf("service list: %w", err)
	}

	nodes, err := cli.NodeList(ctx, types.NodeListOptions{
		Filters: filters.Args{},
	})
	if err != nil {
		return fmt.Errorf("node list: %w", err)
	}

	nodeCount = len(nodes)

	for i := range services { // avoid copying large struct
		svc := &services[i]
		metadataCache[svc.ID] = buildMetadata(svc)
		updateServiceReplicasGauge(svc, metadataCache[svc.ID])
	}

	return nil
}

// updateServiceReplicasGauge sets the per-service desired replicas value
// based on the service mode and configured replica count.
func updateServiceReplicasGauge(svc *swarm.Service, metadata serviceMetadata) {
	if svc.Spec.Mode.Replicated != nil {
		setDesiredReplicasGauge(metadata, float64(*svc.Spec.Mode.Replicated.Replicas))
	} else {
		setDesiredReplicasGauge(metadata, float64(nodeCount))
	}
}

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata serviceMetadata, val float64) {
	labels := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
	}
	for k, v := range metadata.customLabels {
		labels[k] = v
	}

	desiredReplicasGauge.With(labelutil.SanitizeMetricLabels(labels)).Set(val)
}

// ListenSwarmEvents listens to Docker events for service and node changes.
// Node create/remove updates nodeCount and re-seeds the desired replicas gauge.
// Service updates refresh the cached metadata; service remove clears the gauge.
func ListenSwarmEvents(ctx context.Context, cli *client.Client) error {
	filterArgs := filters.NewArgs()
	filterArgs.Add("type", "service")
	filterArgs.Add("type", "node")

	evtCh, errCh := cli.Events(ctx, events.ListOptions{
		Since:   time.Now().Format(time.RFC3339),
		Filters: filterArgs,
		Until:   "",
	})

	logrus.Info("Start listening for new Swarm events...")

	for {
		select {
		case err := <-errCh:
			// TODO: add reconnect with backoff.
			return fmt.Errorf("events stream: %w", err)
		case evt := <-evtCh:
			// Process in a goroutine but pass by pointer to avoid copying the event struct.
			go func(evt *events.Message) {
				logrus.WithFields(logrus.Fields{
					"type":       evt.Type,
					"action":     evt.Action,
					"actor.id":   evt.Actor.ID,
					"actor.name": evt.Actor.Attributes["name"],
				}).Info("New event received.")

				err := processEvent(ctx, cli, evt)
				if err != nil {
					logrus.Error(err)
				}
			}(&evt)
		}
	}
}

// processEvent handles node and service events. For nodes, only create/remove
// matters for desired replica approximation. For services, remove clears metrics;
// all other actions trigger a fresh inspect to update the cache and gauge.
func processEvent(ctx context.Context, cli *client.Client, evt *events.Message) error {
	if evt.Type == "node" {
		//nolint:exhaustive // only create/remove affect desired replica approximation
		switch evt.Action {
		case events.ActionCreate:
			nodeCount++

			err := InitDesiredReplicasGauge(ctx, cli)
			if err != nil {
				logrus.WithError(err).Warn("InitDesiredReplicasGauge after node create")
			}
		case events.ActionRemove:
			nodeCount--

			err := InitDesiredReplicasGauge(ctx, cli)
			if err != nil {
				logrus.WithError(err).Warn("InitDesiredReplicasGauge after node remove")
			}
		default:
			// Ignore other node events (e.g., updates) for this gauge.
		}

		return nil
	}

	sid := evt.Actor.ID

	//nolint:exhaustive // for services we only handle remove vs others
	switch evt.Action {
	case events.ActionRemove:
		metadata, ok := metadataCache[sid]
		if !ok {
			return ErrNoCachedMetadata
		}

		// TODO: at this point, the vector should be deleted (?)
		setDesiredReplicasGauge(metadata, float64(0))
		// Clean up labels cache as this won't be used anymore
		delete(metadataCache, sid)

		return nil
	default:
		// treat other service actions as update
	}

	svc, _, err := cli.ServiceInspectWithRaw(ctx, sid, types.ServiceInspectOptions{
		InsertDefaults: false,
	})
	if err != nil {
		return fmt.Errorf("service inspect %s: %w", sid, err)
	}

	metadataCache[sid] = buildMetadata(&svc)
	updateServiceReplicasGauge(&svc, metadataCache[sid])

	return nil
}
