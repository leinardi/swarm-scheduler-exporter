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
	"sync"

	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/moby/moby/client"
)

// fakeDocker implements DockerAPI for unit tests.
// All fields are read-only after construction; call counts are guarded by mu.
type fakeDocker struct {
	nodes      []swarm.Node
	services   []swarm.Service
	tasks      []swarm.Task
	containers []container.Summary
	inspects   map[string]container.InspectResponse

	// serviceByID overrides the ServiceInspect response per service ID.
	serviceByID map[string]swarm.Service

	// Per-call errors.
	nodeListErr      error
	serviceListErr   error
	taskListErr      error
	containerListErr error

	// inspectErr is returned for any ContainerInspect call not found in inspects.
	inspectErr error

	// serviceInspectErr is returned for service IDs not in serviceByID.
	serviceInspectErr error

	// eventsCh / errCh are used by Events(); callers must close them when done.
	eventsCh chan events.Message
	errCh    chan error

	mu            sync.Mutex
	nodeListCalls int
	inspectCalls  int
}

var _ DockerAPI = (*fakeDocker)(nil)

func (f *fakeDocker) NodeList(
	_ context.Context,
	_ client.NodeListOptions,
) (client.NodeListResult, error) {
	f.mu.Lock()
	f.nodeListCalls++
	f.mu.Unlock()

	if f.nodeListErr != nil {
		return client.NodeListResult{}, f.nodeListErr
	}

	out := make([]swarm.Node, len(f.nodes))
	copy(out, f.nodes)

	return client.NodeListResult{Items: out}, nil
}

func (f *fakeDocker) ServiceList(
	_ context.Context,
	_ client.ServiceListOptions,
) (client.ServiceListResult, error) {
	if f.serviceListErr != nil {
		return client.ServiceListResult{}, f.serviceListErr
	}

	out := make([]swarm.Service, len(f.services))
	copy(out, f.services)

	return client.ServiceListResult{Items: out}, nil
}

func (f *fakeDocker) ServiceInspect(
	_ context.Context,
	id string,
	_ client.ServiceInspectOptions,
) (client.ServiceInspectResult, error) {
	if f.serviceByID != nil {
		if svc, ok := f.serviceByID[id]; ok {
			return client.ServiceInspectResult{Service: svc}, nil
		}
	}

	if f.serviceInspectErr != nil {
		return client.ServiceInspectResult{}, f.serviceInspectErr
	}

	return client.ServiceInspectResult{}, nil
}

func (f *fakeDocker) TaskList(
	_ context.Context,
	_ client.TaskListOptions,
) (client.TaskListResult, error) {
	if f.taskListErr != nil {
		return client.TaskListResult{}, f.taskListErr
	}

	out := make([]swarm.Task, len(f.tasks))
	copy(out, f.tasks)

	return client.TaskListResult{Items: out}, nil
}

func (f *fakeDocker) ContainerList(
	_ context.Context,
	_ client.ContainerListOptions,
) (client.ContainerListResult, error) {
	if f.containerListErr != nil {
		return client.ContainerListResult{}, f.containerListErr
	}

	out := make([]container.Summary, len(f.containers))
	copy(out, f.containers)

	return client.ContainerListResult{Items: out}, nil
}

func (f *fakeDocker) ContainerInspect(
	_ context.Context,
	id string,
	_ client.ContainerInspectOptions,
) (client.ContainerInspectResult, error) {
	f.mu.Lock()
	f.inspectCalls++
	f.mu.Unlock()

	if f.inspects != nil {
		if resp, ok := f.inspects[id]; ok {
			return client.ContainerInspectResult{Container: resp}, nil
		}
	}

	if f.inspectErr != nil {
		return client.ContainerInspectResult{}, f.inspectErr
	}

	return client.ContainerInspectResult{}, nil
}

func (f *fakeDocker) Events(_ context.Context, _ client.EventsListOptions) client.EventsResult {
	if f.eventsCh == nil {
		ch := make(chan events.Message)
		ec := make(chan error, 1)

		close(ch)

		return client.EventsResult{Messages: ch, Err: ec}
	}

	return client.EventsResult{Messages: f.eventsCh, Err: f.errCh}
}
