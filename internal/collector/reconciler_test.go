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

	"github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

var (
	errListUnavailable    = errors.New("list unavailable")
	errInspectUnavailable = errors.New("inspect unavailable")
)

// reconcilerDocker is a DockerAPI whose state a test changes while the reconciler runs. A service
// in services is also what ServiceInspect returns for its ID; any other ID is not found unless
// inspectErr names it.
type reconcilerDocker struct {
	mu sync.Mutex

	services       []swarm.Service
	nodes          []swarm.Node
	serviceListErr error
	nodeListErr    error
	inspectErr     map[string]error

	// afterServiceList runs after ServiceList has built its result, before it returns.
	afterServiceList func()

	serviceListCalls int
	nodeListCalls    int
	inspectCalls     map[string]int
}

var _ DockerAPI = (*reconcilerDocker)(nil)

func newReconcilerDocker(services []swarm.Service, nodes []swarm.Node) *reconcilerDocker {
	return &reconcilerDocker{
		services:     services,
		nodes:        nodes,
		inspectErr:   make(map[string]error),
		inspectCalls: make(map[string]int),
	}
}

func (d *reconcilerDocker) NodeList(
	context.Context,
	client.NodeListOptions,
) (client.NodeListResult, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.nodeListCalls++
	if d.nodeListErr != nil {
		return client.NodeListResult{}, d.nodeListErr
	}

	return client.NodeListResult{Items: append([]swarm.Node(nil), d.nodes...)}, nil
}

func (d *reconcilerDocker) ServiceList(
	context.Context,
	client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.mu.Lock()
	d.serviceListCalls++
	listErr := d.serviceListErr
	services := append([]swarm.Service(nil), d.services...)
	hook := d.afterServiceList
	d.mu.Unlock()

	if listErr != nil {
		return client.ServiceListResult{}, listErr
	}

	if hook != nil {
		hook()
	}

	return client.ServiceListResult{Items: services}, nil
}

func (d *reconcilerDocker) ServiceInspect(
	_ context.Context,
	serviceID string,
	_ client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.inspectCalls[serviceID]++

	if inspectErr, failing := d.inspectErr[serviceID]; failing {
		return client.ServiceInspectResult{}, inspectErr
	}

	for index := range d.services {
		if d.services[index].ID == serviceID {
			return client.ServiceInspectResult{Service: d.services[index]}, nil
		}
	}

	return client.ServiceInspectResult{}, fmt.Errorf(
		"service %s: %w",
		serviceID,
		errdefs.ErrNotFound,
	)
}

func (*reconcilerDocker) TaskList(
	context.Context,
	client.TaskListOptions,
) (client.TaskListResult, error) {
	return client.TaskListResult{}, nil
}

func (*reconcilerDocker) ContainerList(
	context.Context,
	client.ContainerListOptions,
) (client.ContainerListResult, error) {
	return client.ContainerListResult{Items: []container.Summary{}}, nil
}

func (*reconcilerDocker) ContainerInspect(
	context.Context,
	string,
	client.ContainerInspectOptions,
) (client.ContainerInspectResult, error) {
	return client.ContainerInspectResult{}, nil
}

func (*reconcilerDocker) Events(context.Context, client.EventsListOptions) client.EventsResult {
	messages := make(chan events.Message)
	close(messages)

	return client.EventsResult{Messages: messages, Err: make(chan error, 1)}
}

func (d *reconcilerDocker) setServices(services ...swarm.Service) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.services = services
}

func (d *reconcilerDocker) setNodes(nodes ...swarm.Node) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.nodes = nodes
}

func (d *reconcilerDocker) setListErrors(serviceListErr, nodeListErr error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.serviceListErr = serviceListErr
	d.nodeListErr = nodeListErr
}

func (d *reconcilerDocker) setInspectErr(serviceID string, inspectErr error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	if inspectErr == nil {
		delete(d.inspectErr, serviceID)

		return
	}

	d.inspectErr[serviceID] = inspectErr
}

func (d *reconcilerDocker) serviceListCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.serviceListCalls
}

func (d *reconcilerDocker) inspectCount(serviceID string) int {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.inspectCalls[serviceID]
}

