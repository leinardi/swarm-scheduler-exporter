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
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

// runningTask returns a running task of serviceID in slot, on nodeID.
func runningTask(serviceID string, slot int, nodeID string) swarm.Task {
	return swarm.Task{
		Meta:         swarm.Meta{CreatedAt: time.Date(2025, 1, 1, 0, 0, slot, 0, time.UTC)},
		ServiceID:    serviceID,
		Slot:         slot,
		NodeID:       nodeID,
		DesiredState: swarm.TaskStateRunning,
		Status:       swarm.TaskStatus{State: swarm.TaskStateRunning},
	}
}

// pollTestSetup is a reconciler past its first resync, with the replicas-state families and the
// rejections counter installed.
type pollTestSetup struct {
	docker     *reconcilerDocker
	reconciler *Reconciler
	families   *replicasStateSnapshot
	rejections prometheus.Counter
}

func newPollTestSetup(
	t *testing.T,
	services []swarm.Service,
	nodes []swarm.Node,
	tasks []swarm.Task,
) *pollTestSetup {
	t.Helper()

	dockerClient := newReconcilerDocker(services, nodes)
	dockerClient.setTasks(tasks...)

	reconciler := newTestReconciler(t, dockerClient)
	families := installReplicasStateGauges(t)

	rejections := prometheus.NewCounter(
		prometheus.CounterOpts{Name: "test_poll_rejections_total", Help: "test"},
	)
	previous := pollRejectionsTotalCounter
	pollRejectionsTotalCounter = rejections

	t.Cleanup(func() { pollRejectionsTotalCounter = previous })

	completeFirstResync(t, reconciler)

	return &pollTestSetup{
		docker:     dockerClient,
		reconciler: reconciler,
		families:   families,
		rejections: rejections,
	}
}

// pollAgainst counts the tasks of snapshot, runs change (a change landing while the tasks were
// being listed), then applies the counts, returning the verdict.
func (setup *pollTestSetup) pollAgainst(t *testing.T, snapshot *pollSnapshot, change func()) error {
	t.Helper()

	counters, pollErr := pollReplicasState(context.Background(), setup.docker, snapshot)
	if pollErr != nil {
		t.Fatalf("pollReplicasState: %v", pollErr)
	}

	if change != nil {
		change()
	}

	return setup.reconciler.applyPollCounts(snapshot, counters)
}

// poll runs one poll with no change in between, failing the test if it is not fully published.
func (setup *pollTestSetup) poll(t *testing.T) {
	t.Helper()

	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), nil)
	if applyErr != nil {
		t.Fatalf("poll: %v", applyErr)
	}
}

// update makes Docker return services and reconciles an update event for each of them.
func (setup *pollTestSetup) update(services ...swarm.Service) {
	setup.docker.setServices(services...)

	for index := range services {
		setup.reconciler.enqueueEvent(serviceEvent(services[index].ID, events.ActionUpdate))
	}

	setup.reconciler.cycle(context.Background())
}

func (setup *pollTestSetup) value(
	t *testing.T,
	fqName string,
	labels prometheus.Labels,
) (float64, bool) {
	t.Helper()

	return snapshotValue(t, setup.families, fqName, labels)
}

func (setup *pollTestSetup) assertValue(
	t *testing.T,
	what, fqName string,
	labels prometheus.Labels,
	want float64,
) {
	t.Helper()

	got, found := setup.value(t, fqName, labels)
	if !found || got != want {
		t.Errorf("%s = %v (found %v), want %v", what, got, found, want)
	}
}

// ---- Staleness ----

