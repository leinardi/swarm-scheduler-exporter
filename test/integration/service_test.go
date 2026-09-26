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
	"strings"
	"testing"
	"time"
)

// TestService_Removed checks that removing a service deletes every one of its series rather than
// leaving them at 0: a stale zero would read as "service down" forever.
func TestService_Removed(t *testing.T) {
	const stack = "it-remove"

	doomed := serviceKey{stack: stack, service: "doomed"}
	serviceID := deployService(t, stack, doomed.service, &serviceOpts{replicas: 1})
	baseURL := startExporter(t, []serviceKey{doomed})

	families := []string{
		metricDesired, metricRunning, metricAtDesired, metricSchedulable, metricReplicasState,
		metricUpdateState, metricUpdateStarted, metricUpdateComplete,
	}

	// Every family must be there first, or its absence afterwards would prove nothing.
	eventually(t, 60*time.Second, func(ctx context.Context) error {
		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		for _, family := range families {
			if !scraped.hasService(family, doomed) {
				return fmt.Errorf("no %s series yet for %s: %w", family, doomed.service, errNotYet)
			}
		}

		return compareMetrics(scraped, stack, []metricWant{wantService(metricRunning, doomed, 1)})
	})

	removeService(t, serviceID)

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		var left []string

		for _, smp := range scraped.samples {
			if labelsInclude(smp.labels, doomed.labels()) {
				left = append(left, formatSample(smp))
			}
		}

		if len(left) > 0 {
			return fmt.Errorf(
				"%d series left for the removed service:\n  %s: %w",
				len(left),
				strings.Join(left, "\n  "),
				errNotYet,
			)
		}

		return nil
	})
}