// newTestReconciler returns a reconciler on dockerClient with the gauges installed and short
// delays, and restores the package caches and the active reconciler after the test.
func newTestReconciler(t *testing.T, dockerClient DockerAPI) *Reconciler {
	t.Helper()

	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installNodesByStateGauge(t)

	previous := activeReconciler.Load()

	t.Cleanup(func() { activeReconciler.Store(previous) })

	reconciler := NewReconciler(dockerClient)
	reconciler.retryDelays = []time.Duration{
		10 * time.Millisecond,
		10 * time.Millisecond,
		10 * time.Millisecond,
	}
	reconciler.backoffInitial = 20 * time.Millisecond
	reconciler.backoffMax = 80 * time.Millisecond
	reconciler.resyncBackoff = reconciler.backoffInitial
	reconciler.nodesBackoff = reconciler.backoffInitial

	return reconciler
}

// runReconciler runs reconciler.Run until the test ends, then restores the active reconciler Run
// replaced.
func runReconciler(t *testing.T, reconciler *Reconciler) {
	t.Helper()

	previous := activeReconciler.Load()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})

	go func() {
		reconciler.Run(ctx)
		close(done)
	}()

	t.Cleanup(func() {
		cancel()
		<-done
		activeReconciler.Store(previous)
	})
}

// eventually polls cond until it holds, failing the test with what if it does not within 5s.
func eventually(t *testing.T, what string, cond func() bool) {
	t.Helper()

	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()

	ticker := time.NewTicker(5 * time.Millisecond)
	defer ticker.Stop()

	for !cond() {
		select {
		case <-deadline.C:
			t.Fatalf("timed out waiting for %s", what)
		case <-ticker.C:
		}
	}
}

func serviceEvent(serviceID string, action events.Action) *events.Message {
	return &events.Message{
		Type:   events.ServiceEventType,
		Action: action,
		Actor:  events.Actor{ID: serviceID},
	}
}

func nodeEvent(nodeID string) *events.Message {
	return &events.Message{
		Type:   events.NodeEventType,
		Action: events.ActionUpdate,
		Actor:  events.Actor{ID: nodeID},
	}
}

// completeFirstResync runs the first resync directly and fails the test if it did not complete.
func completeFirstResync(t *testing.T, reconciler *Reconciler) {
	t.Helper()

	reconciler.resync(context.Background())

	_, completed, _ := reconciler.resyncState()
	if completed == 0 {
		t.Fatal("first resync did not complete")
	}
}

func desiredSeries(t *testing.T) int {
	t.Helper()

	return testutil.CollectAndCount(desiredReplicasGauge)
}

func pendingLengths(reconciler *Reconciler) (setLen, orderLen int) {
	reconciler.mu.Lock()
	defer reconciler.mu.Unlock()

	return len(reconciler.pendingSet), len(reconciler.pendingOrder)
}

// ---- Startup ----

func TestReconciler_FirstResyncSeedsCachesAndSignalsReady(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 3)
	agent := makeGlobalService("glb1", "stack", "agent")
	dockerClient := newReconcilerDocker(
		[]swarm.Service{web, agent},
		[]swarm.Node{makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2")},
	)
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)

	select {
	case <-reconciler.Ready():
	case <-time.After(5 * time.Second):
		t.Fatal("Ready was never closed")
	}

	if got := testutil.ToFloat64(
		desiredReplicasGauge.With(serviceLabels("stack", "web", serviceModeReplicated)),
	); got != 3 {
		t.Errorf("web desired_replicas = %v, want 3", got)
	}

	if got := testutil.ToFloat64(
		desiredReplicasGauge.With(serviceLabels("stack", "agent", serviceModeGlobal)),
	); got != 2 {
		t.Errorf("agent desired_replicas = %v, want 2 (eligible nodes)", got)
	}

	if len(getCachedNodes()) != 2 {
		t.Errorf("cached nodes = %d, want 2", len(getCachedNodes()))
	}
}

func TestReconciler_FirstResyncRetriesAfterFailure(t *testing.T) {
	dockerClient := newReconcilerDocker(
		[]swarm.Service{makeReplicatedService("svc1", "stack", "web", 1)},
		nil,
	)
	dockerClient.setListErrors(errListUnavailable, nil)
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)

	eventually(t, "a failed first resync", func() bool {
		serviceListCalls := dockerClient.serviceListCount()

		return serviceListCalls > 0
	})

	dockerClient.setListErrors(nil, nil)

	select {
	case <-reconciler.Ready():
	case <-time.After(5 * time.Second):
		t.Fatal("the first resync never completed after the list recovered")
	}

	if _, cached := getServiceMetadata("svc1"); !cached {
		t.Error("svc1 not cached after the retried resync")
	}
}

