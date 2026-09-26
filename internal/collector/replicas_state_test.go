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
	"maps"
	"sync"
	"testing"
	"time"

	"github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
)

var errTaskListFailed = errors.New("docker down")

func makeTask(createdAt, statusTS time.Time, versionIndex uint64) *swarm.Task {
	return &swarm.Task{
		Meta: swarm.Meta{
			CreatedAt: createdAt,
			Version:   swarm.Version{Index: versionIndex},
		},
		Status: swarm.TaskStatus{Timestamp: statusTS},
	}
}

// makeStateTask returns a task with the given creation time, desired state and actual state.
func makeStateTask(createdAt time.Time, desired, state swarm.TaskState) *swarm.Task {
	task := makeTask(createdAt, time.Time{}, 1)
	task.DesiredState = desired
	task.Status.State = state

	return task
}

func TestPreferredTask(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	later := base.Add(time.Second)

	lateShutdownOld := makeStateTask(base, swarm.TaskStateShutdown, swarm.TaskStateShutdown)
	lateShutdownOld.Status.Timestamp = base.Add(time.Minute)

	lateShutdownNew := makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStateAssigned)
	lateShutdownNew.Status.Timestamp = later

	tests := []struct {
		name   string
		winner *swarm.Task
		loser  *swarm.Task
	}{
		{
			name:   "issue 62: start-first update whose new task failed keeps the old one",
			winner: makeStateTask(base, swarm.TaskStateRunning, swarm.TaskStateRunning),
			loser:  makeStateTask(later, swarm.TaskStateShutdown, swarm.TaskStateFailed),
		},
		{
			name:   "node down: replacement beats stale task still reported running",
			winner: makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStatePending),
			loser:  makeStateTask(base, swarm.TaskStateShutdown, swarm.TaskStateRunning),
		},
		{
			name:   "crash and restart: replacement beats failed task",
			winner: makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStateRunning),
			loser:  makeStateTask(base, swarm.TaskStateShutdown, swarm.TaskStateFailed),
		},
		{
			name:   "start-first in progress: serving old task beats starting new one",
			winner: makeStateTask(base, swarm.TaskStateRunning, swarm.TaskStateRunning),
			loser:  makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStateStarting),
		},
		{
			name:   "start-first with both running: newer wins",
			winner: makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStateRunning),
			loser:  makeStateTask(base, swarm.TaskStateRunning, swarm.TaskStateRunning),
		},
		{
			name:   "stop-first in progress: new task desired ready beats old one being stopped",
			winner: makeStateTask(later, swarm.TaskStateReady, swarm.TaskStatePending),
			loser:  makeStateTask(base, swarm.TaskStateShutdown, swarm.TaskStateRunning),
		},
		{
			name:   "late shutdown status: new task wins despite older task's newer status timestamp",
			winner: lateShutdownNew,
			loser:  lateShutdownOld,
		},
		{
			name:   "both retired and not running: newer wins",
			winner: makeStateTask(later, swarm.TaskStateShutdown, swarm.TaskStateRejected),
			loser:  makeStateTask(base, swarm.TaskStateShutdown, swarm.TaskStateFailed),
		},
		{
			name:   "both wanted and not running: newer wins",
			winner: makeStateTask(later, swarm.TaskStateRunning, swarm.TaskStatePreparing),
			loser:  makeStateTask(base, swarm.TaskStateRunning, swarm.TaskStatePending),
		},
		{
			name:   "empty desired state counts as wanted",
			winner: makeStateTask(base, "", swarm.TaskStateRunning),
			loser:  makeStateTask(later, swarm.TaskStateShutdown, swarm.TaskStateFailed),
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if !preferredTask(tc.winner, tc.loser) {
				t.Error("preferredTask(winner, loser) = false, want true")
			}

			if preferredTask(tc.loser, tc.winner) {
				t.Error("preferredTask(loser, winner) = true, want false")
			}
		})
	}
}

func TestPreferredTask_AllRulesTie(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	for _, desired := range []swarm.TaskState{swarm.TaskStateRunning, swarm.TaskStateShutdown} {
		first := makeStateTask(base, desired, swarm.TaskStateRunning)
		second := makeStateTask(base, desired, swarm.TaskStateRunning)

		// Identical rank: neither replaces the other, so the first one listed is kept.
		if preferredTask(first, second) || preferredTask(second, first) {
			t.Errorf("desired=%s: tied tasks should not replace each other", desired)
		}
	}
}

