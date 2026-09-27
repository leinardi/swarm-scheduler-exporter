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
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
)

var errSeedUnavailable = errors.New("service list unavailable")

// lifecycleDocker is a collector.DockerAPI for the wiring tests: an empty swarm whose ServiceList
// fails its first seedFailures calls, and whose event stream stays open until its context ends.
type lifecycleDocker struct {
	seedFailures int

	mu                 sync.Mutex
	serviceListCalls   int
	seeded             bool
	taskListCalls      int
	taskListBeforeSeed bool
	eventsCalls        int
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
	if d.serviceListCalls <= d.seedFailures {
		return client.ServiceListResult{}, errSeedUnavailable
	}

	d.seeded = true

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
	if !d.seeded {
		d.taskListBeforeSeed = true
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

func (d *lifecycleDocker) snapshot() (serviceListCalls, taskListCalls, eventsCalls int, taskListBeforeSeed bool) {
	d.mu.Lock()
	defer d.mu.Unlock()

	return d.serviceListCalls, d.taskListCalls, d.eventsCalls, d.taskListBeforeSeed
}

// startWorkers starts the event listener and the poller the way run does.
func startWorkers(ctx context.Context, dockerAPI collector.DockerAPI) *sync.WaitGroup {
	var workerGroup sync.WaitGroup

	seeded := make(chan struct{})
	startEventListener(ctx, &workerGroup, dockerAPI, seeded)
	startPoller(ctx, &workerGroup, dockerAPI, time.Hour, seeded)

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

func TestWorkers_SeedRetryThenPollAndListenOnce(t *testing.T) {
	dockerAPI := &lifecycleDocker{seedFailures: 1}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := startWorkers(ctx, dockerAPI)

	waitUntil(t, "the first poll after the seed", func() bool {
		_, taskListCalls, _, _ := dockerAPI.snapshot()

		return taskListCalls > 0
	})
	waitUntil(t, "the event stream", func() bool {
		_, _, eventsCalls, _ := dockerAPI.snapshot()

		return eventsCalls > 0
	})

	cancel()
	waitWorkers(t, workerGroup)

	serviceListCalls, _, eventsCalls, taskListBeforeSeed := dockerAPI.snapshot()

	if taskListBeforeSeed {
		t.Error("the poller listed tasks before the seed succeeded")
	}

	if serviceListCalls != 2 {
		t.Errorf("ServiceList calls = %d, want 2 (one failed seed, one retry)", serviceListCalls)
	}

	if eventsCalls != 1 {
		t.Errorf("event streams opened = %d, want exactly 1", eventsCalls)
	}
}

func TestWorkers_CancelBeforeSeedStopsBoth(t *testing.T) {
	dockerAPI := &lifecycleDocker{seedFailures: 1 << 30}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	workerGroup := startWorkers(ctx, dockerAPI)

	waitUntil(t, "the first seed attempt", func() bool {
		serviceListCalls, _, _, _ := dockerAPI.snapshot()

		return serviceListCalls > 0
	})

	// The listener is now waiting out its backoff and the poller is waiting for the seed.
	cancel()
	waitWorkers(t, workerGroup)

	_, taskListCalls, eventsCalls, _ := dockerAPI.snapshot()

	if taskListCalls != 0 {
		t.Errorf("TaskList calls = %d, want 0: the seed never succeeded", taskListCalls)
	}

	if eventsCalls != 0 {
		t.Errorf("event streams opened = %d, want 0: the seed never succeeded", eventsCalls)
	}
}
