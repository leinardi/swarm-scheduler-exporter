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
	"maps"
	"slices"
	"strings"
	"testing"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/swarm"
	dockerclient "github.com/moby/moby/client"
)

const (
	stackNamespaceLabel = "com.docker.stack.namespace"

	// taskReapTimeout bounds the wait for a removed service's tasks to disappear.
	taskReapTimeout = 60 * time.Second
	// nodesSettleTimeout bounds the wait for every node to be Ready and Active again.
	nodesSettleTimeout = 90 * time.Second //nolint:unused // shared scenario helper, not every scenario uses it
)

// stopFast is the default workload: it idles, and exits promptly on SIGTERM. A bare sleep would run
// as PID 1 with no SIGTERM handler and hold every stop (scale down, update, removal) for the whole
// grace period.
var stopFast = []string{"sh", "-c", "trap 'exit 0' TERM; while true; do sleep 1; done"}

// serviceOpts are the knobs of deployService. The zero value is a replicated service with one
// replica running stopFast.
type serviceOpts struct {
	global           bool
	replicas         uint64 // replicated only; 0 means 1 unless zeroReplicas is set
	zeroReplicas     bool
	command          []string // defaults to stopFast
	env              []string
	constraints      []string
	restartCondition swarm.RestartPolicyCondition // Swarm's default (any) when empty
	updateConfig     *swarm.UpdateConfig
	labels           map[string]string // extra service labels, e.g. for -label
}

// serviceKey names a service the way the exporter labels it: stack plus the service name with the
// stack prefix stripped.
type serviceKey struct {
	stack   string
	service string
}

// deployService creates <stack>_<name> the way docker stack deploy would — the stack namespace
// label on both the service and its containers, which is the only place the exporter reads stack
// and service from — running the workload image by ID. It returns the service ID and removes the
// service, waiting for its tasks to be reaped, when the test ends.
func deployService(t *testing.T, stack, name string, opts *serviceOpts) string {
	t.Helper()

	ctx := testCtx(t)
	spec := serviceSpec(stack, name, opts)

	callCtx, cancel := opCtx(ctx)
	defer cancel()

	// QueryRegistry false plus an image referenced by ID: nothing, neither the manager nor a
	// node's executor, ever contacts a registry.
	created, err := swarmClient.ServiceCreate(
		callCtx,
		dockerclient.ServiceCreateOptions{Spec: spec, QueryRegistry: false},
	)
	if err != nil {
		t.Fatalf("create service %s: %v", spec.Name, err)
	}

	t.Cleanup(func() {
		cleanupCtx, cancelCleanup := teardownCtx()
		defer cancelCleanup()

		removeErr := removeServiceAndWait(cleanupCtx, created.ID)
		if removeErr != nil {
			t.Errorf("cleanup: %v", removeErr)
		}
	})

	return created.ID
}

func serviceSpec(stack, name string, opts *serviceOpts) swarm.ServiceSpec {
	labels := map[string]string{stackNamespaceLabel: stack}
	maps.Copy(labels, opts.labels)

	command := opts.command
	if command == nil {
		command = stopFast
	}

	stopGrace := time.Second

	spec := swarm.ServiceSpec{
		Annotations: swarm.Annotations{Name: stack + "_" + name, Labels: labels},
		TaskTemplate: swarm.TaskSpec{
			ContainerSpec: &swarm.ContainerSpec{
				Image:           cluster.WorkloadImageID,
				Command:         command,
				Env:             opts.env,
				Labels:          map[string]string{stackNamespaceLabel: stack},
				StopGracePeriod: &stopGrace,
			},
			Placement: &swarm.Placement{Constraints: opts.constraints},
		},
		UpdateConfig: opts.updateConfig,
	}

	if opts.restartCondition != "" {
		spec.TaskTemplate.RestartPolicy = &swarm.RestartPolicy{Condition: opts.restartCondition}
	}

	if opts.global {
		spec.Mode = swarm.ServiceMode{Global: &swarm.GlobalService{}}

		return spec
	}

	replicas := opts.replicas
	if replicas == 0 && !opts.zeroReplicas {
		replicas = 1
	}

	spec.Mode = swarm.ServiceMode{Replicated: &swarm.ReplicatedService{Replicas: &replicas}}

	return spec
}