func TestRetiredTask(t *testing.T) {
	tests := []struct {
		desired swarm.TaskState
		want    bool
	}{
		{swarm.TaskStateShutdown, true},
		{swarm.TaskStateFailed, true},
		{swarm.TaskStateRejected, true},
		{swarm.TaskStateRemove, true},
		{swarm.TaskStateOrphaned, true},
		{swarm.TaskStateComplete, false},
		{swarm.TaskStateRunning, false},
		{swarm.TaskStateReady, false},
		{"", false},
	}

	for _, tc := range tests {
		task := &swarm.Task{DesiredState: tc.desired}
		if got := retiredTask(task); got != tc.want {
			t.Errorf("retiredTask(desired=%q) = %v, want %v", tc.desired, got, tc.want)
		}
	}
}

func TestNewerThan_CreatedAt(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	newer := makeTask(base.Add(time.Second), time.Time{}, 1)
	older := makeTask(base, time.Time{}, 2)

	if !newerThan(newer, older) {
		t.Error("newer CreatedAt should win")
	}

	if newerThan(older, newer) {
		t.Error("older CreatedAt should lose")
	}
}

func TestNewerThan_CreatedAt_Equal_FallsBackToStatusTimestamp(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	a := makeTask(base, base.Add(time.Second), 1)
	b := makeTask(base, base, 1)

	if !newerThan(a, b) {
		t.Error("later StatusTimestamp should win when CreatedAt equal")
	}

	if newerThan(b, a) {
		t.Error("earlier StatusTimestamp should lose when CreatedAt equal")
	}
}

func TestNewerThan_CreatedAt_Equal_OneZeroStatusTS_FallsBackToVersion(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	// candidate has non-zero StatusTS; current has zero StatusTS.
	// Code falls through to Version.Index when one side is zero.
	a := makeTask(base, time.Time{}, 10)
	b := makeTask(base, time.Time{}, 5)

	if !newerThan(a, b) {
		t.Error("higher Version.Index should win when both StatusTS are zero")
	}

	if newerThan(b, a) {
		t.Error("lower Version.Index should lose")
	}
}

func TestNewerThan_AllEqual(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	a := makeTask(base, base, 5)
	b := makeTask(base, base, 5)
	// Equal Version.Index → newerThan returns false (not strictly newer).
	if newerThan(a, b) {
		t.Error("equal tasks should return false")
	}
}

func TestTaskCounter_IncAndGet(t *testing.T) {
	labels := map[string]string{"stack": "s", "service": "sv"}
	counter := newTaskCounter(labels)

	counter.inc("running")
	counter.inc("running")
	counter.inc("failed")

	if counter.states["running"] != 2 {
		t.Errorf("running = %v, want 2", counter.states["running"])
	}

	if counter.states["failed"] != 1 {
		t.Errorf("failed = %v, want 1", counter.states["failed"])
	}

	if counter.states["new"] != 0 {
		t.Errorf("new = %v, want 0", counter.states["new"])
	}
}

// ---- Docker-bound tests (use fakeDocker) ----

func TestPollReplicasState_DedupeBySlot_NewerWins(t *testing.T) {
	resetCollectorState(t)

	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	setServiceMetadata("svc1", &md)

	// Same desired state and neither running, so only creation time tells them apart.
	older := swarm.Task{
		Meta:         swarm.Meta{CreatedAt: base, Version: swarm.Version{Index: 1}},
		ServiceID:    "svc1",
		Slot:         1,
		DesiredState: swarm.TaskStateShutdown,
		Status:       swarm.TaskStatus{State: swarm.TaskStateFailed},
	}
	newer := swarm.Task{
		Meta: swarm.Meta{
			CreatedAt: base.Add(time.Second),
			Version:   swarm.Version{Index: 2},
		},
		ServiceID:    "svc1",
		Slot:         1,
		DesiredState: swarm.TaskStateShutdown,
		Status:       swarm.TaskStatus{State: swarm.TaskStateRejected},
	}

	fd := &fakeDocker{tasks: []swarm.Task{older, newer}}

	sc, err := PollReplicasState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	c, ok := sc["svc1"]
	if !ok {
		t.Fatal("expected entry for svc1")
	}
	// Only the newer (rejected) task should count.
	if c.states[string(swarm.TaskStateRejected)] != 1 {
		t.Errorf("rejected = %v, want 1", c.states[string(swarm.TaskStateRejected)])
	}

	if c.states[string(swarm.TaskStateFailed)] != 0 {
		t.Errorf(
			"failed = %v, want 0 (older task should be deduped)",
			c.states[string(swarm.TaskStateFailed)],
		)
	}
}

