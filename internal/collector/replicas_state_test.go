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
	"testing"
	"time"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
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

	older := swarm.Task{
		Meta:      swarm.Meta{CreatedAt: base, Version: swarm.Version{Index: 1}},
		ServiceID: "svc1",
		Slot:      1,
		Status:    swarm.TaskStatus{State: swarm.TaskStateRunning},
	}
	newer := swarm.Task{
		Meta:      swarm.Meta{CreatedAt: base.Add(time.Second), Version: swarm.Version{Index: 2}},
		ServiceID: "svc1",
		Slot:      1,
		Status:    swarm.TaskStatus{State: swarm.TaskStateFailed},
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
	// Only the newer (failed) task should count.
	if c.states[string(swarm.TaskStateFailed)] != 1 {
		t.Errorf("failed = %v, want 1", c.states[string(swarm.TaskStateFailed)])
	}

	if c.states[string(swarm.TaskStateRunning)] != 0 {
		t.Errorf(
			"running = %v, want 0 (older task should be deduped)",
			c.states[string(swarm.TaskStateRunning)],
		)
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

// ---- UpdateReplicasStateGauge ----

func TestUpdateReplicasStateGauge_AtDesired(t *testing.T) {
	resetCollectorState(t)
	rrg, adg := installReplicasStateGauges(t)

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

	running := testutil.ToFloat64(rrg.With(prometheus.Labels{
		labelStack: "s", labelService: "sv", labelServiceMode: serviceModeReplicated,
		labelDisplayName: displayName("s", "sv"),
	}))
	if running != 3 {
		t.Errorf("running_replicas = %v, want 3", running)
	}

	atDesired := testutil.ToFloat64(adg.With(prometheus.Labels{
		labelStack: "s", labelService: "sv", labelServiceMode: serviceModeReplicated,
		labelDisplayName: displayName("s", "sv"),
	}))
	if atDesired != 1 {
		t.Errorf("at_desired = %v, want 1 (running==desired)", atDesired)
	}
}

func TestUpdateReplicasStateGauge_NotAtDesired(t *testing.T) {
	resetCollectorState(t)
	_, adg := installReplicasStateGauges(t)

	md := makeTestMetadata("s", "sv", serviceModeReplicated)
	setServiceMetadata("svc1", &md)
	setServiceDesiredReplicas("svc1", 3)

	lbls := serviceLabels("s", "sv", serviceModeReplicated)
	tc := newTaskCounter(lbls)
	tc.inc(string(swarm.TaskStateRunning)) // only 1, desired=3
	sc := serviceCounter{"svc1": tc}

	UpdateReplicasStateGauge(sc)

	atDesired := testutil.ToFloat64(adg.With(prometheus.Labels{
		labelStack: "s", labelService: "sv", labelServiceMode: serviceModeReplicated,
		labelDisplayName: displayName("s", "sv"),
	}))
	if atDesired != 0 {
		t.Errorf("at_desired = %v, want 0 (running != desired)", atDesired)
	}
}

func TestUpdateReplicasStateGauge_MissingDesiredCache_NoPanic(t *testing.T) {
	resetCollectorState(t)
	_, adg := installReplicasStateGauges(t)

	// svc_nodesired not in metadata cache — getServiceDesiredReplicas returns false.
	lbls := serviceLabels("s", "sv", serviceModeReplicated)
	tc := newTaskCounter(lbls)
	tc.inc(string(swarm.TaskStateRunning))
	sc := serviceCounter{"svc_nodesired": tc}

	// Must not panic.
	UpdateReplicasStateGauge(sc)

	atDesired := testutil.ToFloat64(adg.With(prometheus.Labels{
		labelStack: "s", labelService: "sv", labelServiceMode: serviceModeReplicated,
		labelDisplayName: displayName("s", "sv"),
	}))
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
