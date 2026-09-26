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
	"fmt"
	"testing"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	dockerclient "github.com/moby/moby/client"
)

// containerAbsencePolls is how many consecutive exporter polls must leave the task container out.
const containerAbsencePolls = 3

// TestContainers_OptIn checks the opt-in container metrics against the manager's daemon: a plain
// container is reported with -containers, while a Swarm task container on the same daemon is
// reported only when -containers-include-swarm is set as well.
func TestContainers_OptIn(t *testing.T) {
	const (
		stack     = "it-containers"
		plainName = "it-plain"
	)

	pinned := serviceKey{stack: stack, service: "pinned"}
	serviceID := deployService(
		t,
		stack,
		pinned.service,
		&serviceOpts{constraints: []string{"node.role==manager"}},
	)

	// The exporter only sees the daemon it talks to — the manager's — so the task must run there
	// before its absence or presence means anything.
	eventually(t, 60*time.Second, func(ctx context.Context) error {
		tasks, err := listTasks(ctx, serviceID)
		if err != nil {
			return err
		}

		running := runningTasks(tasks)
		if len(running) != 1 || running[0].nodeID != cluster.Manager.SwarmNodeID {
			return fmt.Errorf("want one running task on the manager %s, have:\n%s: %w",
				cluster.Manager.SwarmNodeID, describeTasks(tasks), errNotYet)
		}

		return nil
	})

	runPlainContainer(t, plainName)

	plain := map[string]string{"container": plainName, "orchestrator": "none", "state": "running"}
	task := map[string]string{
		labelStack:     stack,
		labelService:   pinned.service,
		"orchestrator": "swarm",
	}
	taskRunning := map[string]string{
		labelStack:     stack,
		labelService:   pinned.service,
		"orchestrator": "swarm",
		"state":        "running",
	}

	// Both containers exist before the exporter starts, so every poll that reports the plain one
	// also saw the task container, and must have skipped it.
	defaultURL := startExporter(t, []serviceKey{pinned}, "-containers")

	eventually(
		t,
		60*time.Second,
		metricsMatch(defaultURL, stack, want(metricContainerState, plain, 1)),
	)

	// Absence has to hold, not just be seen once: the exporter resets the container gauge and
	// re-adds its series one by one, so a scrape landing mid-rewrite can miss a container that
	// the exporter does report.
	consistently(
		t,
		defaultURL,
		containerAbsencePolls,
		metricsMatch(defaultURL, stack, wantAbsent(metricContainerState, task)),
	)

	includeURL := startExporter(t, []serviceKey{pinned}, "-containers", "-containers-include-swarm")

	eventually(t, 60*time.Second, metricsMatch(includeURL, stack,
		want(metricContainerState, plain, 1),
		want(metricContainerState, taskRunning, 1),
	))
}

// runPlainContainer starts a container outside Swarm on the manager's daemon and removes it when
// the test ends.
func runPlainContainer(t *testing.T, name string) {
	t.Helper()

	callCtx, cancel := opCtx(testCtx(t))
	defer cancel()

	created, err := swarmClient.ContainerCreate(callCtx, dockerclient.ContainerCreateOptions{
		Config: &container.Config{Image: cluster.WorkloadImageID, Cmd: stopFast},
		Name:   name,
	})
	if err != nil {
		t.Fatalf("create container %s: %v", name, err)
	}

	t.Cleanup(func() {
		cleanupCtx, cancelCleanup := teardownCtx()
		defer cancelCleanup()

		_, rmErr := swarmClient.ContainerRemove(
			cleanupCtx,
			created.ID,
			dockerclient.ContainerRemoveOptions{Force: true},
		)
		if rmErr != nil && !cerrdefs.IsNotFound(rmErr) {
			t.Errorf("cleanup: remove container %s: %v", name, rmErr)
		}
	})

	_, err = swarmClient.ContainerStart(callCtx, created.ID, dockerclient.ContainerStartOptions{})
	if err != nil {
		t.Fatalf("start container %s: %v", name, err)
	}
}