// TestPollReplicasState_NewerRetiredTask_DoesNotHideRunningTask reproduces issue 62: a
// start-first update whose new task failed leaves a newer retired task next to the older task
// that is still serving, in the same slot (replicated) or on the same node (global).
func TestPollReplicasState_NewerRetiredTask_DoesNotHideRunningTask(t *testing.T) {
	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)

	tests := []struct {
		name string
		mode string
		slot int
		node string
	}{
		{name: "replicated keyed by slot", mode: serviceModeReplicated, slot: 1},
		{name: "global keyed by node", mode: serviceModeGlobal, node: "node-a"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			resetCollectorState(t)

			md := makeTestMetadata("stack", "svc", tc.mode)
			setServiceMetadata("svc1", &md)

			serving := swarm.Task{
				Meta:         swarm.Meta{CreatedAt: base, Version: swarm.Version{Index: 1}},
				ServiceID:    "svc1",
				Slot:         tc.slot,
				NodeID:       tc.node,
				DesiredState: swarm.TaskStateRunning,
				Status:       swarm.TaskStatus{State: swarm.TaskStateRunning},
			}
			failedUpdate := swarm.Task{
				Meta: swarm.Meta{
					CreatedAt: base.Add(time.Hour),
					Version:   swarm.Version{Index: 2},
				},
				ServiceID:    "svc1",
				Slot:         tc.slot,
				NodeID:       tc.node,
				DesiredState: swarm.TaskStateShutdown,
				Status:       swarm.TaskStatus{State: swarm.TaskStateFailed},
			}

			// Both list orders: Docker does not guarantee one.
			for _, tasks := range [][]swarm.Task{{serving, failedUpdate}, {failedUpdate, serving}} {
				sc, err := PollReplicasState(context.Background(), &fakeDocker{tasks: tasks})
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}

				c, ok := sc["svc1"]
				if !ok {
					t.Fatal("expected entry for svc1")
				}

				if got := c.states[string(swarm.TaskStateRunning)]; got != 1 {
					t.Errorf("running = %v, want 1", got)
				}

				if got := c.states[string(swarm.TaskStateFailed)]; got != 0 {
					t.Errorf("failed = %v, want 0 (retired task must not be counted)", got)
				}
			}
		})
	}
}

func TestPollReplicasState_GlobalService_DedupeByNodeID(t *testing.T) {
	resetCollectorState(t)

	base := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	md := makeTestMetadata("", "glbsvc", serviceModeGlobal)
	setServiceMetadata("glb1", &md)

	// Two tasks on different nodes — both should count (different dedup keys).
	t1 := swarm.Task{
		Meta:      swarm.Meta{CreatedAt: base, Version: swarm.Version{Index: 1}},
		ServiceID: "glb1",
		NodeID:    "node-a",
		Status:    swarm.TaskStatus{State: swarm.TaskStateRunning},
	}
	t2 := swarm.Task{
		Meta:      swarm.Meta{CreatedAt: base, Version: swarm.Version{Index: 1}},
		ServiceID: "glb1",
		NodeID:    "node-b",
		Status:    swarm.TaskStatus{State: swarm.TaskStateRunning},
	}

	fd := &fakeDocker{tasks: []swarm.Task{t1, t2}}

	sc, err := PollReplicasState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	c, ok := sc["glb1"]
	if !ok {
		t.Fatal("expected entry for glb1")
	}

	if c.states[string(swarm.TaskStateRunning)] != 2 {
		t.Errorf("running = %v, want 2 (one per node)", c.states[string(swarm.TaskStateRunning)])
	}
}

func TestPollReplicasState_GoneService_Skipped(t *testing.T) {
	resetCollectorState(t)

	// "svc_gone" not in metadata cache → slow path → inspect returns not-found.
	task := swarm.Task{
		Meta:      swarm.Meta{CreatedAt: time.Now(), Version: swarm.Version{Index: 1}},
		ServiceID: "svc_gone",
		Slot:      1,
		Status:    swarm.TaskStatus{State: swarm.TaskStateRunning},
	}

	fd := &fakeDocker{
		tasks:             []swarm.Task{task},
		serviceInspectErr: errdefs.ErrNotFound,
	}

	sc, err := PollReplicasState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error (gone service should be skipped, not errored): %v", err)
	}

	if _, ok := sc["svc_gone"]; ok {
		t.Error("gone service should not appear in counter")
	}
}