func TestReconciler_KeysWaitForFirstResync(t *testing.T) {
	dockerClient := newReconcilerDocker(
		[]swarm.Service{makeReplicatedService("svc1", "stack", "web", 1)},
		nil,
	)
	reconciler := newTestReconciler(t, dockerClient)

	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))

	if taken := reconciler.takeDueKeys(time.Now().Add(time.Hour)); len(taken) != 0 {
		t.Fatalf("took %d keys before the first resync, want 0", len(taken))
	}

	completeFirstResync(t, reconciler)

	if taken := reconciler.takeDueKeys(time.Now()); len(taken) != 1 {
		t.Fatalf("took %d keys after the first resync, want 1", len(taken))
	}
}

// ---- Ordering ----

func TestReconciler_UpdateThenRemove(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)
	dockerClient := newReconcilerDocker(
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
	)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionRemove))
	dockerClient.setServices()

	reconciler.cycle(context.Background())

	if _, cached := getServiceMetadata("svc1"); cached {
		t.Error("svc1 still cached after update then remove")
	}

	if got := desiredSeries(t); got != 0 {
		t.Errorf("desired_replicas series = %d, want 0", got)
	}
}

func TestReconciler_RemoveThenLateUpdate(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)
	dockerClient := newReconcilerDocker(
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
	)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	dockerClient.setServices()
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionRemove))
	reconciler.cycle(context.Background())

	// An update event delivered after the remove must not bring the service back.
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.cycle(context.Background())

	if _, cached := getServiceMetadata("svc1"); cached {
		t.Error("a late update brought svc1 back")
	}

	if got := desiredSeries(t); got != 0 {
		t.Errorf("desired_replicas series = %d, want 0", got)
	}
}

func TestReconciler_NodeEventDuringRemove(t *testing.T) {
	agent := makeGlobalService("glb1", "stack", "agent")
	dockerClient := newReconcilerDocker(
		[]swarm.Service{agent},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
	)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	dockerClient.setServices()
	dockerClient.setNodes(makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2"))
	reconciler.enqueueEvent(serviceEvent("glb1", events.ActionRemove))
	reconciler.enqueueEvent(nodeEvent("n2"))

	reconciler.cycle(context.Background())

	if len(getCachedNodes()) != 2 {
		t.Errorf("cached nodes = %d, want 2", len(getCachedNodes()))
	}

	if _, cached := getServiceMetadata("glb1"); cached {
		t.Error("glb1 still cached")
	}

	// The node refresh recomputed glb1 before its removal was reconciled; nothing may be left.
	if got := desiredSeries(t); got != 0 {
		t.Errorf("desired_replicas series = %d, want 0", got)
	}
}

func TestReconciler_BurstCoalesces(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)
	dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	for range 100 {
		reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	}

	reconciler.cycle(context.Background())

	if got := dockerClient.inspectCount("svc1"); got != 1 {
		t.Errorf("inspects = %d, want 1 for 100 coalesced events", got)
	}
}

func TestReconciler_EventDuringStartupSnapshot(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	late := makeReplicatedService("svc2", "stack", "late", 4)
	dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
	reconciler := newTestReconciler(t, dockerClient)

	// svc2 is created after ServiceList has read the services: only its event reports it.
	dockerClient.afterServiceList = func() {
		dockerClient.afterServiceList = nil
		dockerClient.setServices(web, late)
		reconciler.enqueueEvent(serviceEvent("svc2", events.ActionCreate))
	}

	runReconciler(t, reconciler)

	eventually(t, "svc2 cached from its event", func() bool {
		_, cached := getServiceMetadata("svc2")

		return cached
	})

	if got, _ := getServiceDesiredReplicas("svc2"); got != 4 {
		t.Errorf("svc2 desired = %v, want 4", got)
	}
}

func TestReconciler_LabelIdentityChangeDeletesOldSeries(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)
	dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	moved := makeReplicatedService("svc1", "other", "web", 2)
	dockerClient.setServices(moved)
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.cycle(context.Background())

	if got := desiredSeries(t); got != 1 {
		t.Fatalf(
			"desired_replicas series = %d, want 1: the old stack's series must be deleted",
			got,
		)
	}

	if got := testutil.ToFloat64(
		desiredReplicasGauge.With(serviceLabels("other", "web", serviceModeReplicated)),
	); got != 2 {
		t.Errorf("desired_replicas under the new identity = %v, want 2", got)
	}
}

