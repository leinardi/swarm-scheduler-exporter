/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
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

package collector

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

var errSeedTransient = errors.New("transient service list failure")

// seedDocker wraps fakeDocker with a ServiceList that blocks until its context is done when block
// is set, and otherwise fails its first failFirst calls. Every call is reported on calls.
type seedDocker struct {
	*fakeDocker

	failFirst int
	block     bool
	calls     chan struct{}

	mu               sync.Mutex
	serviceListCalls int
}

func newSeedDocker(inner *fakeDocker, failFirst int, block bool) *seedDocker {
	return &seedDocker{
		fakeDocker: inner,
		failFirst:  failFirst,
		block:      block,
		calls:      make(chan struct{}, 16),
	}
}

func (d *seedDocker) ServiceList(
	ctx context.Context,
	options client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.mu.Lock()
	d.serviceListCalls++
	call := d.serviceListCalls
	d.mu.Unlock()

	select {
	case d.calls <- struct{}{}:
	default:
	}

	if d.block {
		<-ctx.Done()

		return client.ServiceListResult{}, fmt.Errorf("blocked service list: %w", ctx.Err())
	}

	if call <= d.failFirst {
		return client.ServiceListResult{}, errSeedTransient
	}

	return d.fakeDocker.ServiceList(ctx, options)
}

func (d *seedDocker) serviceListCallCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.serviceListCalls
}

// runSeed runs SeedWithRetry in a goroutine and returns the channel its error arrives on.
func runSeed(ctx context.Context, dockerClient DockerAPI) <-chan error {
	done := make(chan error, 1)

	go func() {
		_, seedErr := SeedWithRetry(ctx, dockerClient)
		done <- seedErr
	}()

	return done
}

func waitSeedCall(t *testing.T, dockerClient *seedDocker) {
	t.Helper()

	select {
	case <-dockerClient.calls:
	case <-time.After(5 * time.Second):
		t.Fatal("ServiceList was never called")
	}
}

func waitSeedReturn(t *testing.T, done <-chan error) error {
	t.Helper()

	select {
	case seedErr := <-done:
		return seedErr
	case <-time.After(5 * time.Second):
		t.Fatal("SeedWithRetry did not return")

		return nil
	}
}

func TestSeedWithRetry_RetriesThenSucceeds(t *testing.T) {
	resetCollectorState(t)
	desired := installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installNodesByStateGauge(t)

	svc := makeReplicatedService("svc1", "stack", "web", 3)
	dockerClient := newSeedDocker(&fakeDocker{
		services: []swarm.Service{svc},
		nodes:    []swarm.Node{makeSchedulableNode("n1", "h1")},
	}, 1, false)

	ctx := t.Context()

	seedErr := waitSeedReturn(t, runSeed(ctx, dockerClient))
	if seedErr != nil {
		t.Fatalf("SeedWithRetry: %v", seedErr)
	}

	if got := dockerClient.serviceListCallCount(); got != 2 {
		t.Errorf("ServiceList calls = %d, want 2 (one failure, one retry)", got)
	}

	if _, cached := getServiceMetadata("svc1"); !cached {
		t.Error("svc1 metadata not cached after the retried seed")
	}

	if got := testutil.ToFloat64(
		desired.With(serviceLabels("stack", "web", serviceModeReplicated)),
	); got != 3 {
		t.Errorf("desired_replicas = %v, want 3", got)
	}
}

func TestSeedWithRetry_FailedAttemptLeavesNoState(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installNodesByStateGauge(t)

	svc := makeReplicatedService("svc1", "stack", "web", 3)
	dockerClient := &fakeDocker{services: []swarm.Service{svc}, nodeListErr: errSeedTransient}

	_, seedErr := InitDesiredReplicasGauge(context.Background(), dockerClient)
	if !errors.Is(seedErr, errSeedTransient) {
		t.Fatalf("err = %v, want the node list failure", seedErr)
	}

	if _, cached := getServiceMetadata("svc1"); cached {
		t.Error("a seed whose node list failed cached service metadata")
	}
}

func TestSeedWithRetry_CancelDuringBlockedCall(t *testing.T) {
	resetCollectorState(t)

	dockerClient := newSeedDocker(&fakeDocker{}, 0, true)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := runSeed(ctx, dockerClient)
	waitSeedCall(t, dockerClient)
	cancel()

	// Returning within the wait above, well before dockerRequestTimeout, shows the blocked call
	// followed the cancellation rather than its own deadline.
	seedErr := waitSeedReturn(t, done)
	if !errors.Is(seedErr, context.Canceled) {
		t.Errorf("err = %v, want a context.Canceled wrap", seedErr)
	}
}

