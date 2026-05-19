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
	"testing"
	"time"

	"github.com/docker/docker/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func TestUpdateStateFromSwarm(t *testing.T) {
	cases := []struct {
		input swarm.UpdateState
		want  string
	}{
		{swarm.UpdateStateUpdating, updateStateUpdating},
		{swarm.UpdateStateCompleted, updateStateCompleted},
		{swarm.UpdateStatePaused, updateStatePaused},
		{swarm.UpdateStateRollbackPaused, updateStatePaused},
		{swarm.UpdateStateRollbackStarted, updateStateRollbackStarted},
		{swarm.UpdateStateRollbackCompleted, updateStateRollbackCompleted},
		{"unknown_state", updateStatePaused},
	}
	for _, tc := range cases {
		got := updateStateFromSwarm(tc.input)
		if got != tc.want {
			t.Errorf("updateStateFromSwarm(%q) = %q, want %q", tc.input, got, tc.want)
		}
	}
}

func TestCloneLabelsWithState(t *testing.T) {
	base := prometheus.Labels{"stack": "s", "service": "sv"}
	got := cloneLabelsWithState(base, "updating")

	if got[labelState] != "updating" {
		t.Errorf("state label = %q, want 'updating'", got[labelState])
	}

	if got["stack"] != "s" || got["service"] != "sv" {
		t.Error("base labels should be copied")
	}

	// Original must not be mutated.
	if _, exists := base[labelState]; exists {
		t.Error("original base labels should not be mutated")
	}
}

func TestCloneLabelsWithState_DoesNotMutateOriginal(t *testing.T) {
	base := prometheus.Labels{"k": "v"}
	_ = cloneLabelsWithState(base, "state1")

	_ = cloneLabelsWithState(base, "state2")
	if len(base) != 1 {
		t.Errorf("original labels mutated: len=%d", len(base))
	}
}

// ---- Gauge-level tests for UpdateServiceUpdateMetricsForService ----

func TestUpdateServiceUpdateMetricsForService_UpdatingState(t *testing.T) {
	stateG, _, _ := installServiceUpdateGauges(t)

	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	ts := time.Now()
	svc := &swarm.Service{
		UpdateStatus: &swarm.UpdateStatus{
			State:     swarm.UpdateStateUpdating,
			StartedAt: &ts,
		},
	}

	UpdateServiceUpdateMetricsForService(svc, &md)

	baseLbls := serviceLabels("stack", "svc", serviceModeReplicated)
	for _, state := range knownStates {
		lbls := cloneLabelsWithState(baseLbls, state)
		got := testutil.ToFloat64(stateG.With(lbls))

		want := 0.0
		if state == updateStateUpdating {
			want = 1.0
		}

		if got != want {
			t.Errorf("state=%q: value=%v, want %v", state, got, want)
		}
	}
}

func TestUpdateServiceUpdateMetricsForService_NilUpdateStatus_DefaultsCompleted(t *testing.T) {
	stateG, _, _ := installServiceUpdateGauges(t)

	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	svc := &swarm.Service{UpdateStatus: nil}

	UpdateServiceUpdateMetricsForService(svc, &md)

	baseLbls := serviceLabels("stack", "svc", serviceModeReplicated)
	completedLbls := cloneLabelsWithState(baseLbls, updateStateCompleted)

	got := testutil.ToFloat64(stateG.With(completedLbls))
	if got != 1.0 {
		t.Errorf("completed = %v, want 1.0 when UpdateStatus is nil", got)
	}
}

func TestUpdateServiceUpdateMetricsForService_TimestampsPopulated(t *testing.T) {
	_, startedTS, completedTS := installServiceUpdateGauges(t)

	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	started := time.Unix(1_700_000_000, 0)
	completed := time.Unix(1_700_000_060, 0)
	svc := &swarm.Service{
		UpdateStatus: &swarm.UpdateStatus{
			State:       swarm.UpdateStateCompleted,
			StartedAt:   &started,
			CompletedAt: &completed,
		},
	}

	UpdateServiceUpdateMetricsForService(svc, &md)

	baseLbls := serviceLabels("stack", "svc", serviceModeReplicated)
	gotStarted := testutil.ToFloat64(startedTS.With(baseLbls))
	gotCompleted := testutil.ToFloat64(completedTS.With(baseLbls))

	if gotStarted != float64(started.Unix()) {
		t.Errorf("started_ts = %v, want %v", gotStarted, float64(started.Unix()))
	}

	if gotCompleted != float64(completed.Unix()) {
		t.Errorf("completed_ts = %v, want %v", gotCompleted, float64(completed.Unix()))
	}
}

func TestClearServiceUpdateMetrics_DeletesSeries(t *testing.T) {
	stateG, startedTS, completedTS := installServiceUpdateGauges(t)

	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	svc := &swarm.Service{UpdateStatus: nil}

	UpdateServiceUpdateMetricsForService(svc, &md)

	// Verify series exist before clear.
	if testutil.CollectAndCount(stateG) == 0 {
		t.Fatal("expected some series before clear")
	}

	ClearServiceUpdateMetrics(&md)

	// After clear all series for this service should be gone.
	if testutil.CollectAndCount(stateG) != 0 {
		t.Errorf(
			"expected 0 series after ClearServiceUpdateMetrics, got %d",
			testutil.CollectAndCount(stateG),
		)
	}

	if testutil.CollectAndCount(startedTS) != 0 {
		t.Errorf(
			"expected 0 started_ts series after clear, got %d",
			testutil.CollectAndCount(startedTS),
		)
	}

	if testutil.CollectAndCount(completedTS) != 0 {
		t.Errorf(
			"expected 0 completed_ts series after clear, got %d",
			testutil.CollectAndCount(completedTS),
		)
	}
}
