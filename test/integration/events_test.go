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
	"net/http"
	"strings"
	"testing"
	"time"
)

const metricEventsReconnects = "swarm_exporter_events_reconnects_total"

// TestEvents_ReconnectResyncs checks that a service removed while the event stream was down, whose
// remove event is never replayed, is dropped once the stream reconnects. The replay cannot catch
// it, since the proxy brings the stream back without its history, and the periodic resync is
// minutes away: only the resync every reconnect requests can.
func TestEvents_ReconnectResyncs(t *testing.T) {
	const stack = "it-reconnect"

	kept := serviceKey{stack: stack, service: "kept"}
	doomed := serviceKey{stack: stack, service: "doomed"}

	deployService(t, stack, kept.service, &serviceOpts{replicas: 1})
	doomedID := deployService(t, stack, doomed.service, &serviceOpts{replicas: 1})

	proxy := startDockerProxy(t)
	baseURL := startExporterVia(t, proxy.dockerHost(), []serviceKey{kept, doomed})

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		return compareMetrics(scraped, stack, []metricWant{
			wantService(metricRunning, kept, 1),
			wantService(metricRunning, doomed, 1),
		})
	})

	connectsBefore := proxy.eventConnectCount()

	proxy.cutEventStreams()
	removeService(t, doomedID)
	proxy.restoreEventStreams()

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		if proxy.eventConnectCount() <= connectsBefore {
			return fmt.Errorf("event stream not reconnected yet: %w", errNotYet)
		}

		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		if reconnects := scraped.sum(metricEventsReconnects, nil); reconnects < 1 {
			return fmt.Errorf(
				"%s = %g, want at least 1: %w",
				metricEventsReconnects,
				reconnects,
				errNotYet,
			)
		}

		if scraped.hasService(metricDesired, doomed) {
			return fmt.Errorf(
				"%s still has a series for the removed service: %w",
				metricDesired,
				errNotYet,
			)
		}

		return compareMetrics(scraped, stack, []metricWant{wantService(metricRunning, kept, 1)})
	})
}

// TestSeed_FailedThenRecovers checks that a first resync that fails leaves the exporter unhealthy
// with no service series, and that the retried one seeds it once the Docker API recovers.
func TestSeed_FailedThenRecovers(t *testing.T) {
	const stack = "it-seed"

	web := serviceKey{stack: stack, service: "web"}
	deployService(t, stack, web.service, &serviceOpts{replicas: 1})

	proxy := startDockerProxy(t)
	proxy.setServiceListFailing(true)

	baseURL := startExporterServing(t, proxy.dockerHost())

	// At least two failures: the first resync, and a retry of it.
	eventually(t, 30*time.Second, func(context.Context) error {
		if failures := proxy.serviceListFailureCount(); failures < 2 {
			return fmt.Errorf(
				"%d failed service lists, want a retried seed: %w",
				failures,
				errNotYet,
			)
		}

		return nil
	})

	body, status, err := httpGet(testCtx(t), baseURL+"/healthz")
	if err != nil {
		t.Fatal(err)
	}

	if status != http.StatusServiceUnavailable ||
		!strings.Contains(string(body), "initial resync not completed") {
		t.Errorf(
			"/healthz during a failing seed = %d %q, want 503 \"initial resync not completed\"",
			status,
			body,
		)
	}

	scraped, err := scrapeMetrics(testCtx(t), baseURL)
	if err != nil {
		t.Fatal(err)
	}

	if scraped.hasService(metricDesired, web) || scraped.hasService(metricRunning, web) {
		t.Error("service series published before the first resync succeeded")
	}

	proxy.setServiceListFailing(false)

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		readyErr := readiness(ctx, baseURL, []serviceKey{web})
		if readyErr != nil {
			return readyErr
		}

		scraped, scrapeErr := scrapeMetrics(ctx, baseURL)
		if scrapeErr != nil {
			return scrapeErr
		}

		return compareMetrics(scraped, stack, []metricWant{wantService(metricRunning, web, 1)})
	})
}
