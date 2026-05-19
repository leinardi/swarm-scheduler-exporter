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
)

func resetHealthState(t *testing.T) {
	t.Helper()
	t.Cleanup(func() { lastPollSuccessUnixNano.Store(0) })
	lastPollSuccessUnixNano.Store(0)
}

func TestHealthSnapshot_NeverPolled(t *testing.T) {
	resetHealthState(t)

	healthy, reason := HealthSnapshot(10*time.Second, time.Now())
	if healthy {
		t.Error("expected unhealthy when never polled")
	}

	if reason == "" {
		t.Error("expected non-empty reason when never polled")
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
