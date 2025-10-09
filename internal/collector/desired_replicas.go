package collector

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
	desiredReplicasGauge *prometheus.GaugeVec
	nodeCount            = 0
)

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

func updateServiceReplicasGauge(svc *swarm.Service, metadata serviceMetadata) {
	if svc.Spec.Mode.Replicated != nil {
		setDesiredReplicasGauge(metadata, float64(*svc.Spec.Mode.Replicated.Replicas))
	} else {
		setDesiredReplicasGauge(metadata, float64(nodeCount))
	}
}

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
			// @TODO: auto-reconnect when connection lost
			return fmt.Errorf("events stream: %w", err)
		case evt := <-evtCh:
			// process in a goroutine but pass by pointer to avoid copying
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

func processEvent(ctx context.Context, cli *client.Client, evt *events.Message) error {
	if evt.Type == "node" {
		// Re-init desired replicas gauge when a node is added/deleted,
		// to be sure global services have the right number of desired replicas
		//nolint:exhaustive // we only care about create/remove for node counts
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
			// ignore other node actions
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

		//	@TODO:	at this point, the vector should be deleted (?)
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

	svcPtr := &svc
	metadataCache[sid] = buildMetadata(svcPtr)
	updateServiceReplicasGauge(svcPtr, metadataCache[sid])

	return nil
}
