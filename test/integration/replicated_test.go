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
)

// TestReplicated_Scale scales a replicated service 1 → 3 → 0. desired, running and at_desired
// follow, and at 0 the service keeps its series (at 0, not deleted) although it has no task left
// to count.
func TestReplicated_Scale(t *testing.T) {
	const stack = "it-scale"

	web := serviceKey{stack: stack, service: "web"}
	serviceID := deployService(t, stack, web.service, &serviceOpts{replicas: 1})
	baseURL := startExporter(t, []serviceKey{web})

	expectReplicas := func(count float64) {
		t.Helper()

		eventually(t, 60*time.Second, metricsMatch(
			baseURL,
			stack,
			wantService(metricDesired, web, count),
			wantService(metricRunning, web, count),
			wantService(metricAtDesired, web, 1),
			want(
				metricReplicasState,
				map[string]string{labelStack: stack, labelService: web.service, "state": "running"},
				count,
			),
		))
	}

	expectReplicas(1)

	scaleService(t, serviceID, 3)
	expectReplicas(3)

	scaleService(t, serviceID, 0)

	// Scaled-down tasks linger in shutdown until Swarm reaps them, and while they exist the
	// exporter counts them the ordinary way. Only a poll that starts after the last one is gone
	// exercises the service-without-tasks path.
	eventually(t, 60*time.Second, func(ctx context.Context) error {
		tasks, err := listTasks(ctx, serviceID)
		if err != nil {
			return err
		}

		if len(tasks) > 0 {
			return fmt.Errorf(
				"scaled-down tasks not reaped yet:\n%s: %w",
				describeTasks(tasks),
				errNotYet,
			)
		}

		return nil
	})

	waitForFreshPoll(t, baseURL)
	expectReplicas(0)
}