func TestPollReplicasState_TaskListError(t *testing.T) {
	resetCollectorState(t)

	fd := &fakeDocker{taskListErr: errTaskListFailed}

	_, err := PollReplicasState(context.Background(), fd)
	if err == nil {
		t.Error("expected error from TaskList failure")
	}
}

func TestPollReplicasState_ServicesWithoutTasks_EmitZeroSeries(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	// A global service no node is eligible for: no tasks, desired 0.
	unscheduledGlobal := makeTestMetadata("s", "glb", serviceModeGlobal)
	setServiceMetadata("glb_empty", &unscheduledGlobal)
	setServiceDesiredReplicas("glb_empty", 0)

	// A replicated service whose tasks were never created: no tasks, desired 2.
	pendingReplicated := makeTestMetadata("s", "pending", serviceModeReplicated)
	setServiceMetadata("rep_pending", &pendingReplicated)
	setServiceDesiredReplicas("rep_pending", 2)

	// A service with a running task, to check it is left alone.
	running := makeTestMetadata("s", "ok", serviceModeReplicated)
	setServiceMetadata("rep_ok", &running)
	setServiceDesiredReplicas("rep_ok", 1)

	fd := &fakeDocker{tasks: []swarm.Task{{
		Meta:      swarm.Meta{CreatedAt: time.Now(), Version: swarm.Version{Index: 1}},
		ServiceID: "rep_ok",
		Slot:      1,
		Status:    swarm.TaskStatus{State: swarm.TaskStateRunning},
	}}}

	sc, err := PollReplicasState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(sc) != 3 {
		t.Fatalf("got %d services, want 3 (services without tasks included)", len(sc))
	}

	UpdateReplicasStateGauge(sc)

	gathered := gatherSeries(t, families)

	if got := len(familySeries(gathered, runningReplicasFQName)); got != 3 {
		t.Errorf("running_replicas series = %d, want 3", got)
	}

	if got := len(familySeries(gathered, atDesiredFQName)); got != 3 {
		t.Errorf("at_desired series = %d, want 3", got)
	}

	if got := len(familySeries(gathered, replicasStateFQName)); got != 3*len(knownTaskStates) {
		t.Errorf("replicas_state series = %d, want %d", got, 3*len(knownTaskStates))
	}

	cases := []struct {
		service, mode      string
		running, atDesired float64
	}{
		{"glb", serviceModeGlobal, 0, 1},
		{"pending", serviceModeReplicated, 0, 0},
		{"ok", serviceModeReplicated, 1, 1},
	}
	for _, tc := range cases {
		lbls := serviceLabels("s", tc.service, tc.mode)
		if got, found := gathered[seriesID(runningReplicasFQName, lbls)]; !found ||
			got != tc.running {
			t.Errorf(
				"[%s] running_replicas = %v (found %v), want %v",
				tc.service,
				got,
				found,
				tc.running,
			)
		}

		if got, found := gathered[seriesID(atDesiredFQName, lbls)]; !found || got != tc.atDesired {
			t.Errorf(
				"[%s] at_desired = %v (found %v), want %v",
				tc.service,
				got,
				found,
				tc.atDesired,
			)
		}
	}
}

func TestAddServicesWithoutTasks(t *testing.T) {
	resetCollectorState(t)

	md := makeTestMetadata("s", "empty", serviceModeReplicated)
	setServiceMetadata("svc_empty", &md)

	existing := newTaskCounter(serviceLabels("s", "busy", serviceModeReplicated))
	existing.inc(string(swarm.TaskStateRunning))
	sc := serviceCounter{"svc_busy": existing}

	// svc_removed is not in the metadata cache: it was removed while polling.
	addServicesWithoutTasks(sc, []string{"svc_busy", "svc_empty", "svc_removed"})

	if sc["svc_busy"].states[string(swarm.TaskStateRunning)] != 1 {
		t.Error("existing counter must not be replaced")
	}

	empty, ok := sc["svc_empty"]
	if !ok {
		t.Fatal("expected an empty counter for svc_empty")
	}

	if len(empty.states) != 0 {
		t.Errorf("svc_empty states = %v, want none", empty.states)
	}

	if empty.labels[labelService] != "empty" {
		t.Errorf("svc_empty labels = %v, want service=empty", empty.labels)
	}

	if _, ok := sc["svc_removed"]; ok {
		t.Error("uncached service must not be added")
	}
}

// ---- UpdateReplicasStateGauge ----

