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

package main

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/client"
	"github.com/prometheus/client_golang/prometheus"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
	"github.com/leinardi/swarm-scheduler-exporter/internal/server"
)

var errListUnavailable = errors.New("service list unavailable")

// lifecycleDocker is a collector.DockerAPI for the wiring tests: an empty swarm whose ServiceList
// fails its first resyncFailures calls, and whose event stream stays open until its context ends.
type lifecycleDocker struct {
	resyncFailures int

	mu                   sync.Mutex
	serviceListCalls     int
	resynced             bool
	taskListCalls        int
	taskListBeforeResync bool
	eventsCalls          int
}

var _ collector.DockerAPI = (*lifecycleDocker)(nil)

func (*lifecycleDocker) NodeList(
	context.Context,
	client.NodeListOptions,
) (client.NodeListResult, error) {
	return client.NodeListResult{}, nil
}

func (d *lifecycleDocker) ServiceList(
	context.Context,
	client.ServiceListOptions,
) (client.ServiceListResult, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.serviceListCalls++
	if d.serviceListCalls <= d.resyncFailures {
		return client.ServiceListResult{}, errListUnavailable
	}

	d.resynced = true

	return client.ServiceListResult{}, nil
}

func (*lifecycleDocker) ServiceInspect(
	context.Context,
	string,
	client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	return client.ServiceInspectResult{}, nil
}

func (d *lifecycleDocker) TaskList(
	context.Context,
	client.TaskListOptions,
) (client.TaskListResult, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	d.taskListCalls++
	if !d.resynced {
		d.taskListBeforeResync = true
	}

	return client.TaskListResult{}, nil
}

func (*lifecycleDocker) ContainerList(
	context.Context,
	client.ContainerListOptions,
) (client.ContainerListResult, error) {
	return client.ContainerListResult{}, nil
}

func (*lifecycleDocker) ContainerInspect(
	context.Context,
	string,
	client.ContainerInspectOptions,
) (client.ContainerInspectResult, error) {
	return client.ContainerInspectResult{}, nil
}

func (d *lifecycleDocker) Events(context.Context, client.EventsListOptions) client.EventsResult {
	d.mu.Lock()
	d.eventsCalls++
	d.mu.Unlock()

	// Never written: the listener only leaves the stream when its context ends.
	return client.EventsResult{Messages: make(chan events.Message), Err: make(chan error)}
}

func (d *lifecycleDocker) snapshot() (serviceListCalls, taskListCalls, eventsCalls int, taskListBeforeResync bool) {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.serviceListCalls, d.taskListCalls, d.eventsCalls, d.taskListBeforeResync
}

// runWorkers starts the reconciler, the event listener and the poller the way run does.
func runWorkers(ctx context.Context, dockerAPI collector.DockerAPI) *sync.WaitGroup {
	var workerGroup sync.WaitGroup

	startWorkers(ctx, &workerGroup, dockerAPI, time.Hour)

	return &workerGroup
}

// waitUntil polls cond until it holds, failing the test with what if it does not within 5s.
func waitUntil(t *testing.T, what string, cond func() bool) {
	t.Helper()

	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()

	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()

	for !cond() {
		select {
		case <-deadline.C:
			t.Fatalf("timed out waiting for %s", what)
		case <-ticker.C:
		}
	}
}

// waitWorkers waits for workerGroup, failing the test if the workers do not return within 5s.
func waitWorkers(t *testing.T, workerGroup *sync.WaitGroup) {
	t.Helper()

	done := make(chan struct{})

	go func() {
		workerGroup.Wait()
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("workers did not return after cancellation")
	}
}

func TestWorkers_FirstResyncRetryThenPollAndListenOnce(t *testing.T) {
	dockerAPI := &lifecycleDocker{resyncFailures: 1}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := runWorkers(ctx, dockerAPI)

	waitUntil(t, "the first poll after the first resync", func() bool {
		_, taskListCalls, _, _ := dockerAPI.snapshot()

		return taskListCalls > 0
	})
	waitUntil(t, "the event stream", func() bool {
		_, _, eventsCalls, _ := dockerAPI.snapshot()

		return eventsCalls > 0
	})

	cancel()
	waitWorkers(t, workerGroup)

	serviceListCalls, _, eventsCalls, taskListBeforeResync := dockerAPI.snapshot()

	if taskListBeforeResync {
		t.Error("the poller listed tasks before the first resync succeeded")
	}

	if serviceListCalls != 2 {
		t.Errorf("ServiceList calls = %d, want 2 (one failed resync, one retry)", serviceListCalls)
	}

	if eventsCalls != 1 {
		t.Errorf("event streams opened = %d, want exactly 1", eventsCalls)
	}
}