func TestReconciler_ResyncRemovesServicesNoLongerListed(t *testing.T) {
	dockerClient := newReconcilerDocker([]swarm.Service{
		makeReplicatedService("svc1", "stack", "web", 1),
		makeReplicatedService("svc2", "stack", "db", 1),
	}, nil)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	dockerClient.setServices(makeReplicatedService("svc1", "stack", "web", 1))
	reconciler.requestResync(time.Now(), "test")
	reconciler.resync(context.Background())

	if _, cached := getServiceMetadata("svc2"); cached {
		t.Error("svc2 still cached after a resync that no longer lists it")
	}

	if got := desiredSeries(t); got != 1 {
		t.Errorf("desired_replicas series = %d, want 1", got)
	}
}

func TestReconciler_JobCounts(t *testing.T) {
	replicatedJob := makeReplicatedJobService("rjob1", "stack", "migrate", 5)
	globalJob := makeGlobalJobService("gjob1", "stack", "prune")
	dockerClient := newReconcilerDocker(
		[]swarm.Service{replicatedJob, globalJob},
		[]swarm.Node{makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2")},
	)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	// A node event recomputes every service from the new snapshot.
	dockerClient.setNodes(
		makeSchedulableNode("n1", "h1"),
		makeSchedulableNode("n2", "h2"),
		makeSchedulableNode("n3", "h3"),
	)
	reconciler.enqueueEvent(nodeEvent("n3"))
	reconciler.cycle(context.Background())

	replicatedLabels := serviceLabels("stack", "migrate", serviceModeReplicatedJob)
	globalLabels := serviceLabels("stack", "prune", serviceModeGlobalJob)

	cases := []struct {
		name  string
		gauge *prometheus.GaugeVec
		lbls  prometheus.Labels
		want  float64
	}{
		{"replicated-job desired is total completions", desiredReplicasGauge, replicatedLabels, 5},
		{"replicated-job schedulable is 0", schedulableReplicasGauge, replicatedLabels, 0},
		{"global-job desired is eligible nodes", desiredReplicasGauge, globalLabels, 3},
		{"global-job schedulable is 0", schedulableReplicasGauge, globalLabels, 0},
	}

	for _, testCase := range cases {
		if got := testutil.ToFloat64(testCase.gauge.With(testCase.lbls)); got != testCase.want {
			t.Errorf("%s: got %v, want %v", testCase.name, got, testCase.want)
		}
	}
}

// ---- Bounds ----

func TestReconciler_OverflowRequestsResync(t *testing.T) {
	reconciler := newTestReconciler(t, newReconcilerDocker(nil, nil))
	completeFirstResync(t, reconciler)
	reconciler.pendingCap = 4

	requestedBefore, _, _ := reconciler.resyncState()
	epochBefore := reconciler.epoch

	for index := range 5 {
		reconciler.enqueueEvent(serviceEvent(fmt.Sprintf("svc%d", index), events.ActionUpdate))
	}

	setLen, orderLen := pendingLengths(reconciler)
	if setLen != 0 || orderLen != 0 {
		t.Errorf("after overflow: set %d, FIFO %d, want both 0", setLen, orderLen)
	}

	requested, completed, _ := reconciler.resyncState()
	if requested != requestedBefore+1 || requested <= completed {
		t.Errorf(
			"resync requested = %d (was %d), completed = %d: want one more outstanding request",
			requested,
			requestedBefore,
			completed,
		)
	}

	if reconciler.epoch <= epochBefore {
		t.Error("overflow did not bump the epoch")
	}
}

func TestReconciler_RepeatedOverflowsLeaveNoStaleKeys(t *testing.T) {
	reconciler := newTestReconciler(t, newReconcilerDocker(nil, nil))
	completeFirstResync(t, reconciler)
	reconciler.pendingCap = 4

	for round := range 3 {
		for index := range 5 {
			reconciler.enqueueEvent(
				serviceEvent(fmt.Sprintf("r%d-svc%d", round, index), events.ActionUpdate),
			)
		}

		setLen, orderLen := pendingLengths(reconciler)
		if setLen != 0 || orderLen != 0 {
			t.Fatalf("overflow %d: set %d, FIFO %d, want both 0", round, setLen, orderLen)
		}
	}

	reconciler.mu.Lock()
	generations := len(reconciler.serviceGeneration)
	reconciler.mu.Unlock()

	// Nothing is cached: every generation the dropped keys bumped must be gone with them.
	if generations > reconciler.pendingCap {
		t.Errorf("service generations = %d after the overflows, want at most the cap (%d)",
			generations, reconciler.pendingCap)
	}

	reconciler.enqueueEvent(serviceEvent("fresh", events.ActionUpdate))

	taken := reconciler.takeDueKeys(time.Now())
	if len(taken) != 1 || taken[0].serviceID != "fresh" {
		t.Errorf("took %v, want only the key queued after the overflows", taken)
	}
}

func TestReconciler_OverflowDuringResyncRunsAnotherPass(t *testing.T) {
	dockerClient := newReconcilerDocker(
		[]swarm.Service{makeReplicatedService("svc1", "stack", "web", 1)},
		nil,
	)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)
	reconciler.pendingCap = 4

	// The overflow lands after ServiceList returned and before its result is applied.
	dockerClient.afterServiceList = func() {
		dockerClient.afterServiceList = nil

		for index := range 5 {
			reconciler.enqueueEvent(
				serviceEvent(fmt.Sprintf("burst%d", index), events.ActionUpdate),
			)
		}
	}

	reconciler.requestResync(time.Now(), "test")
	reconciler.resync(context.Background())

	if !reconciler.resyncDue(time.Now()) {
		t.Fatal("no second pass due after an overflow during the first")
	}

	reconciler.resync(context.Background())

	if serviceListCalls := dockerClient.serviceListCount(); serviceListCalls != 3 {
		t.Errorf(
			"ServiceList calls = %d, want 3 (startup, the pass, the second pass)",
			serviceListCalls,
		)
	}

	requested, completed, _ := reconciler.resyncState()
	if requested != completed {
		t.Errorf(
			"requested %d, completed %d after the second pass, want equal",
			requested,
			completed,
		)
	}
}