func TestUpdateReplicasStateGauge_AtDesired(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	md := makeTestMetadata("s", "sv", serviceModeReplicated)
	setServiceMetadata("svc1", &md)
	setServiceDesiredReplicas("svc1", 3)

	lbls := serviceLabels("s", "sv", serviceModeReplicated)
	tc := newTaskCounter(lbls)
	tc.inc(string(swarm.TaskStateRunning))
	tc.inc(string(swarm.TaskStateRunning))
	tc.inc(string(swarm.TaskStateRunning))
	sc := serviceCounter{"svc1": tc}

	UpdateReplicasStateGauge(sc)

	running, found := snapshotValue(
		t,
		families,
		runningReplicasFQName,
		serviceLabels("s", "sv", serviceModeReplicated),
	)
	if !found {
		t.Fatalf("running_replicas series missing")
	}

	if running != 3 {
		t.Errorf("running_replicas = %v, want 3", running)
	}

	atDesired, found := snapshotValue(
		t,
		families,
		atDesiredFQName,
		serviceLabels("s", "sv", serviceModeReplicated),
	)
	if !found {
		t.Fatalf("at_desired series missing")
	}

	if atDesired != 1 {
		t.Errorf("at_desired = %v, want 1 (running==desired)", atDesired)
	}
}

func TestUpdateReplicasStateGauge_NotAtDesired(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	md := makeTestMetadata("s", "sv", serviceModeReplicated)
	setServiceMetadata("svc1", &md)
	setServiceDesiredReplicas("svc1", 3)

	lbls := serviceLabels("s", "sv", serviceModeReplicated)
	tc := newTaskCounter(lbls)
	tc.inc(string(swarm.TaskStateRunning)) // only 1, desired=3
	sc := serviceCounter{"svc1": tc}

	UpdateReplicasStateGauge(sc)

	atDesired, found := snapshotValue(
		t,
		families,
		atDesiredFQName,
		serviceLabels("s", "sv", serviceModeReplicated),
	)
	if !found {
		t.Fatalf("at_desired series missing")
	}

	if atDesired != 0 {
		t.Errorf("at_desired = %v, want 0 (running != desired)", atDesired)
	}
}

func TestUpdateReplicasStateGauge_MissingDesiredCache_NoPanic(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	// svc_nodesired not in metadata cache — getServiceDesiredReplicas returns false.
	lbls := serviceLabels("s", "sv", serviceModeReplicated)
	tc := newTaskCounter(lbls)
	tc.inc(string(swarm.TaskStateRunning))
	sc := serviceCounter{"svc_nodesired": tc}

	// Must not panic.
	UpdateReplicasStateGauge(sc)

	atDesired, found := snapshotValue(
		t,
		families,
		atDesiredFQName,
		serviceLabels("s", "sv", serviceModeReplicated),
	)
	if !found {
		t.Fatalf("at_desired series missing")
	}

	if atDesired != 0 {
		t.Errorf("at_desired = %v, want 0 when desired cache missing", atDesired)
	}
}

func TestServiceCounter_GetCreatesLazily(t *testing.T) {
	sc := make(serviceCounter)
	labels := map[string]string{"stack": "s"}

	c1 := sc.get("svc1", labels)
	c1.inc("running")
	sc["svc1"] = c1

	c2 := sc.get("svc1", labels)
	if c2.states["running"] != 1 {
		t.Errorf("expected stored counter, got %v", c2.states["running"])
	}

	_ = sc.get("svc2", labels)
	if len(sc) != 2 {
		t.Errorf("expected 2 entries after lazy create, got %d", len(sc))
	}
}

// ---- Snapshot publication ----

// replicasStateCounters returns a serviceCounter with one service per name in stack "s", each
// with running tasks, after caching metadata and desired replicas for it.
func replicasStateCounters(t *testing.T, running, desired float64, names ...string) serviceCounter {
	t.Helper()

	counters := make(serviceCounter, len(names))

	for _, name := range names {
		serviceID := "id_" + name
		metadata := makeTestMetadata("s", name, serviceModeReplicated)
		setServiceMetadata(serviceID, &metadata)
		setServiceDesiredReplicas(serviceID, desired)

		counter := newTaskCounter(serviceLabels("s", name, serviceModeReplicated))
		for range int(running) {
			counter.inc(string(swarm.TaskStateRunning))
		}

		counters[serviceID] = counter
	}

	return counters
}