func TestWorkers_CancelBeforeFirstResyncStopsPoller(t *testing.T) {
	dockerAPI := &lifecycleDocker{resyncFailures: 1 << 30}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := runWorkers(ctx, dockerAPI)

	waitUntil(t, "the first resync attempt", func() bool {
		serviceListCalls, _, _, _ := dockerAPI.snapshot()

		return serviceListCalls > 0
	})

	// The reconciler is now waiting out its backoff and the poller is waiting for the first
	// resync; the event stream is open, since it must be before the resync lists anything.
	cancel()
	waitWorkers(t, workerGroup)

	_, taskListCalls, eventsCalls, _ := dockerAPI.snapshot()

	if taskListCalls != 0 {
		t.Errorf("TaskList calls = %d, want 0: the first resync never succeeded", taskListCalls)
	}

	if eventsCalls > 1 {
		t.Errorf("event streams opened = %d, want at most 1", eventsCalls)
	}
}

// configureHealthMetrics registers the health gauge and the replicas-state families on a private
// registry, so a poll that runs publishes and marks health the way it does in run, and clears
// the process-wide poll timestamp before and after the test.
func configureHealthMetrics(t *testing.T, pollDelay time.Duration) *prometheus.Registry {
	t.Helper()

	registry := prometheus.NewRegistry()
	previousRegisterer := prometheus.DefaultRegisterer
	prometheus.DefaultRegisterer = registry

	// time.Unix(0, 0).UnixNano() is 0, which HealthSnapshot reads as "never polled".
	collector.MarkPollOK(time.Unix(0, 0))

	t.Cleanup(func() {
		prometheus.DefaultRegisterer = previousRegisterer

		collector.MarkPollOK(time.Unix(0, 0))
	})

	collector.ConfigureHealthGauges("test", "none", "unknown", pollDelay)
	collector.ConfigureReplicasStateGauge()

	return registry
}

// getHealthz serves one GET /healthz with the handler run wires, and returns the recorded response.
func getHealthz(t *testing.T, pollDelay time.Duration) *httptest.ResponseRecorder {
	t.Helper()

	httpMux := server.NewMuxWithHealth(func() (bool, string) {
		return collector.HealthSnapshot(pollDelay, time.Now())
	})

	recorder := httptest.NewRecorder()
	httpMux.ServeHTTP(
		recorder,
		httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/healthz", nil),
	)

	return recorder
}

func TestWorkers_PermanentResyncFailureIsUnhealthy(t *testing.T) {
	pollDelay := time.Second
	registry := configureHealthMetrics(t, pollDelay)

	dockerAPI := &lifecycleDocker{resyncFailures: 1 << 30}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := runWorkers(ctx, dockerAPI)

	// A second attempt comes after the first backoff. A poller that did not wait for the first
	// resync polls as soon as it starts, so by then it would have published a poll.
	waitUntil(t, "a second failed resync attempt", func() bool {
		serviceListCalls, _, _, _ := dockerAPI.snapshot()

		return serviceListCalls > 1
	})

	recorder := getHealthz(t, pollDelay)

	if recorder.Code != http.StatusServiceUnavailable {
		t.Errorf("/healthz status = %d, want %d", recorder.Code, http.StatusServiceUnavailable)
	}

	if body := recorder.Body.String(); body != "initial resync not completed\n" {
		t.Errorf("/healthz body = %q, want the not-ready reason", body)
	}

	if got := gatheredValue(t, registry, "swarm_exporter_health"); got != 0 {
		t.Errorf("swarm_exporter_health = %v, want 0", got)
	}

	cancel()
	waitWorkers(t, workerGroup)
}

func TestWorkers_ResyncThenPublishIsHealthy(t *testing.T) {
	pollDelay := time.Second
	registry := configureHealthMetrics(t, pollDelay)

	dockerAPI := &lifecycleDocker{resyncFailures: 1}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := runWorkers(ctx, dockerAPI)

	waitUntil(t, "/healthz to report healthy after the retried resync", func() bool {
		return getHealthz(t, pollDelay).Code == http.StatusOK
	})

	if got := gatheredValue(t, registry, "swarm_exporter_health"); got != 1 {
		t.Errorf("swarm_exporter_health = %v, want 1", got)
	}

	cancel()
	waitWorkers(t, workerGroup)
}

// gatheredValue gathers registry and returns the value of the single series of the gauge name.
func gatheredValue(t *testing.T, registry *prometheus.Registry, name string) float64 {
	t.Helper()

	families, gatherErr := registry.Gather()
	if gatherErr != nil {
		t.Fatalf("gather: %v", gatherErr)
	}

	for _, family := range families {
		if family.GetName() == name && len(family.GetMetric()) == 1 {
			return family.GetMetric()[0].GetGauge().GetValue()
		}
	}

	t.Fatalf("no single-series gauge %s gathered", name)

	return 0
}