func TestReconciler_EmptyActorIDDropped(t *testing.T) {
	reconciler := newTestReconciler(t, newReconcilerDocker(nil, nil))
	completeFirstResync(t, reconciler)

	dropped := prometheus.NewCounter(
		prometheus.CounterOpts{Name: "test_events_dropped_total", Help: "test"},
	)
	previous := eventsDroppedTotalCounter
	eventsDroppedTotalCounter = dropped

	t.Cleanup(func() { eventsDroppedTotalCounter = previous })

	reconciler.enqueueEvent(serviceEvent("", events.ActionUpdate))
	reconciler.enqueueEvent(
		&events.Message{Type: events.NodeEventType, Action: events.ActionUpdate},
	)

	if setLen, _ := pendingLengths(reconciler); setLen != 0 {
		t.Errorf("queued keys = %d, want 0", setLen)
	}

	if reconciler.nodesDue(time.Now()) {
		t.Error("a node event without actor ID flagged the nodes")
	}

	if got := testutil.ToFloat64(dropped); got != 2 {
		t.Errorf("events_dropped_total = %v, want 2", got)
	}
}

func TestReconciler_RetryLimitRequestsResync(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	dockerClient.setInspectErr("svc1", errInspectUnavailable)
	// Keep the resync this ends in from succeeding, so the count below is only the key's.
	dockerClient.setListErrors(errListUnavailable, nil)

	requestedBefore, _, _ := reconciler.resyncState()

	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	runReconciler(t, reconciler)

	wantInspects := len(reconciler.retryDelays) + 1

	eventually(t, "the retries to run out and request a resync", func() bool {
		requested, _, _ := reconciler.resyncState()

		return requested > requestedBefore
	})

	if got := dockerClient.inspectCount("svc1"); got != wantInspects {
		t.Errorf("inspects = %d, want %d (one attempt and one per retry delay)", got, wantInspects)
	}

	if setLen, _ := pendingLengths(reconciler); setLen != 0 {
		t.Errorf("queued keys = %d after the retries ran out, want 0", setLen)
	}
}

func TestReconciler_FreshEventSupersedesRetry(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)
	reconciler.retryDelays = []time.Duration{time.Hour}

	dockerClient.setInspectErr("svc1", errInspectUnavailable)
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.cycle(context.Background())

	if taken := reconciler.takeDueKeys(time.Now()); len(taken) != 0 {
		t.Fatalf("the failed key was due at once: %v", taken)
	}

	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))

	taken := reconciler.takeDueKeys(time.Now())
	if len(taken) != 1 || taken[0].attempts != 0 {
		t.Errorf("took %v, want svc1 due now with its attempts reset", taken)
	}
}

// ---- Failure ----