// updateService applies mutate to the service's current spec and submits it.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func updateService(t *testing.T, serviceID string, mutate func(*swarm.ServiceSpec)) {
	t.Helper()

	ctx := testCtx(t)

	callCtx, cancel := opCtx(ctx)
	defer cancel()

	inspected, err := swarmClient.ServiceInspect(
		callCtx,
		serviceID,
		dockerclient.ServiceInspectOptions{},
	)
	if err != nil {
		t.Fatalf("inspect service %s: %v", serviceID, err)
	}

	spec := inspected.Service.Spec
	mutate(&spec)

	_, err = swarmClient.ServiceUpdate(callCtx, serviceID, dockerclient.ServiceUpdateOptions{
		Version:       inspected.Service.Version,
		Spec:          spec,
		QueryRegistry: false,
	})
	if err != nil {
		t.Fatalf("update service %s: %v", serviceID, err)
	}
}

// scaleService sets a replicated service's replica count.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func scaleService(t *testing.T, serviceID string, replicas uint64) {
	t.Helper()

	updateService(t, serviceID, func(spec *swarm.ServiceSpec) {
		spec.Mode.Replicated.Replicas = &replicas
	})
}

// removeService removes the service now, inside the test, and waits for its tasks to be reaped.
// The cleanup registered by deployService then finds nothing left to do.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func removeService(t *testing.T, serviceID string) {
	t.Helper()

	err := removeServiceAndWait(testCtx(t), serviceID)
	if err != nil {
		t.Fatal(err)
	}
}

func removeServiceAndWait(ctx context.Context, serviceID string) error {
	callCtx, cancel := opCtx(ctx)
	defer cancel()

	_, err := swarmClient.ServiceRemove(callCtx, serviceID, dockerclient.ServiceRemoveOptions{})
	if err != nil && !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("remove service %s: %w", serviceID, err)
	}

	return pollUntil(ctx, taskReapTimeout, func() error {
		left, countErr := countOrphanTasks(ctx, serviceID)
		if countErr != nil {
			return countErr
		}

		if left > 0 {
			return fmt.Errorf(
				"removed service %s still has %d tasks: %w",
				serviceID,
				left,
				errNotYet,
			)
		}

		return nil
	})
}

// countOrphanTasks counts the tasks still left by a service. It lists every task: once the service
// is gone, the manager refuses a task filter that names it.
func countOrphanTasks(ctx context.Context, serviceID string) (int, error) {
	callCtx, cancel := opCtx(ctx)
	defer cancel()

	listed, err := swarmClient.TaskList(callCtx, dockerclient.TaskListOptions{})
	if err != nil {
		return 0, fmt.Errorf("list tasks: %w", err)
	}

	left := 0

	for idx := range listed.Items {
		if listed.Items[idx].ServiceID == serviceID {
			left++
		}
	}

	return left, nil
}

// taskInfo is where one task sits and what state it is in.
type taskInfo struct {
	id           string
	slot         int
	nodeID       string
	state        swarm.TaskState
	desiredState swarm.TaskState
	createdAt    time.Time
}

func (ti taskInfo) String() string {
	return fmt.Sprintf(
		"slot=%d node=%s state=%s desired=%s task=%s",
		ti.slot,
		ti.nodeID,
		ti.state,
		ti.desiredState,
		ti.id,
	)
}

// taskPlacement returns every task of the service — retired ones included — oldest first.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func taskPlacement(t *testing.T, serviceID string) []taskInfo {
	t.Helper()

	tasks, err := listTasks(testCtx(t), serviceID)
	if err != nil {
		t.Fatal(err)
	}

	return tasks
}