func TestPoll_SameIDJobRerunIsRejected(t *testing.T) {
	job := makeReplicatedJobService("job1", "stack", "migrate", 1)
	finished := makeJobTask("job1", 0, "n1", 1, swarm.TaskStateComplete, swarm.TaskStateComplete)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{job},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{finished},
	)

	setup.poll(t)

	labels := serviceLabels("stack", "migrate", serviceModeReplicatedJob)
	setup.assertValue(t, "at_desired after the first run", atDesiredFQName, labels, 1)

	// The job runs again under the same ID: its tasks of iteration 2 now run, and the snapshot
	// still holds iteration 1, whose finished task would count.
	rerun := makeReplicatedJobService("job1", "stack", "migrate", 1)
	rerun.JobStatus.JobIteration.Index = 2

	setup.docker.setTasks(
		finished,
		makeJobTask("job1", 0, "n1", 2, swarm.TaskStateComplete, swarm.TaskStateRunning),
	)

	applyErr := setup.pollAgainst(
		t,
		setup.reconciler.buildPollSnapshot(),
		func() { setup.update(rerun) },
	)
	if applyErr != nil {
		t.Fatalf("first rejection: %v, want the previous series carried over", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1", got)
	}

	// Carried over, not the stale count: the next poll, against iteration 2, reports it running.
	setup.assertValue(t, "at_desired carried over", atDesiredFQName, labels, 1)

	setup.poll(t)
	setup.assertValue(t, "at_desired of the rerun", atDesiredFQName, labels, 0)
}

func TestPoll_ScalingIsRejected(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)
	tasks := []swarm.Task{runningTask("svc1", 1, "n1"), runningTask("svc1", 2, "n1")}
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		tasks,
	)

	setup.poll(t)

	labels := serviceLabels("stack", "web", serviceModeReplicated)
	setup.assertValue(t, "at_desired at 2 of 2", atDesiredFQName, labels, 1)

	// Scaled to 5 while the tasks were listed: 2 running against the old desired of 2 would say
	// at desired.
	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.update(makeReplicatedService("svc1", "stack", "web", 5))
	})
	if applyErr != nil {
		t.Fatalf("first rejection: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1", got)
	}

	setup.poll(t)
	setup.assertValue(t, "at_desired at 2 of 5", atDesiredFQName, labels, 0)
}

func TestPoll_ModeChangeIsRejectedWithoutCarryOver(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.update(makeGlobalService("svc1", "stack", "web"))
	})
	if applyErr != nil {
		t.Fatalf("first rejection: %v", applyErr)
	}

	// The label identity changed (service_mode), so the old series are not carried over.
	if _, found := setup.value(
		t,
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeReplicated),
	); found {
		t.Error("the replicated-mode series were carried over across a mode change")
	}

	if _, found := setup.value(
		t,
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeGlobal),
	); found {
		t.Error("the global-mode series were published from a poll counted as replicated")
	}
}

func TestPoll_PlacementChangeIsRejected(t *testing.T) {
	agent := makeGlobalService("glb1", "stack", "agent")
	nodes := []swarm.Node{makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2")}
	setup := newPollTestSetup(t, []swarm.Service{agent}, nodes,
		[]swarm.Task{runningTask("glb1", 0, "n1"), runningTask("glb1", 0, "n2")})

	setup.poll(t)

	pinned := makeGlobalService("glb1", "stack", "agent")
	pinned.Spec.TaskTemplate.Placement = &swarm.Placement{
		Constraints: []string{"node.hostname == h1"},
	}

	applyErr := setup.pollAgainst(
		t,
		setup.reconciler.buildPollSnapshot(),
		func() { setup.update(pinned) },
	)
	if applyErr != nil {
		t.Fatalf("first rejection: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1", got)
	}
}

func TestPoll_NodeChangeDuringTaskListRejectsNodeDependentOnly(t *testing.T) {
	agent := makeGlobalService("glb1", "infra", "agent")
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{agent, web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("glb1", 0, "n1"), runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	setup.docker.setTasks(runningTask("glb1", 0, "n1"))

	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.docker.setNodes(makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2"))
		setup.reconciler.enqueueEvent(nodeEvent("n2"))
		setup.reconciler.cycle(context.Background())
	})
	if applyErr != nil {
		t.Fatalf("apply: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1 (the global service only)", got)
	}

	// The replicated service is not node-dependent: its new count (no task) was published.
	setup.assertValue(
		t,
		"web running",
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeReplicated),
		0,
	)
	// The global one kept its previous series.
	setup.assertValue(
		t,
		"agent running",
		runningReplicasFQName,
		serviceLabels("infra", "agent", serviceModeGlobal),
		1,
	)
}

