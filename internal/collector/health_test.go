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

	"github.com/prometheus/client_golang/prometheus/testutil"
)

// resetHealthState clears the poll timestamp and installs a reconciler whose first resync
// completed, so the tests below see only the poll-freshness checks unless they change it.
func resetHealthState(t *testing.T) {
	t.Helper()

	previous := activeReconciler.Load()

	t.Cleanup(func() {
		lastPollSuccessUnixNano.Store(0)
		activeReconciler.Store(previous)
	})

	lastPollSuccessUnixNano.Store(0)

	ready := NewReconciler(&fakeDocker{})
	ready.resyncCompleted = ready.resyncRequested
	ready.resyncOutstandingSince = time.Time{}
	activeReconciler.Store(ready)
}

func TestHealthSnapshot_NeverPolled(t *testing.T) {
	resetHealthState(t)

	healthy, reason := HealthSnapshot(10*time.Second, time.Now())
	if healthy {
		t.Error("expected unhealthy when never polled")
	}

	if reason != "no successful poll yet" {
		t.Errorf("reason = %q, want %q", reason, "no successful poll yet")
	}
}

func TestHealthSnapshot_RecentPoll_IsHealthy(t *testing.T) {
	resetHealthState(t)

	now := time.Now()
	MarkPollOK(now.Add(-1 * time.Second))

	healthy, reason := HealthSnapshot(10*time.Second, now)
	if !healthy {
		t.Errorf("expected healthy for recent poll, reason: %q", reason)
	}
}

func TestHealthSnapshot_StalePoll_IsUnhealthy(t *testing.T) {
	resetHealthState(t)

	pollDelay := 10 * time.Second
	window := 3 * pollDelay // 30s
	now := time.Now()
	MarkPollOK(now.Add(-(window + time.Second)))

	healthy, reason := HealthSnapshot(pollDelay, now)
	if healthy {
		t.Error("expected unhealthy for stale poll")
	}

	if reason == "" {
		t.Error("expected non-empty reason")
	}
}

func TestHealthSnapshot_ThirtySecondFloor(t *testing.T) {
	resetHealthState(t)
	// pollDelay=1s → 3*1=3s, but floor is 30s → window=30s.
	// A poll 20 seconds old should still be healthy.
	pollDelay := 1 * time.Second
	now := time.Now()
	MarkPollOK(now.Add(-20 * time.Second))

	healthy, reason := HealthSnapshot(pollDelay, now)
	if !healthy {
		t.Errorf("expected healthy at 20s with 30s floor, reason: %q", reason)
	}
}

func TestHealthSnapshot_ThirtySecondFloor_StaleAfter30s(t *testing.T) {
	resetHealthState(t)

	pollDelay := 1 * time.Second
	now := time.Now()
	MarkPollOK(now.Add(-31 * time.Second))

	healthy, _ := HealthSnapshot(pollDelay, now)
	if healthy {
		t.Error("expected unhealthy at 31s with 30s floor")
	}
}

func TestHealthGauge_EvaluatedAtScrape(t *testing.T) {
	resetHealthState(t)

	pollDelay := 10 * time.Second
	gauge := newHealthGauge(pollDelay)

	if got := testutil.ToFloat64(gauge); got != 0 {
		t.Errorf("health = %v before any poll, want 0", got)
	}

	MarkPollOK(time.Now())

	if got := testutil.ToFloat64(gauge); got != 1 {
		t.Errorf("health = %v right after a successful poll, want 1", got)
	}

	// No poller runs here: only the scrape-time evaluation can notice the poll went stale.
	MarkPollOK(time.Now().Add(-time.Hour))

	if got := testutil.ToFloat64(gauge); got != 0 {
		t.Errorf("health = %v with a stale poll, want 0", got)
	}
}

func TestPollAndPublishReplicasState_SuccessMovesTimestamp(t *testing.T) {
	resetHealthState(t)
	resetCollectorState(t)
	installReplicasStateGauges(t)

	metadata := makeTestMetadata("s", "web", serviceModeReplicated)
	setServiceMetadata("svc", &metadata)
	setServiceDesiredReplicas("svc", 1)

	before := time.Now()

	publishErr := PollAndPublishReplicasState(context.Background(), &fakeDocker{})
	if publishErr != nil {
		t.Fatalf("PollAndPublishReplicasState: %v", publishErr)
	}

	if got := lastPollSuccessUnixNano.Load(); got < before.UnixNano() {
		t.Errorf("poll timestamp = %d, want at least %d (the publish time)", got, before.UnixNano())
	}
}

func TestPollAndPublishReplicasState_FailedBuildLeavesTimestamp(t *testing.T) {
	resetHealthState(t)
	resetCollectorState(t)
	installReplicasStateGauges(t)

	// The installed families carry no custom label, so a service with one fails the build.
	metadata := makeTestMetadata("s", "web", serviceModeReplicated)
	metadata.customLabels = map[string]string{"team": "a"}
	setServiceMetadata("svc", &metadata)
	setServiceDesiredReplicas("svc", 1)

	previous := time.Now().Add(-time.Minute).UnixNano()
	lastPollSuccessUnixNano.Store(previous)

	publishErr := PollAndPublishReplicasState(context.Background(), &fakeDocker{})
	if !errors.Is(publishErr, errSnapshotLabelsMismatch) {
		t.Fatalf("err = %v, want a snapshot build failure", publishErr)
	}

	if got := lastPollSuccessUnixNano.Load(); got != previous {
		t.Errorf("poll timestamp = %d after a failed build, want it unchanged at %d", got, previous)
	}
}

func TestPollAndPublishReplicasState_FailedPollLeavesTimestamp(t *testing.T) {
	resetHealthState(t)
	resetCollectorState(t)
	installReplicasStateGauges(t)

	previous := time.Now().Add(-time.Minute).UnixNano()
	lastPollSuccessUnixNano.Store(previous)

	publishErr := PollAndPublishReplicasState(
		context.Background(),
		&fakeDocker{taskListErr: errTaskListFailed},
	)
	if !errors.Is(publishErr, errTaskListFailed) {
		t.Fatalf("err = %v, want the task list failure", publishErr)
	}

	if got := lastPollSuccessUnixNano.Load(); got != previous {
		t.Errorf("poll timestamp = %d after a failed poll, want it unchanged at %d", got, previous)
	}
}
