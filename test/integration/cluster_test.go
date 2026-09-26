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
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	dockerclient "github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/testenv"
)

// TestCluster_Smoke checks the harness and the exporter's basics end to end: node accounting,
// health, build info, and a global service that runs everywhere without any node pulling an image.
func TestCluster_Smoke(t *testing.T) {
	const stack = "it-smoke"

	everywhere := serviceKey{stack: stack, service: "everywhere"}
	serviceID := deployService(t, stack, everywhere.service, &serviceOpts{global: true})
	baseURL := startExporter(t, []serviceKey{everywhere})
	nodeCount := float64(len(cluster.Nodes))

	_, status, err := httpGet(testCtx(t), baseURL+"/healthz")
	if err != nil || status != http.StatusOK {
		t.Fatalf("/healthz: status %d, err %v; want 200", status, err)
	}

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		tasks, listErr := listTasks(ctx, serviceID)
		if listErr != nil {
			return listErr
		}

		return runsOnEveryNode(runningTasks(tasks))
	})

	eventually(t, 60*time.Second, metricsMatch(
		baseURL,
		stack,
		want(
			metricNodesByState,
			map[string]string{"role": "manager", "availability": "active", "status": "ready"},
			1,
		),
		want(
			metricNodesByState,
			map[string]string{"role": "worker", "availability": "active", "status": "ready"},
			float64(len(cluster.Workers())),
		),
		wantSum(metricNodesByState, nil, nodeCount),
		want(metricHealth, nil, 1),
		wantService(metricDesired, everywhere, nodeCount),
		wantService(metricRunning, everywhere, nodeCount),
		wantService(metricAtDesired, everywhere, 1),
	))

	scraped, err := scrapeMetrics(testCtx(t), baseURL)
	if err != nil {
		t.Fatal(err)
	}

	buildInfo, ok := scraped.value(metricBuildInfo, nil)
	if !ok || buildInfo != 1 {
		t.Fatalf(
			"%s: want exactly one series = 1, got %v",
			metricBuildInfo,
			scraped.find(metricBuildInfo, nil),
		)
	}

	assertServiceImage(t, serviceID, cluster.WorkloadImageID)

	for idx := range cluster.Nodes {
		assertNoImagePulls(t, &cluster.Nodes[idx])
	}
}

// runsOnEveryNode holds when there is exactly one running task on each cluster node.
func runsOnEveryNode(running []taskInfo) error {
	perNode := make(map[string]int, len(running))
	for _, task := range running {
		perNode[task.nodeID]++
	}

	for idx := range cluster.Nodes {
		if perNode[cluster.Nodes[idx].SwarmNodeID] != 1 {
			return fmt.Errorf("want one running task on each of %d nodes, have:\n%s: %w",
				len(cluster.Nodes), describeTasks(running), errNotYet)
		}
	}

	if len(running) != len(cluster.Nodes) {
		return fmt.Errorf(
			"want %d running tasks, have:\n%s: %w",
			len(cluster.Nodes),
			describeTasks(running),
			errNotYet,
		)
	}

	return nil
}

func assertServiceImage(t *testing.T, serviceID, imageID string) {
	t.Helper()

	callCtx, cancel := opCtx(testCtx(t))
	defer cancel()

	inspected, err := swarmClient.ServiceInspect(
		callCtx,
		serviceID,
		dockerclient.ServiceInspectOptions{},
	)
	if err != nil {
		t.Fatalf("inspect service: %v", err)
	}

	got := inspected.Service.Spec.TaskTemplate.ContainerSpec.Image
	if got != imageID {
		t.Fatalf("service image = %q, want the workload image ID %q", got, imageID)
	}
}

// assertNoImagePulls reads the node daemon's image events since the cluster started: there must be
// no pull, and there must be the load that put the workload image there — which also proves the
// event query itself sees the node's history.
func assertNoImagePulls(t *testing.T, node *testenv.Node) {
	t.Helper()

	imageEvents, err := nodeImageEvents(testCtx(t), node, cluster.StartedAt)
	if err != nil {
		t.Fatal(err)
	}

	var pulls, loads []string

	for idx := range imageEvents {
		evt := &imageEvents[idx]

		switch evt.Action { //nolint:exhaustive // only pulls and loads matter here
		case events.ActionPull:
			pulls = append(pulls, evt.Actor.ID)
		case events.ActionLoad:
			loads = append(loads, evt.Actor.ID)
		}
	}

	if len(pulls) > 0 {
		t.Errorf("node %s pulled images since the cluster started: %v", node.Hostname, pulls)
	}

	if len(loads) == 0 {
		t.Errorf(
			"node %s has no image load event since the cluster started; the event query sees nothing",
			node.Hostname,
		)
	}
}

func nodeImageEvents(
	ctx context.Context,
	node *testenv.Node,
	since time.Time,
) ([]events.Message, error) {
	cli, err := node.Client()
	if err != nil {
		return nil, fmt.Errorf("client for %s: %w", node.Hostname, err)
	}
	defer cli.Close()

	callCtx, cancel := opCtx(ctx)
	defer cancel()

	// A bounded window (until now) makes the daemon end the stream with io.EOF once it has
	// replayed the history.
	stream := cli.Events(callCtx, dockerclient.EventsListOptions{
		Since:   strconv.FormatInt(since.Unix(), 10),
		Until:   strconv.FormatInt(time.Now().Unix()+1, 10),
		Filters: make(dockerclient.Filters).Add("type", string(events.ImageEventType)),
	})

	var collected []events.Message

	for {
		select {
		case msg := <-stream.Messages:
			collected = append(collected, msg)
		case streamErr := <-stream.Err:
			if errors.Is(streamErr, io.EOF) {
				return slices.Clip(collected), nil
			}

			return nil, fmt.Errorf("image events of %s: %w", node.Hostname, streamErr)
		}
	}
}
