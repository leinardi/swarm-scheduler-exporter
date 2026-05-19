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

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/events"
	"github.com/docker/docker/api/types/swarm"
)

// fakeDocker implements DockerAPI for unit tests.
// All fields are read-only after construction; call counts are guarded by mu.
type fakeDocker struct {
	nodes      []swarm.Node
	services   []swarm.Service
	tasks      []swarm.Task
	containers []container.Summary
	inspects   map[string]container.InspectResponse

	// serviceByID overrides the ServiceInspectWithRaw response per service ID.
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

func (f *fakeDocker) NodeList(_ context.Context, _ swarm.NodeListOptions) ([]swarm.Node, error) {
	f.mu.Lock()
	f.nodeListCalls++
	f.mu.Unlock()

	if f.nodeListErr != nil {
		return nil, f.nodeListErr
	}

	out := make([]swarm.Node, len(f.nodes))
	copy(out, f.nodes)

	return out, nil
}

func (f *fakeDocker) ServiceList(
	_ context.Context,
	_ swarm.ServiceListOptions,
) ([]swarm.Service, error) {
	if f.serviceListErr != nil {
		return nil, f.serviceListErr
	}

	out := make([]swarm.Service, len(f.services))
	copy(out, f.services)

	return out, nil
}

func (f *fakeDocker) ServiceInspectWithRaw(
	_ context.Context,
	id string,
	_ swarm.ServiceInspectOptions,
) (swarm.Service, []byte, error) {
	if f.serviceByID != nil {
		if svc, ok := f.serviceByID[id]; ok {
			return svc, nil, nil
		}
	}

	if f.serviceInspectErr != nil {
		return swarm.Service{}, nil, f.serviceInspectErr
	}

	return swarm.Service{}, nil, nil
}

func (f *fakeDocker) TaskList(_ context.Context, _ swarm.TaskListOptions) ([]swarm.Task, error) {
	if f.taskListErr != nil {
		return nil, f.taskListErr
	}

	out := make([]swarm.Task, len(f.tasks))
	copy(out, f.tasks)

	return out, nil
}

func (f *fakeDocker) ContainerList(
	_ context.Context,
	_ container.ListOptions,
) ([]container.Summary, error) {
	if f.containerListErr != nil {
		return nil, f.containerListErr
	}

	out := make([]container.Summary, len(f.containers))
	copy(out, f.containers)

	return out, nil
}

func (f *fakeDocker) ContainerInspect(
	_ context.Context,
	id string,
) (container.InspectResponse, error) {
	f.mu.Lock()
	f.inspectCalls++
	f.mu.Unlock()

	if f.inspects != nil {
		if resp, ok := f.inspects[id]; ok {
			return resp, nil
		}
	}

	if f.inspectErr != nil {
		return container.InspectResponse{}, f.inspectErr
	}

	return container.InspectResponse{}, nil
}

func (f *fakeDocker) Events(
	_ context.Context,
	_ events.ListOptions,
) (msgCh <-chan events.Message, errCh <-chan error) {
	if f.eventsCh == nil {
		ch := make(chan events.Message)
		ec := make(chan error, 1)

		close(ch)

		return ch, ec
	}

	return f.eventsCh, f.errCh
}
