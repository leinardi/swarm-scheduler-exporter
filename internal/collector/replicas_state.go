package collector

import (
	"context"
	"fmt"
	"sort"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/swarm"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
)

var replicasStateGauge *prometheus.GaugeVec

func ConfigureReplicasStateGauge() {
	baseLabels := append([]string{
		"stack",
		"service",
		"service_mode",
		"state",
	}, customLabels...)

	replicasStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   "",
		Subsystem:   "",
		Name:        "swarm_service_replicas_state",
		Help:        "State of service replicas",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))
	prometheus.MustRegister(replicasStateGauge)
}

type taskCounter struct {
	states map[string]float64
	labels prometheus.Labels
}

func (tctr taskCounter) inc(state string) {
	tctr.states[state]++
}

type serviceCounter map[string]map[string]taskCounter

func (sctr serviceCounter) get(labels prometheus.Labels) taskCounter {
	service := labels["service"]
	version := labels["service_version"]

	if _, ok := sctr[service]; !ok {
		sctr[service] = map[string]taskCounter{}
	}

	if _, ok := sctr[service][version]; !ok {
		sctr[service][version] = newTaskCounter(labels)
	}

	return sctr[service][version]
}

func newTaskCounter(labels map[string]string) taskCounter {
	return taskCounter{
		labels: labels,
		states: map[string]float64{
			string(swarm.TaskStateNew):       0,
			string(swarm.TaskStateAllocated): 0,
			string(swarm.TaskStatePending):   0,
			string(swarm.TaskStateAssigned):  0,
			string(swarm.TaskStateAccepted):  0,
			string(swarm.TaskStatePreparing): 0,
			string(swarm.TaskStateReady):     0,
			string(swarm.TaskStateStarting):  0,
			string(swarm.TaskStateRunning):   0,
			string(swarm.TaskStateComplete):  0,
			string(swarm.TaskStateShutdown):  0,
			string(swarm.TaskStateFailed):    0,
			string(swarm.TaskStateRejected):  0,
			string(swarm.TaskStateRemove):    0,
			string(swarm.TaskStateOrphaned):  0,
		},
	}
}

func PollReplicasState(ctx context.Context, cli *client.Client) (serviceCounter, error) {
	tasks, err := cli.TaskList(ctx, types.TaskListOptions{
		Filters: filters.Args{},
	})
	if err != nil {
		return serviceCounter{}, fmt.Errorf("task list: %w", err)
	}

	sort.Slice(tasks, func(indexA int, indexB int) bool {
		return tasks[indexA].ServiceID == tasks[indexB].ServiceID &&
			tasks[indexA].Slot == tasks[indexB].Slot &&
			tasks[indexA].Version.Index > tasks[indexB].Version.Index ||
			tasks[indexA].ServiceID == tasks[indexB].ServiceID &&
				tasks[indexA].Slot < tasks[indexB].Slot ||
			tasks[indexA].ServiceID < tasks[indexB].ServiceID
	})

	replicas := make(serviceCounter)

	for i := range tasks { // avoid copying large struct
		task := &tasks[i]

		// Skip tasks whose service does not exist anymore
		labels, err := getServiceLabels(ctx, cli, task)
		if client.IsErrNotFound(err) {
			continue
		} else if err != nil {
			return serviceCounter{}, fmt.Errorf("labels for service %s: %w", task.ServiceID, err)
		}

		replicas.get(labels).inc(string(task.Status.State))
	}

	return replicas, nil
}

func getServiceLabels(
	ctx context.Context,
	cli *client.Client,
	task *swarm.Task,
) (prometheus.Labels, error) {
	sid := task.ServiceID

	if _, ok := metadataCache[sid]; !ok {
		svc, _, err := cli.ServiceInspectWithRaw(ctx, sid, types.ServiceInspectOptions{
			InsertDefaults: false,
		})
		if err != nil {
			return map[string]string{}, fmt.Errorf("service inspect %s: %w", sid, err)
		}

		svcPtr := &svc
		metadataCache[sid] = buildMetadata(svcPtr)
	}

	labels := prometheus.Labels{
		"stack":        metadataCache[sid].stack,
		"service":      metadataCache[sid].service,
		"service_mode": metadataCache[sid].serviceMode,
	}

	for k, v := range metadataCache[sid].customLabels {
		labels[k] = v
	}

	return labels, nil
}

func UpdateReplicasStateGauge(sctr serviceCounter) {
	for _, versions := range sctr {
		for _, tctr := range versions {
			for state, ctr := range tctr.states {
				labels := labelutil.SanitizeMetricLabels(tctr.labels)
				labels["state"] = state
				replicasStateGauge.With(labels).Set(ctr)
			}
		}
	}
}