func TestReconciler_NodeListFailureLeavesStateUnchanged(t *testing.T) {
	dockerClient := newReconcilerDocker(
		[]swarm.Service{makeReplicatedService("svc1", "stack", "web", 1)},
		nil,
	)
	dockerClient.setListErrors(nil, errListUnavailable)
	reconciler := newTestReconciler(t, dockerClient)

	reconciler.resync(context.Background())

	if _, cached := getServiceMetadata("svc1"); cached {
		t.Error("a resync whose NodeList failed cached a service")
	}

	if got := desiredSeries(t); got != 0 {
		t.Errorf("desired_replicas series = %d, want 0", got)
	}

	if _, completed, _ := reconciler.resyncState(); completed != 0 {
		t.Errorf("resync completed = %d after a failure, want 0", completed)
	}

	if reconciler.resyncDue(time.Now()) {
		t.Error("the failed resync is due again at once, want it backed off")
	}

	dockerClient.setListErrors(nil, nil)
	runReconciler(t, reconciler)

	select {
	case <-reconciler.Ready():
	case <-time.After(5 * time.Second):
		t.Fatal("the resync was never retried")
	}
}

func TestReconciler_PersistentListFailureIsRateLimited(t *testing.T) {
	dockerClient := newReconcilerDocker(nil, nil)
	dockerClient.setListErrors(errListUnavailable, nil)
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)

	// A real elapsed window is what this test measures: the number of attempts within it.
	const window = 400 * time.Millisecond

	<-time.After(window)

	serviceListCalls := dockerClient.serviceListCount()

	// Backoff 20, 40, 80, 80, ... ms: about 7 attempts fit in the window. Without the backoff the
	// loop would spin through thousands.
	if serviceListCalls < 2 || serviceListCalls > 12 {
		t.Errorf("ServiceList calls in %s = %d, want between 2 and 12", window, serviceListCalls)
	}
}

func TestReconciler_PeriodicResync(t *testing.T) {
	dockerClient := newReconcilerDocker(nil, nil)
	reconciler := newTestReconciler(t, dockerClient)
	reconciler.resyncInterval = 20 * time.Millisecond

	runReconciler(t, reconciler)

	eventually(t, "a periodic resync after the first", func() bool {
		serviceListCalls := dockerClient.serviceListCount()

		return serviceListCalls >= 3
	})
}

// ---- Health ----

func TestHealthSnapshot_ResyncOutstandingPastGrace(t *testing.T) {
	resetHealthState(t)

	reconciler := activeReconciler.Load()
	pollDelay := 10 * time.Second
	now := time.Now()

	MarkPollOK(now)

	reconciler.mu.Lock()
	reconciler.resyncRequested = reconciler.resyncCompleted + 1
	reconciler.resyncOutstandingSince = now.Add(-20 * time.Second)
	reconciler.mu.Unlock()

	if healthy, reason := HealthSnapshot(pollDelay, now); !healthy {
		t.Errorf("unhealthy within the grace period: %q", reason)
	}

	reconciler.mu.Lock()
	reconciler.resyncOutstandingSince = now.Add(-31 * time.Second)
	reconciler.mu.Unlock()

	healthy, reason := HealthSnapshot(pollDelay, now)
	if healthy || reason != "resync outstanding" {
		t.Errorf(
			"HealthSnapshot = %v, %q; want unhealthy, %q",
			healthy,
			reason,
			"resync outstanding",
		)
	}
}

func TestHealthSnapshot_InitialResyncNotCompleted(t *testing.T) {
	resetHealthState(t)

	activeReconciler.Store(NewReconciler(&fakeDocker{}))
	MarkPollOK(time.Now())

	healthy, reason := HealthSnapshot(10*time.Second, time.Now())
	if healthy || reason != "initial resync not completed" {
		t.Errorf(
			"HealthSnapshot = %v, %q; want unhealthy, %q",
			healthy,
			reason,
			"initial resync not completed",
		)
	}
}

// ---- Event stream ----

// streamDocker records the context of every Events call and serves streams the test controls.
type streamDocker struct {
	*fakeDocker

	mu       sync.Mutex
	contexts []context.Context
	errChans []chan error
	opened   chan struct{}
}

func newStreamDocker() *streamDocker {
	return &streamDocker{fakeDocker: &fakeDocker{}, opened: make(chan struct{}, 8)}
}

func (d *streamDocker) Events(ctx context.Context, _ client.EventsListOptions) client.EventsResult {
	errChan := make(chan error, 1)

	d.mu.Lock()
	d.contexts = append(d.contexts, ctx)
	d.errChans = append(d.errChans, errChan)
	d.mu.Unlock()

	d.opened <- struct{}{}

	return client.EventsResult{Messages: make(chan events.Message), Err: errChan}
}

func (d *streamDocker) stream(index int) (streamContext context.Context, errChan chan error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.contexts[index], d.errChans[index]
}

func waitOpened(t *testing.T, dockerClient *streamDocker) {
	t.Helper()

	select {
	case <-dockerClient.opened:
	case <-time.After(5 * time.Second):
		t.Fatal("event stream was never opened")
	}
}

