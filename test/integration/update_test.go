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

	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/swarm"
	dockerclient "github.com/moby/moby/client"
)

// TestUpdate_Completed rolls a new env out to a replicated service and checks the update is
// reported completed, with both timestamps set, and the service back at its desired count.
func TestUpdate_Completed(t *testing.T) {
	const stack = "it-update"

	web := serviceKey{stack: stack, service: "web"}
	serviceID := deployService(t, stack, web.service, &serviceOpts{
		replicas: 2,
		updateConfig: &swarm.UpdateConfig{
			Parallelism: 1,
			Monitor:     time.Second,
			Order:       swarm.UpdateOrderStopFirst,
		},
	})
	baseURL := startExporter(t, []serviceKey{web})

	eventually(t, 60*time.Second, metricsMatch(baseURL, stack,
		wantService(metricRunning, web, 2),
		wantService(metricAtDesired, web, 1),
	))

	updateService(t, serviceID, func(spec *swarm.ServiceSpec) {
		spec.TaskTemplate.ContainerSpec.Env = append(
			spec.TaskTemplate.ContainerSpec.Env,
			"IT_ROLLOUT=2",
		)
	})

	// Before any update the exporter already reports "completed", with zero timestamps; non-zero
	// timestamps are what tie the state to this update.
	eventually(t, 90*time.Second, metricsMatch(baseURL, stack,
		want(metricUpdateState, updateStateLabels(web, "completed"), 1),
		want(metricUpdateState, updateStateLabels(web, "updating"), 0),
		wantPositive(metricUpdateStarted, web.labels()),
		wantPositive(metricUpdateComplete, web.labels()),
		wantService(metricRunning, web, 2),
		wantService(metricAtDesired, web, 1),
	))

	assertUpdateState(t, serviceID, swarm.UpdateStateCompleted)
}

// TestUpdate_StartFirstFailure starts a start-first update whose new tasks exit 1, with
// FailureAction pause. The update pauses, the old task keeps serving, and newer tasks that Swarm
// has given up on sit in the same slot, next to a fresh retry that is not running. The exporter
// must count the old, running task and so report the service at its desired count, not below it.
//
// The service has a healthcheck so the new tasks fail before they ever count as running: a task
// with a healthcheck stays "starting" until its first check passes, and the new command exits
// before that. Without it the new task is briefly running, which is enough for start-first to
// stop the old task, and the service really does drop to zero.
func TestUpdate_StartFirstFailure(t *testing.T) {
	const stack = "it-update-fail"

	web := serviceKey{stack: stack, service: "web"}
	serviceID := deployService(t, stack, web.service, &serviceOpts{
		replicas: 1,
		healthcheck: &container.HealthConfig{
			Test:     []string{"CMD", "true"},
			Interval: time.Second,
			Timeout:  time.Second,
			Retries:  1,
		},
		updateConfig: &swarm.UpdateConfig{
			Parallelism:   1,
			Monitor:       2 * time.Second,
			Order:         swarm.UpdateOrderStartFirst,
			FailureAction: swarm.UpdateFailureActionPause,
		},
	})
	baseURL := startExporter(t, []serviceKey{web})

	eventually(t, 60*time.Second, metricsMatch(baseURL, stack,
		wantService(metricRunning, web, 1),
		wantService(metricAtDesired, web, 1),
	))

	oldTasks := runningTasks(taskPlacement(t, serviceID))
	if len(oldTasks) != 1 {
		t.Fatalf("want one running task before the update, have:\n%s", describeTasks(oldTasks))
	}

	oldTask := oldTasks[0]

	updateService(t, serviceID, func(spec *swarm.ServiceSpec) {
		spec.TaskTemplate.ContainerSpec.Command = []string{"sh", "-c", "exit 1"}
	})

	eventually(t, 90*time.Second, func(ctx context.Context) error {
		return checkUpdateState(ctx, serviceID, swarm.UpdateStatePaused)
	})

	placement := taskPlacement(t, serviceID)
	t.Logf("tasks after the paused update:\n%s", describeTasks(placement))

	// The slot must hold both the old task, still serving, and a newer one Swarm retired: that
	// pairing is what makes the choice of task per slot matter.
	var serving, retiredNewer bool

	for _, task := range placement {
		switch {
		case task.id == oldTask.id:
			serving = task.state == swarm.TaskStateRunning &&
				task.desiredState == swarm.TaskStateRunning
		case task.slot == oldTask.slot && task.createdAt.After(oldTask.createdAt) && task.desiredState != swarm.TaskStateRunning:
			retiredNewer = true
		}
	}

	if !serving || !retiredNewer {
		t.Fatalf(
			"want the old task serving (%v) and a retired newer task in slot %d (%v), have:\n%s",
			serving,
			oldTask.slot,
			retiredNewer,
			describeTasks(placement),
		)
	}

	eventually(t, 60*time.Second, metricsMatch(baseURL, stack,
		want(metricUpdateState, updateStateLabels(web, "paused"), 1),
		wantService(metricDesired, web, 1),
		wantService(metricRunning, web, 1),
		wantService(metricAtDesired, web, 1),
	))
}

func updateStateLabels(svc serviceKey, state string) map[string]string {
	labels := svc.labels()
	labels["state"] = state

	return labels
}

// assertUpdateState checks Swarm's own view of the service's last update.
func assertUpdateState(t *testing.T, serviceID string, want swarm.UpdateState) {
	t.Helper()

	err := checkUpdateState(testCtx(t), serviceID, want)
	if err != nil {
		t.Fatal(err)
	}
}

func checkUpdateState(ctx context.Context, serviceID string, want swarm.UpdateState) error {
	callCtx, cancel := opCtx(ctx)
	defer cancel()

	inspected, err := swarmClient.ServiceInspect(
		callCtx,
		serviceID,
		dockerclient.ServiceInspectOptions{},
	)
	if err != nil {
		return fmt.Errorf("inspect service %s: %w", serviceID, err)
	}

	status := inspected.Service.UpdateStatus
	if status == nil || status.State != want {
		return fmt.Errorf(
			"swarm update status of %s is %+v, want %s: %w",
			serviceID,
			status,
			want,
			errNotYet,
		)
	}

	return nil
}