func TestPoll_UnappliedNodeChangeRejectsNodeDependentOnly(t *testing.T) {
	agent := makeGlobalService("glb1", "stack", "agent")
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{agent, web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("glb1", 0, "n1"), runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	// A node joined, but the node list fails: the refresh sits in backoff with the node event
	// queued and unapplied, before and after the snapshot is taken.
	setup.docker.setNodes(makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2"))
	setup.docker.setListErrors(nil, errListUnavailable)
	setup.reconciler.enqueueEvent(nodeEvent("n2"))
	setup.reconciler.cycle(context.Background())

	setup.docker.setTasks(
		runningTask("glb1", 0, "n1"),
		runningTask("glb1", 0, "n2"),
		runningTask("svc1", 1, "n1"),
	)

	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), nil)
	if applyErr != nil {
		t.Fatalf("apply: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1 (the global service only)", got)
	}

	// Counted against the one-node list, the agent would read 2 running of 1 desired.
	setup.assertValue(
		t,
		"agent running",
		runningReplicasFQName,
		serviceLabels("stack", "agent", serviceModeGlobal),
		1,
	)
	setup.assertValue(
		t,
		"web running",
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeReplicated),
		1,
	)
}

func TestPoll_QueuedUnappliedEventIsRejected(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	// Queued while the tasks were listed, not applied yet.
	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
	})
	if applyErr != nil {
		t.Fatalf("first rejection: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1", got)
	}
}

func TestPoll_ResyncRequestedDuringTaskListIsRejected(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	// A resync (a dropped event, an overflow) means the caches may be behind on anything: the
	// epoch moves and no count from before it is published.
	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.reconciler.requestResync(time.Now(), "test")
	})
	if applyErr != nil {
		t.Fatalf("first rejection: %v", applyErr)
	}

	if got := testutil.ToFloat64(setup.rejections); got != 1 {
		t.Errorf("poll_rejections_total = %v, want 1", got)
	}
}

func TestPoll_RemovedServiceIsDroppedAndMarkedDirty(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		nil,
		[]swarm.Task{runningTask("svc1", 1, "n1")},
	)

	setup.poll(t)

	applyErr := setup.pollAgainst(t, setup.reconciler.buildPollSnapshot(), func() {
		setup.reconciler.removeService("svc1")
	})
	if applyErr != nil {
		t.Fatalf("apply: %v", applyErr)
	}

	if _, found := setup.value(
		t,
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeReplicated),
	); found {
		t.Error("a service removed during the poll was published")
	}

	if setLen, _ := pendingLengths(setup.reconciler); setLen != 1 {
		t.Errorf("queued keys = %d, want the dropped service marked dirty", setLen)
	}
}

func TestPoll_ThirdConsecutiveRejectionOmitsAndFails(t *testing.T) {
	resetHealthState(t)

	web := makeReplicatedService("svc1", "stack", "web", 1)
	db := makeReplicatedService("svc2", "stack", "db", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web, db},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc1", 1, "n1"), runningTask("svc2", 1, "n1")},
	)
	setup.reconciler.retryDelays = []time.Duration{time.Hour}
	setup.reconciler.nextPeriodicResync = time.Now().Add(time.Hour)

	runReconciler(t, setup.reconciler)

	ctx := context.Background()

	firstErr := PollAndPublishReplicasState(ctx, setup.docker, setup.reconciler)
	if firstErr != nil {
		t.Fatalf("first poll: %v", firstErr)
	}

	// svc1's inspect now fails with an hour until its retry: its key stays dirty, so every poll
	// is rejected for it.
	setup.docker.setInspectErr("svc1", errInspectUnavailable)
	setup.reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))

	eventually(
		t,
		"svc1's inspect to fail",
		func() bool { return setup.docker.inspectCount("svc1") > 0 },
	)

	webLabels := serviceLabels("stack", "web", serviceModeReplicated)

	for rejection := 1; rejection <= maxCarriedPolls; rejection++ {
		pollErr := PollAndPublishReplicasState(ctx, setup.docker, setup.reconciler)
		if pollErr != nil {
			t.Fatalf("rejection %d: %v, want the series carried over", rejection, pollErr)
		}

		setup.assertValue(t, "svc1 running carried over", runningReplicasFQName, webLabels, 1)
	}

	timestampBefore := lastPollSuccessUnixNano.Load()

	thirdErr := PollAndPublishReplicasState(ctx, setup.docker, setup.reconciler)
	if !errors.Is(thirdErr, ErrPollPartiallyRejected) {
		t.Fatalf("third rejection: err = %v, want ErrPollPartiallyRejected", thirdErr)
	}

	if _, found := setup.value(t, runningReplicasFQName, webLabels); found {
		t.Error("svc1's series still published after its third rejection in a row")
	}

	setup.assertValue(
		t,
		"svc2 running",
		runningReplicasFQName,
		serviceLabels("stack", "db", serviceModeReplicated),
		1,
	)

	if got := lastPollSuccessUnixNano.Load(); got != timestampBefore {
		t.Errorf("poll timestamp moved on a partial failure: %d, want %d", got, timestampBefore)
	}
}