func listTasks(ctx context.Context, serviceID string) ([]taskInfo, error) {
	callCtx, cancel := opCtx(ctx)
	defer cancel()

	listed, err := swarmClient.TaskList(callCtx, dockerclient.TaskListOptions{
		Filters: make(dockerclient.Filters).Add("service", serviceID),
	})
	if err != nil {
		return nil, fmt.Errorf("list tasks of %s: %w", serviceID, err)
	}

	tasks := make([]taskInfo, 0, len(listed.Items))

	for idx := range listed.Items {
		task := &listed.Items[idx]
		tasks = append(tasks, taskInfo{
			id:           task.ID,
			slot:         task.Slot,
			nodeID:       task.NodeID,
			state:        task.Status.State,
			desiredState: task.DesiredState,
			createdAt:    task.CreatedAt,
		})
	}

	slices.SortFunc(
		tasks,
		func(left, right taskInfo) int { return left.createdAt.Compare(right.createdAt) },
	)

	return tasks, nil
}

// runningTasks keeps the tasks that are running and meant to be.
func runningTasks(tasks []taskInfo) []taskInfo {
	return slices.DeleteFunc(slices.Clone(tasks), func(task taskInfo) bool {
		return task.state != swarm.TaskStateRunning || task.desiredState != swarm.TaskStateRunning
	})
}

func describeTasks(tasks []taskInfo) string {
	lines := make([]string, 0, len(tasks))
	for _, task := range tasks {
		lines = append(lines, "  "+task.String())
	}

	return strings.Join(lines, "\n")
}

// --- Nodes ---

// updateNode applies mutate to the node's current spec and submits it.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func updateNode(t *testing.T, nodeID string, mutate func(*swarm.NodeSpec)) {
	t.Helper()

	err := updateNodeSpec(testCtx(t), nodeID, mutate)
	if err != nil {
		t.Fatal(err)
	}
}

//nolint:unused // shared scenario helper, not every scenario uses it
func updateNodeSpec(ctx context.Context, nodeID string, mutate func(*swarm.NodeSpec)) error {
	callCtx, cancel := opCtx(ctx)
	defer cancel()

	inspected, err := swarmClient.NodeInspect(callCtx, nodeID, dockerclient.NodeInspectOptions{})
	if err != nil {
		return fmt.Errorf("inspect node %s: %w", nodeID, err)
	}

	spec := inspected.Node.Spec
	mutate(&spec)

	_, err = swarmClient.NodeUpdate(
		callCtx,
		nodeID,
		dockerclient.NodeUpdateOptions{Version: inspected.Node.Version, Spec: spec},
	)
	if err != nil {
		return fmt.Errorf("update node %s: %w", nodeID, err)
	}

	return nil
}

// waitNodesReadyActive waits until every cluster node is Ready and Active, the state disruptive
// tests must leave the cluster in.
//
//nolint:unused // shared scenario helper, not every scenario uses it
func waitNodesReadyActive(ctx context.Context) error {
	return pollUntil(ctx, nodesSettleTimeout, func() error {
		callCtx, cancel := opCtx(ctx)
		defer cancel()

		listed, err := swarmClient.NodeList(callCtx, dockerclient.NodeListOptions{})
		if err != nil {
			return fmt.Errorf("list nodes: %w", err)
		}

		var notSettled []string

		for idx := range listed.Items {
			node := &listed.Items[idx]
			if node.Status.State != swarm.NodeStateReady ||
				node.Spec.Availability != swarm.NodeAvailabilityActive {
				notSettled = append(
					notSettled,
					fmt.Sprintf(
						"%s=%s/%s",
						node.Description.Hostname,
						node.Status.State,
						node.Spec.Availability,
					),
				)
			}
		}

		if len(listed.Items) != len(cluster.Nodes) || len(notSettled) > 0 {
			return fmt.Errorf("%d of %d nodes listed, not ready+active: %v: %w",
				len(listed.Items), len(cluster.Nodes), notSettled, errNotYet)
		}

		return nil
	})
}

// pollUntil calls check every pollInterval until it returns nil, timeout elapses or ctx ends, and
// returns check's last error otherwise. It is the non-test-bound sibling of eventually, for
// cleanup code that must not call t.Fatal.
func pollUntil(ctx context.Context, timeout time.Duration, check func() error) error {
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()

	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()

	for {
		lastErr := check()
		if lastErr == nil {
			return nil
		}

		select {
		case <-ctx.Done():
			return errors.Join(lastErr, ctx.Err())
		case <-deadline.C:
			return fmt.Errorf("still failing after %s: %w", timeout, lastErr)
		case <-ticker.C:
		}
	}
}