func TestListenSwarmEvents_StreamHasNoDeadlineAndIsCanceledOnShutdown(t *testing.T) {
	dockerClient := newStreamDocker()
	reconciler := newTestReconciler(t, dockerClient)

	ctx, cancel := context.WithCancel(context.Background())
	listenerDone := make(chan error, 1)

	go func() { listenerDone <- ListenSwarmEvents(ctx, dockerClient, reconciler, time.Now()) }()

	waitOpened(t, dockerClient)

	streamContext, _ := dockerClient.stream(0)
	if deadline, hasDeadline := streamContext.Deadline(); hasDeadline {
		t.Errorf(
			"event stream context has a deadline (%s); the stream must outlive dockerRequestTimeout",
			deadline,
		)
	}

	cancel()

	select {
	case <-listenerDone:
	case <-time.After(5 * time.Second):
		t.Fatal("ListenSwarmEvents did not return after cancellation")
	}

	if streamContext.Err() == nil {
		t.Error("event stream context still live after shutdown")
	}
}

func TestListenSwarmEvents_StreamCanceledOnReconnect(t *testing.T) {
	dockerClient := newStreamDocker()
	reconciler := newTestReconciler(t, dockerClient)

	ctx, cancel := context.WithCancel(context.Background())
	listenerDone := make(chan error, 1)

	t.Cleanup(func() {
		cancel()
		<-listenerDone
	})

	go func() { listenerDone <- ListenSwarmEvents(ctx, dockerClient, reconciler, time.Now()) }()

	waitOpened(t, dockerClient)

	firstContext, firstErr := dockerClient.stream(0)
	firstErr <- errClosedBody

	waitOpened(t, dockerClient)

	if firstContext.Err() == nil {
		t.Error("the first stream's context is still live after the listener reconnected")
	}
}

func TestListenSwarmEvents_SinceHasNanosecondPrecision(t *testing.T) {
	dockerClient := &sinceRecordingDocker{fakeDocker: &fakeDocker{}, since: make(chan string, 1)}
	reconciler := newTestReconciler(t, dockerClient)

	ctx, cancel := context.WithCancel(context.Background())
	listenerDone := make(chan error, 1)

	t.Cleanup(func() {
		cancel()
		<-listenerDone
	})

	anchor := time.Unix(1_700_000_000, 123_456_789)

	go func() { listenerDone <- ListenSwarmEvents(ctx, dockerClient, reconciler, anchor) }()

	var since string

	select {
	case since = <-dockerClient.since:
	case <-time.After(5 * time.Second):
		t.Fatal("event stream was never opened")
	}

	parsed, parseErr := time.Parse(time.RFC3339Nano, since)
	if parseErr != nil {
		t.Fatalf("since %q: %v", since, parseErr)
	}

	if want := anchor.Add(-eventsSinceMargin); !parsed.Equal(want) {
		t.Errorf(
			"since = %s, want %s (the anchor minus the margin, to the nanosecond)",
			parsed,
			want,
		)
	}
}

// sinceRecordingDocker reports the Since option of the first Events call.
type sinceRecordingDocker struct {
	*fakeDocker

	since chan string
}

func (d *sinceRecordingDocker) Events(
	_ context.Context,
	options client.EventsListOptions,
) client.EventsResult {
	select {
	case d.since <- options.Since:
	default:
	}

	return client.EventsResult{Messages: make(chan events.Message), Err: make(chan error)}
}

// ---- Deadlines ----

// deadlineDocker wraps reconcilerDocker and records every request/response call made with a
// context that has no deadline, or one further away than dockerRequestTimeout.
type deadlineDocker struct {
	*reconcilerDocker

	mu        sync.Mutex
	unbounded []string
}

func (d *deadlineDocker) NodeList(
	ctx context.Context,
	options client.NodeListOptions,
) (client.NodeListResult, error) {
	d.check(ctx, "NodeList")

	return d.reconcilerDocker.NodeList(ctx, options)
}

func (d *deadlineDocker) ServiceList(
	ctx context.Context,
	options client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.check(ctx, "ServiceList")

	return d.reconcilerDocker.ServiceList(ctx, options)
}

func (d *deadlineDocker) ServiceInspect(
	ctx context.Context,
	serviceID string,
	options client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	d.check(ctx, "ServiceInspect")

	return d.reconcilerDocker.ServiceInspect(ctx, serviceID, options)
}