// expectedReplicasStateSeries gathers what counters publish, from a separate collector.
func expectedReplicasStateSeries(t *testing.T, counters serviceCounter) map[string]float64 {
	t.Helper()

	reference := newReplicasStateSnapshot(nil)

	metrics, buildErr := buildReplicasState(reference, counters).build()
	if buildErr != nil {
		t.Fatalf("build reference snapshot: %v", buildErr)
	}

	reference.publish(metrics)

	return gatherSeries(t, reference)
}

// TestUpdateReplicasStateGauge_BuildDoesNotPublish pins the fix for #72: while the next set is
// being built, a scrape still sees the whole previous set, never an empty or partial one.
func TestUpdateReplicasStateGauge_BuildDoesNotPublish(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	countersA := replicasStateCounters(t, 1, 1, "a1", "a2")
	countersB := replicasStateCounters(t, 2, 3, "b1")

	expectedA := expectedReplicasStateSeries(t, countersA)
	expectedB := expectedReplicasStateSeries(t, countersB)

	UpdateReplicasStateGauge(countersA)

	if gathered := gatherSeries(t, families); !maps.Equal(gathered, expectedA) {
		t.Fatalf("after publishing A gathered %v, want %v", gathered, expectedA)
	}

	builder := buildReplicasState(families, countersB)

	if gathered := gatherSeries(t, families); !maps.Equal(gathered, expectedA) {
		t.Fatalf("after building B gathered %v, want A %v", gathered, expectedA)
	}

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build B: %v", buildErr)
	}

	if gathered := gatherSeries(t, families); !maps.Equal(gathered, expectedA) {
		t.Fatalf("after build() of B gathered %v, want A %v", gathered, expectedA)
	}

	families.publish(metrics)

	if gathered := gatherSeries(t, families); !maps.Equal(gathered, expectedB) {
		t.Fatalf("after publishing B gathered %v, want %v", gathered, expectedB)
	}
}

// TestUpdateReplicasStateGauge_OnePublishUpdatesAllFamilies checks that replicas_state,
// running_replicas and at_desired all move to the new poll together.
func TestUpdateReplicasStateGauge_OnePublishUpdatesAllFamilies(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	UpdateReplicasStateGauge(replicasStateCounters(t, 1, 2, "svc"))
	UpdateReplicasStateGauge(replicasStateCounters(t, 2, 2, "svc"))

	gathered := gatherSeries(t, families)
	labels := serviceLabels("s", "svc", serviceModeReplicated)

	stateLabels := prometheus.Labels{labelState: string(swarm.TaskStateRunning)}
	maps.Copy(stateLabels, labels)

	checks := []struct {
		name string
		id   string
		want float64
	}{
		{name: "replicas_state running", id: seriesID(replicasStateFQName, stateLabels), want: 2},
		{name: "running_replicas", id: seriesID(runningReplicasFQName, labels), want: 2},
		{name: "at_desired", id: seriesID(atDesiredFQName, labels), want: 1},
	}

	for _, check := range checks {
		if got, found := gathered[check.id]; !found || got != check.want {
			t.Errorf("%s = %v (found %v), want %v", check.name, got, found, check.want)
		}
	}
}

// TestUpdateReplicasStateGauge_ConcurrentScrapes_SeeWholeSnapshots is -race coverage for
// updates and scrapes running together. It cannot reliably catch a partial publish on its own:
// that is pinned by TestUpdateReplicasStateGauge_BuildDoesNotPublish.
func TestUpdateReplicasStateGauge_ConcurrentScrapes_SeeWholeSnapshots(t *testing.T) {
	resetCollectorState(t)
	families := installReplicasStateGauges(t)

	countersA := replicasStateCounters(t, 1, 1, "a1", "a2", "a3")
	countersB := replicasStateCounters(t, 2, 3, "b1", "b2")

	expectedA := expectedReplicasStateSeries(t, countersA)
	expectedB := expectedReplicasStateSeries(t, countersB)

	UpdateReplicasStateGauge(countersA)

	stop := make(chan struct{})

	var updater sync.WaitGroup

	updater.Go(func() {
		for index := 0; ; index++ {
			select {
			case <-stop:
				return
			default:
			}

			if index%2 == 0 {
				UpdateReplicasStateGauge(countersB)
			} else {
				UpdateReplicasStateGauge(countersA)
			}
		}
	})

	defer func() {
		close(stop)
		updater.Wait()
	}()

	const scrapes = 200

	for range scrapes {
		gathered := gatherSeries(t, families)
		if !maps.Equal(gathered, expectedA) && !maps.Equal(gathered, expectedB) {
			t.Fatalf("scrape saw %v, want exactly snapshot A or B", gathered)
		}
	}
}
