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

// desiredReplicasGauge is the gauge vector exported at /metrics.
var desiredReplicasGauge *prometheus.GaugeVec

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
// by listing services and nodes once at startup (or after node changes).
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

	setNodeCount(len(nodes))

	// Reset the whole vector so removed services are dropped from Prometheus.
	desiredReplicasGauge.Reset()

	for i := range services { // avoid copying large struct
		svc := &services[i]
		setServiceMetadata(svc.ID, buildMetadata(svc))
		updateServiceReplicasGauge(svc, mustGetServiceMetadata(svc.ID))
	}

	return nil
}

// mustGetServiceMetadata loads metadata for serviceID; it logs a warning and
// returns an empty (but fully constructed) metadata instance if missing.
func mustGetServiceMetadata(serviceID string) serviceMetadata {
	metadata, ok := getServiceMetadata(serviceID)
	if !ok {
		// This should not happen in current flows; log and return empty labels instead of panicking.
		logrus.WithField("service_id", serviceID).Warn("metadata missing unexpectedly")

		return serviceMetadata{
			stack:        "",
			service:      "",
			serviceMode:  "",
			customLabels: map[string]string{},
		}
	}

	return metadata
}

// labelsForMetadata builds a sanitized label set (same keys used for With/Delete).
func labelsForMetadata(metadata serviceMetadata) prometheus.Labels {
	labels := prometheus.Labels{
		"stack":        metadata.stack,
		"service":      metadata.service,
		"service_mode": metadata.serviceMode,
	}
	for key, value := range metadata.customLabels {
		labels[key] = value
	}

	return labelutil.SanitizeMetricLabels(labels)
}

// updateServiceReplicasGauge sets the per-service desired replicas value
// based on the service mode and configured replica count.
func updateServiceReplicasGauge(svc *swarm.Service, metadata serviceMetadata) {
	if svc.Spec.Mode.Replicated != nil {
		setDesiredReplicasGauge(metadata, float64(*svc.Spec.Mode.Replicated.Replicas))
	} else {
		setDesiredReplicasGauge(metadata, float64(getNodeCount()))
	}
}

// setDesiredReplicasGauge writes the gauge value with sanitized label keys.
func setDesiredReplicasGauge(metadata serviceMetadata, val float64) {
	desiredReplicasGauge.With(labelsForMetadata(metadata)).Set(val)
}

// ListenSwarmEvents listens to Docker events for service and node changes.
// Node create/remove updates nodeCount and re-seeds the desired replicas gauge.
// Service updates refresh the cached metadata; service remove deletes the time-series.
func ListenSwarmEvents(ctx context.Context, cli *client.Client) error {
	filterArgs := filters.NewArgs()
	filterArgs.Add("type", "service")
	filterArgs.Add("type", "node")

	eventChan, errorChan := cli.Events(ctx, events.ListOptions{
		Since:   time.Now().Format(time.RFC3339),
		Filters: filterArgs,
		Until:   "",
	})

	logrus.Info("Start listening for new Swarm events...")

	for {
		select {
		case err := <-errorChan:
			// TODO: add reconnect with backoff.
			return fmt.Errorf("events stream: %w", err)
		case evt := <-eventChan:
			// Process in a goroutine but pass by value to avoid pointer-to-loop-var pitfalls.
			eventCopy := evt
			go func(eventMsg events.Message) {
				logrus.WithFields(logrus.Fields{
					"type":       eventMsg.Type,
					"action":     eventMsg.Action,
					"actor.id":   eventMsg.Actor.ID,
					"actor.name": eventMsg.Actor.Attributes["name"],
				}).Info("New event received.")

				err := processEvent(ctx, cli, &eventMsg)
				if err != nil {
					logrus.Error(err)
				}
			}(eventCopy)
		}
	}
}

// processEvent handles node and service events. For nodes, only create/remove
// matters for desired replica approximation. For services, remove deletes the series;
// all other actions trigger a fresh inspect to update the cache and gauge.
func processEvent(ctx context.Context, cli *client.Client, evt *events.Message) error {
	if evt.Type == "node" {
		switch evt.Action { //nolint:exhaustive // only create/remove affect desired replica approximation here
		case events.ActionCreate:
			incNodeCount(1)

			err := InitDesiredReplicasGauge(ctx, cli)
			if err != nil {
				logrus.WithError(err).Warn("InitDesiredReplicasGauge after node create")
			}
		case events.ActionRemove:
			incNodeCount(-1)

			err := InitDesiredReplicasGauge(ctx, cli)
			if err != nil {
				logrus.WithError(err).Warn("InitDesiredReplicasGauge after node remove")
			}
		default:
			// Ignore other node events (e.g., updates) for this gauge.
		}

		return nil
	}

	serviceID := evt.Actor.ID

	switch evt.Action { //nolint:exhaustive // for services we only handle remove vs others
	case events.ActionRemove:
		metadata, ok := getServiceMetadata(serviceID)
		if !ok {
			return ErrNoCachedMetadata
		}
		// Delete the series entirely to avoid stale zero-valued metrics.
		_ = desiredReplicasGauge.Delete(labelsForMetadata(metadata))

		deleteServiceMetadata(serviceID)

		return nil
	default:
		// treat other service actions as update
	}

	service, _, err := cli.ServiceInspectWithRaw(ctx, serviceID, types.ServiceInspectOptions{
		InsertDefaults: false,
	})
	if err != nil {
		return fmt.Errorf("service inspect %s: %w", serviceID, err)
	}

	setServiceMetadata(serviceID, buildMetadata(&service))
	updateServiceReplicasGauge(&service, mustGetServiceMetadata(serviceID))

	return nil
}