func TestSeedWithRetry_CancelDuringBackoff(t *testing.T) {
	resetCollectorState(t)

	dockerClient := newSeedDocker(&fakeDocker{}, 1<<30, false)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := runSeed(ctx, dockerClient)
	waitSeedCall(t, dockerClient)
	cancel()

	seedErr := waitSeedReturn(t, done)
	if !errors.Is(seedErr, context.Canceled) {
		t.Errorf("err = %v, want a context.Canceled wrap", seedErr)
	}

	// A backoff that ignored the cancellation would have run a second attempt when it expired.
	if got := dockerClient.serviceListCallCount(); got != 1 {
		t.Errorf("ServiceList calls = %d, want 1: no attempt after cancellation", got)
	}
}

// deadlineDocker wraps fakeDocker and records every request/response call made with a context
// that has no deadline, or one further away than dockerRequestTimeout.
type deadlineDocker struct {
	*fakeDocker

	mu        sync.Mutex
	unbounded []string
}

func (d *deadlineDocker) NodeList(
	ctx context.Context,
	options client.NodeListOptions,
) (client.NodeListResult, error) {
	d.check(ctx, "NodeList")

	return d.fakeDocker.NodeList(ctx, options)
}

func (d *deadlineDocker) ServiceList(
	ctx context.Context,
	options client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.check(ctx, "ServiceList")

	return d.fakeDocker.ServiceList(ctx, options)
}

func (d *deadlineDocker) ServiceInspect(
	ctx context.Context,
	serviceID string,
	options client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	d.check(ctx, "ServiceInspect")

	return d.fakeDocker.ServiceInspect(ctx, serviceID, options)
}

func (d *deadlineDocker) TaskList(
	ctx context.Context,
	options client.TaskListOptions,
) (client.TaskListResult, error) {
	d.check(ctx, "TaskList")

	return d.fakeDocker.TaskList(ctx, options)
}

func (d *deadlineDocker) check(ctx context.Context, method string) {
	deadline, hasDeadline := ctx.Deadline()
	if hasDeadline && time.Until(deadline) <= dockerRequestTimeout {
		return
	}

	d.mu.Lock()
	d.unbounded = append(d.unbounded, method)
	d.mu.Unlock()
}

func TestDockerCalls_HaveDeadlines(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installNodesByStateGauge(t)
	installReplicasStateGauges(t)

	global := makeGlobalService("glb1", "stack", "agent")
	replicated := makeReplicatedService("svc1", "stack", "web", 2)
	uncached := makeReplicatedService("svc2", "other", "late", 1)

	// No nodes: the seed caches an empty node list, so the global service falls back to the
	// NodeList-based eligibility helpers.
	dockerClient := &deadlineDocker{fakeDocker: &fakeDocker{
		services: []swarm.Service{global, replicated},
		serviceByID: map[string]swarm.Service{
			"glb1": global,
			"svc1": replicated,
			"svc2": uncached,
		},
		tasks: []swarm.Task{{ID: "t1", ServiceID: "svc2", Slot: 1}},
	}}

	root := context.Background()

	_, seedErr := InitDesiredReplicasGauge(root, dockerClient)
	if seedErr != nil {
		t.Fatalf("seed: %v", seedErr)
	}

	serviceEvent := events.Message{
		Type:   "service",
		Action: events.ActionUpdate,
		Actor:  events.Actor{ID: "svc1"},
	}

	processErr := processEvent(root, dockerClient, &serviceEvent)
	if processErr != nil {
		t.Fatalf("service event: %v", processErr)
	}

	nodeEvent := events.Message{
		Type:   "node",
		Action: events.ActionUpdate,
		Actor:  events.Actor{ID: "n1"},
	}

	processErr = processEvent(root, dockerClient, &nodeEvent)
	if processErr != nil {
		t.Fatalf("node event: %v", processErr)
	}

	// svc2 is not cached, so the poll takes the slow path and inspects it.
	_, pollErr := PollReplicasState(root, dockerClient)
	if pollErr != nil {
		t.Fatalf("poll: %v", pollErr)
	}

	updateErr := UpdateNodesByState(root, dockerClient)
	if updateErr != nil {
		t.Fatalf("nodes by state: %v", updateErr)
	}

	dockerClient.mu.Lock()
	defer dockerClient.mu.Unlock()

	if len(dockerClient.unbounded) != 0 {
		t.Errorf("Docker calls without a request deadline: %v", dockerClient.unbounded)
	}
}