// ---- Liveness ----

func TestPoll_PendingRetryDoesNotDelayPublication(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	db := makeReplicatedService("svc2", "stack", "db", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web, db},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc2", 1, "n1")},
	)
	setup.reconciler.retryDelays = []time.Duration{time.Hour}
	setup.reconciler.nextPeriodicResync = time.Now().Add(time.Hour)

	runReconciler(t, setup.reconciler)

	// svc2's inspect fails with an hour until its retry: the poll must not wait for it.
	setup.docker.setInspectErr("svc2", errInspectUnavailable)
	setup.reconciler.enqueueEvent(serviceEvent("svc2", events.ActionUpdate))
	eventually(
		t,
		"svc2's inspect to fail",
		func() bool { return setup.docker.inspectCount("svc2") > 0 },
	)

	setup.docker.setTasks(runningTask("svc1", 1, "n1"), runningTask("svc2", 1, "n1"))

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	pollErr := PollAndPublishReplicasState(ctx, setup.docker, setup.reconciler)
	if pollErr != nil {
		t.Fatalf("poll with a retry pending: %v", pollErr)
	}

	setup.assertValue(
		t,
		"svc1 running",
		runningReplicasFQName,
		serviceLabels("stack", "web", serviceModeReplicated),
		1,
	)
}

func TestPoll_PersistentListFailureDoesNotDelayPolls(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 1)
	setup := newPollTestSetup(
		t,
		[]swarm.Service{web},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
		[]swarm.Task{runningTask("svc1", 1, "n1")},
	)
	setup.reconciler.backoffMax = time.Hour
	setup.reconciler.resyncBackoff = time.Hour

	setup.docker.setListErrors(errListUnavailable, nil)
	setup.reconciler.requestResync(time.Now(), "test")

	runReconciler(t, setup.reconciler)

	eventually(t, "the resync to fail", func() bool { return setup.docker.serviceListCount() > 1 })

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	for poll := range 3 {
		pollErr := PollAndPublishReplicasState(ctx, setup.docker, setup.reconciler)
		if pollErr != nil {
			t.Fatalf("poll %d with a failing resync outstanding: %v", poll, pollErr)
		}
	}
}

// ---- Cancellation races ----

// waitGroupReturns fails the test if workerGroup.Wait does not return within 5s.
func waitGroupReturns(t *testing.T, workerGroup *sync.WaitGroup) {
	t.Helper()

	done := make(chan struct{})

	go func() {
		workerGroup.Wait()
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("workerGroup.Wait did not return")
	}
}

// pollExchange runs the exchange under test: the snapshot request, or the apply request of a
// snapshot taken first.
type pollExchange func(ctx context.Context, reconciler *Reconciler) error

var pollExchanges = map[string]pollExchange{
	"snapshot": func(ctx context.Context, reconciler *Reconciler) error {
		_, requestErr := reconciler.requestPollSnapshot(ctx)

		return requestErr
	},
	"apply": func(ctx context.Context, reconciler *Reconciler) error {
		snapshot := reconciler.buildPollSnapshot()

		return reconciler.submitPollCounts(ctx, snapshot, serviceCounter{})
	},
}

