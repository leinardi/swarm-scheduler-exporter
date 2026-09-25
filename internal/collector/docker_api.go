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

	"github.com/moby/moby/client"
)

// DockerAPI is the subset of the moby client.Client used by this package. It only lists,
// inspects and streams events: the exporter must never change Docker state, and this interface
// (together with the depguard docker-sdk-boundary rule) is what enforces that.
// Accepting an interface (rather than the concrete *client.Client) allows tests
// to inject a fake without requiring a running Docker daemon.
// *client.Client satisfies this interface — no adapter is needed.
type DockerAPI interface {
	NodeList(ctx context.Context, options client.NodeListOptions) (client.NodeListResult, error)
	ServiceList(
		ctx context.Context,
		options client.ServiceListOptions,
	) (client.ServiceListResult, error)
	ServiceInspect(
		ctx context.Context,
		serviceID string,
		options client.ServiceInspectOptions,
	) (client.ServiceInspectResult, error)
	TaskList(ctx context.Context, options client.TaskListOptions) (client.TaskListResult, error)
	ContainerList(
		ctx context.Context,
		options client.ContainerListOptions,
	) (client.ContainerListResult, error)
	ContainerInspect(
		ctx context.Context,
		containerID string,
		options client.ContainerInspectOptions,
	) (client.ContainerInspectResult, error)
	Events(ctx context.Context, options client.EventsListOptions) client.EventsResult
}

// *client.Client must keep satisfying DockerAPI without an adapter.
var _ DockerAPI = (*client.Client)(nil)