func (d *deadlineDocker) TaskList(
	ctx context.Context,
	_ client.TaskListOptions,
) (client.TaskListResult, error) {
	d.check(ctx, "TaskList")

	return client.TaskListResult{Items: []swarm.Task{{ID: "t1", ServiceID: "svc2", Slot: 1}}}, nil
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
	global := makeGlobalService("glb1", "stack", "agent")
	replicated := makeReplicatedService("svc1", "stack", "web", 2)
	uncached := makeReplicatedService("svc2", "other", "late", 1)

	inner := newReconcilerDocker(
		[]swarm.Service{global, replicated},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
	)
	dockerClient := &deadlineDocker{reconcilerDocker: inner}
	reconciler := newTestReconciler(t, dockerClient)
	installReplicasStateGauges(t)

	root := context.Background()

	// Resync, a service event and a node event.
	completeFirstResync(t, reconciler)
	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.enqueueEvent(nodeEvent("n1"))
	reconciler.cycle(root)

	// svc2 is not cached, so the poll takes the slow path and inspects it.
	inner.setServices(global, replicated, uncached)

	_, pollErr := PollReplicasState(root, dockerClient)
	if pollErr != nil {
		t.Fatalf("poll: %v", pollErr)
	}

	dockerClient.mu.Lock()
	defer dockerClient.mu.Unlock()

	if len(dockerClient.unbounded) != 0 {
		t.Errorf("Docker calls without a request deadline: %v", dockerClient.unbounded)
	}
}

// panickingDocker panics on every service inspect.
type panickingDocker struct {
	*reconcilerDocker
}

func (*panickingDocker) ServiceInspect(
	context.Context,
	string,
	client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	panic("unexpected service data")
}

func TestReconciler_PanicInStepRequestsResync(t *testing.T) {
	dockerClient := &panickingDocker{reconcilerDocker: newReconcilerDocker(nil, nil)}
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	requestedBefore, _, _ := reconciler.resyncState()

	reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	reconciler.cycle(context.Background())

	if requested, _, _ := reconciler.resyncState(); requested != requestedBefore+1 {
		t.Errorf("resync requested = %d after a panic, want %d", requested, requestedBefore+1)
	}
}

// blockingListDocker blocks every ServiceList until its context is done, reporting each call.
type blockingListDocker struct {
	*reconcilerDocker

	called chan struct{}
}

func (d *blockingListDocker) ServiceList(
	ctx context.Context,
	_ client.ServiceListOptions,
) (client.ServiceListResult, error) {
	select {
	case d.called <- struct{}{}:
	default:
	}

	<-ctx.Done()

	return client.ServiceListResult{}, fmt.Errorf("blocked service list: %w", ctx.Err())
}

func TestReconciler_CancelDuringBlockedCall(t *testing.T) {
	dockerClient := &blockingListDocker{
		reconcilerDocker: newReconcilerDocker(nil, nil),
		called:           make(chan struct{}, 1),
	}
	reconciler := newTestReconciler(t, dockerClient)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan struct{})

	go func() {
		reconciler.Run(ctx)
		close(done)
	}()

	select {
	case <-dockerClient.called:
	case <-time.After(5 * time.Second):
		t.Fatal("the first resync never listed services")
	}

	cancel()

	// Returning within this bound, a third of dockerRequestTimeout, shows the blocked call
	// followed the cancellation rather than its own deadline.
	select {
	case <-done:
	case <-time.After(dockerRequestTimeout / 3):
		t.Fatal("Run did not return after cancellation during a blocked ServiceList")
	}
}

// panickingListDocker panics on every ServiceList, counting calls.
type panickingListDocker struct {
	*reconcilerDocker

	mu    sync.Mutex
	calls int
}

func (d *panickingListDocker) ServiceList(
	context.Context,
	client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.mu.Lock()
	d.calls++
	d.mu.Unlock()

	panic("unexpected service list data")
}

func TestReconciler_PanickingResyncIsRateLimited(t *testing.T) {
	dockerClient := &panickingListDocker{reconcilerDocker: newReconcilerDocker(nil, nil)}
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)

	// A real elapsed window is what this test measures: the number of attempts within it.
	const window = 400 * time.Millisecond

	<-time.After(window)

	dockerClient.mu.Lock()
	calls := dockerClient.calls
	dockerClient.mu.Unlock()

	// The resync backoff (20, 40, 80, 80, ... ms) fits about 7 attempts in the window; retried at
	// once, a resync that panics every time would run thousands.
	if calls < 2 || calls > 12 {
		t.Errorf("ServiceList calls in %s = %d, want between 2 and 12", window, calls)
	}
}