func newRaceReconciler(t *testing.T) *Reconciler {
	t.Helper()

	reconciler := newTestReconciler(t, newReconcilerDocker(nil, nil))
	installReplicasStateGauges(t)
	completeFirstResync(t, reconciler)

	return reconciler
}

func TestPoll_ReconcilerExitsBeforeSubmission(t *testing.T) {
	for name, exchange := range pollExchanges {
		t.Run(name, func(t *testing.T) {
			reconciler := newRaceReconciler(t)

			ctx, cancel := context.WithCancel(context.Background())

			var workerGroup sync.WaitGroup

			workerGroup.Go(func() { reconciler.Run(ctx) })

			cancel()
			waitGroupReturns(t, &workerGroup)

			// Nobody receives any more; the poller, on the same root context, gives up.
			var exchangeErr error

			workerGroup.Go(func() { exchangeErr = exchange(ctx, reconciler) })
			waitGroupReturns(t, &workerGroup)

			if !errors.Is(exchangeErr, context.Canceled) {
				t.Errorf("err = %v, want a context.Canceled wrap", exchangeErr)
			}
		})
	}
}

func TestPoll_ReconcilerExitsBeforeReply(t *testing.T) {
	for name, exchange := range pollExchanges {
		t.Run(name, func(t *testing.T) {
			reconciler := newRaceReconciler(t)

			reconcilerContext, cancelReconciler := context.WithCancel(context.Background())
			defer cancelReconciler()

			// The poller's own context outlives the test, so only the reconciler's answer can
			// release it.
			pollerContext, cancelPoller := context.WithTimeout(context.Background(), time.Minute)
			defer cancelPoller()

			// Shutdown lands after the request was accepted, before it is answered.
			reconciler.pollRequestHook = cancelReconciler

			var (
				workerGroup sync.WaitGroup
				exchangeErr error
			)

			workerGroup.Go(func() { reconciler.Run(reconcilerContext) })
			workerGroup.Go(func() { exchangeErr = exchange(pollerContext, reconciler) })
			waitGroupReturns(t, &workerGroup)

			if !errors.Is(exchangeErr, context.Canceled) {
				t.Errorf("err = %v, want a context.Canceled wrap", exchangeErr)
			}
		})
	}
}

func TestPoll_PollerExitsBeforeReply(t *testing.T) {
	for name, exchange := range pollExchanges {
		t.Run(name, func(t *testing.T) {
			reconciler := newRaceReconciler(t)

			reconcilerContext, cancelReconciler := context.WithCancel(context.Background())
			defer cancelReconciler()

			pollerContext, cancelPoller := context.WithCancel(context.Background())
			defer cancelPoller()

			pollerDone := make(chan struct{})

			// The poller gives up after the request was accepted; the reconciler answers only
			// once it has, into the buffered reply nobody reads.
			reconciler.pollRequestHook = func() {
				cancelPoller()
				<-pollerDone
			}

			var (
				workerGroup sync.WaitGroup
				exchangeErr error
			)

			workerGroup.Go(func() { reconciler.Run(reconcilerContext) })
			workerGroup.Go(func() {
				exchangeErr = exchange(pollerContext, reconciler)

				close(pollerDone)
			})

			// The reconciler must still be serving: a poller that stopped waiting cannot block it.
			eventually(t, "the poller to give up", func() bool {
				select {
				case <-pollerDone:
					return true
				default:
					return false
				}
			})

			reconciler.pollRequestHook = nil

			_, requestErr := reconciler.requestPollSnapshot(context.Background())
			if requestErr != nil {
				t.Errorf("snapshot after an abandoned request: %v", requestErr)
			}

			cancelReconciler()
			waitGroupReturns(t, &workerGroup)

			if !errors.Is(exchangeErr, context.Canceled) {
				t.Errorf("err = %v, want a context.Canceled wrap", exchangeErr)
			}
		})
	}
}
