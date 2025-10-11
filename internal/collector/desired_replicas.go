package collector

import (
	"context"
	"errors"
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
		Name: "swarm_service_desired_replicas",
		Help: "Number of desired replicas for swarm services",
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(desiredReplicasGauge)
}

func InitDesiredReplicasGauge(ctx context.Context, cli *client.Client) error {
	services, err := cli.ServiceList(ctx, types.ServiceListOptions{})
	if err != nil {
		return err
	}

	nodes, err := cli.NodeList(ctx, types.NodeListOptions{})
	if err != nil {
		return err
	}

	nodeCount = len(nodes)

	for _, svc := range services {
		metadataCache[svc.ID] = buildMetadata(svc)
		updateServiceReplicasGauge(svc, metadataCache[svc.ID])
	}

	return nil
}

func updateServiceReplicasGauge(svc swarm.Service, metadata serviceMetadata) {
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

	evtCh, errCh := cli.Events(ctx, types.EventsOptions{
		Since:   time.Now().Format(time.RFC3339),
		Filters: filterArgs,
	})

	logrus.Info("Start listening for new Swarm events...")

	for {
		select {
		case err := <-errCh:
			//	@TODO:	auto-reconnect when connection lost
			return err
		case evt := <-evtCh:
			go func(evt events.Message) {
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
			}(evt)
		}
	}
}

func processEvent(ctx context.Context, cli *client.Client, evt events.Message) error {
	if evt.Type == "node" {
		// Re-init desired replicas gauge when a node is added/deleted,
		// to be sure global services have the right number of desired replicas
		switch evt.Action {
		case "create":
			nodeCount = nodeCount + 1

			InitDesiredReplicasGauge(ctx, cli)
		case "remove":
			nodeCount = nodeCount - 1

			InitDesiredReplicasGauge(ctx, cli)
		}

		return nil
	}

	sid := evt.Actor.ID

	if evt.Action == "remove" {
		metadata, ok := metadataCache[sid]
		if !ok {
			return errors.New("no cached metadata found for removed service")
		}

		//	@TODO:	at this point, the vector should be deleted (?)
		setDesiredReplicasGauge(metadata, float64(0))

		// Clean up labels cache as this won't be used anymore
		delete(metadataCache, sid)

		return nil
	}

	svc, _, err := cli.ServiceInspectWithRaw(ctx, sid, types.ServiceInspectOptions{})
	if err != nil {
		return err
	}

	metadataCache[sid] = buildMetadata(svc)
	updateServiceReplicasGauge(svc, metadataCache[sid])

	return nil
}
