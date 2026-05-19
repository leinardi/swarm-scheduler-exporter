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
	"testing"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/events"
	"github.com/docker/docker/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

// makeSchedulableNode returns a node that passes isNodeSchedulable.
func makeSchedulableNode(id, hostname string) swarm.Node {
	return swarm.Node{
		ID: id,
		Description: swarm.NodeDescription{
			Hostname: hostname,
			Platform: swarm.Platform{OS: "linux", Architecture: "amd64"},
		},
		Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		Spec: swarm.NodeSpec{
			Role:         swarm.NodeRoleWorker,
			Availability: swarm.NodeAvailabilityActive,
		},
	}
}

func TestIsNodeSchedulable(t *testing.T) {
	ready := makeSchedulableNode("n1", "host1")

	cases := []struct {
		name string
		node *swarm.Node
		want bool
	}{
		{"nil node", nil, false},
		{"ready+active", &ready, true},
		{
			"not ready",
			func() *swarm.Node {
				n := ready
				n.Status.State = swarm.NodeStateDown

				return &n
			}(),
			false,
		},
		{
			"paused",
			func() *swarm.Node {
				n := ready
				n.Spec.Availability = swarm.NodeAvailabilityPause

				return &n
			}(),
			false,
		},
		{
			"drain",
			func() *swarm.Node {
				n := ready
				n.Spec.Availability = swarm.NodeAvailabilityDrain

				return &n
			}(),
			false,
		},
	}
	for _, tc := range cases {
		got := isNodeSchedulable(tc.node)
		if got != tc.want {
			t.Errorf("[%s] isNodeSchedulable = %v, want %v", tc.name, got, tc.want)
		}
	}
}

func TestPlatformMatches(t *testing.T) {
	node := makeSchedulableNode("n1", "host1") // linux/amd64

	cases := []struct {
		name     string
		required []swarm.Platform
		want     bool
	}{
		{
			"exact match linux/amd64",
			[]swarm.Platform{{OS: "linux", Architecture: "amd64"}},
			true,
		},
		{
			"case-insensitive OS",
			[]swarm.Platform{{OS: "Linux", Architecture: "AMD64"}},
			true,
		},
		{
			"OS wildcard",
			[]swarm.Platform{{OS: "", Architecture: "amd64"}},
			true,
		},
		{
			"arch wildcard",
			[]swarm.Platform{{OS: "linux", Architecture: ""}},
			true,
		},
		{
			"both wildcards",
			[]swarm.Platform{{OS: "", Architecture: ""}},
			true,
		},
		{
			"OS mismatch",
			[]swarm.Platform{{OS: "windows", Architecture: "amd64"}},
			false,
		},
		{
			"arch mismatch",
			[]swarm.Platform{{OS: "linux", Architecture: "arm64"}},
			false,
		},
		{
			"multiple platforms one matches",
			[]swarm.Platform{
				{OS: "windows", Architecture: "amd64"},
				{OS: "linux", Architecture: "amd64"},
			},
			true,
		},
	}
	for _, tc := range cases {
		got := platformMatches(&node, tc.required)
		if got != tc.want {
			t.Errorf("[%s] platformMatches = %v, want %v", tc.name, got, tc.want)
		}
	}
}

// TestPlatformMatchesArchNormalization verifies that kernel-style architecture
// names (as reported by Docker via uname -m) match Docker manifest names
// (GOARCH) used in service Spec.TaskTemplate.Placement.Platforms.
func TestPlatformMatchesArchNormalization(t *testing.T) {
	cases := []struct {
		name     string
		nodeArch string
		reqArch  string
		want     bool
	}{
		{"x86_64 node matches amd64 platform", "x86_64", "amd64", true},
		{"amd64 node matches x86_64 platform", "amd64", "x86_64", true},
		{"aarch64 node matches arm64 platform", "aarch64", "arm64", true},
		{"arm64 node matches aarch64 platform", "arm64", "aarch64", true},
		{"i686 node matches 386 platform", "i686", "386", true},
		{"armv7l node matches arm platform", "armv7l", "arm", true},
		{"x86_64 node does NOT match arm64", "x86_64", "arm64", false},
		{"aarch64 node does NOT match amd64", "aarch64", "amd64", false},
	}
	for _, tc := range cases {
		node := swarm.Node{
			Description: swarm.NodeDescription{
				Hostname: "n",
				Platform: swarm.Platform{OS: "linux", Architecture: tc.nodeArch},
			},
			Status: swarm.NodeStatus{State: swarm.NodeStateReady},
			Spec:   swarm.NodeSpec{Availability: swarm.NodeAvailabilityActive},
		}

		got := platformMatches(&node, []swarm.Platform{{OS: "linux", Architecture: tc.reqArch}})
		if got != tc.want {
			t.Errorf("[%s] platformMatches(%s vs %s) = %v, want %v",
				tc.name, tc.nodeArch, tc.reqArch, got, tc.want)
		}
	}
}

func TestConstraintsMatch(t *testing.T) {
	node := swarm.Node{
		ID: "abc123",
		Description: swarm.NodeDescription{
			Hostname: "pollux",
			Platform: swarm.Platform{OS: "linux", Architecture: "amd64"},
			Engine:   swarm.EngineDescription{Labels: map[string]string{"engine_env": "prod"}},
		},
		Status: swarm.NodeStatus{State: swarm.NodeStateReady},
		Spec: swarm.NodeSpec{
			Annotations:  swarm.Annotations{Labels: map[string]string{"zone": "us-east-1"}},
			Role:         swarm.NodeRoleManager,
			Availability: swarm.NodeAvailabilityActive,
		},
	}

	cases := []struct {
		name        string
		constraints []string
		want        bool
	}{
		{"empty constraints", nil, true},
		{"role == manager", []string{"node.role == manager"}, true},
		{"role == worker", []string{"node.role == worker"}, false},
		{"role != worker", []string{"node.role != worker"}, true},
		{"role != manager", []string{"node.role != manager"}, false},
		{"hostname match", []string{"node.hostname == pollux"}, true},
		{"hostname mismatch", []string{"node.hostname == castor"}, false},
		{"node.id match", []string{"node.id == abc123"}, true},
		{"platform.os", []string{"node.platform.os == linux"}, true},
		{"platform.arch", []string{"node.platform.arch == amd64"}, true},
		{"node label", []string{"node.labels.zone == us-east-1"}, true},
		{"node label mismatch", []string{"node.labels.zone == eu-west-1"}, false},
		{"missing node label", []string{"node.labels.nonexistent == x"}, false},
		{"engine label", []string{"engine.labels.engine_env == prod"}, true},
		{"engine label mismatch", []string{"engine.labels.engine_env == dev"}, false},
		{
			"multiple constraints all match",
			[]string{"node.role == manager", "node.hostname == pollux"},
			true,
		},
		{
			"multiple constraints one fails",
			[]string{"node.role == manager", "node.hostname == castor"},
			false,
		},
		{"malformed no operator", []string{"node.role"}, false},
		{"unsupported operator >", []string{"node.labels.count > 5"}, false},
	}
	for _, tc := range cases {
		got := constraintsMatch(&node, tc.constraints)
		if got != tc.want {
			t.Errorf("[%s] constraintsMatch = %v, want %v", tc.name, got, tc.want)
		}
	}
}

func TestNodeAttributes(t *testing.T) {
	node := swarm.Node{
		ID: "myid",
		Description: swarm.NodeDescription{
			Hostname: "myhost",
			Platform: swarm.Platform{OS: "linux", Architecture: "arm64"},
			Engine: swarm.EngineDescription{
				Labels: map[string]string{"env": "staging"},
			},
		},
		Spec: swarm.NodeSpec{
			Annotations: swarm.Annotations{Labels: map[string]string{"region": "eu"}},
			Role:        swarm.NodeRoleWorker,
		},
	}

	attrs := nodeAttributes(&node)

	check := func(key, want string) {
		t.Helper()

		if got := attrs[key]; got != want {
			t.Errorf("attrs[%q] = %q, want %q", key, got, want)
		}
	}

	check("node.id", "myid")
	check("node.hostname", "myhost")
	check("node.role", "worker")
	check("node.platform.os", "linux")
	check("node.platform.arch", "arm64")
	check("node.labels.region", "eu")
	check("engine.labels.env", "staging")
}

func TestNodeAttributes_NoPlatformWhenEmpty(t *testing.T) {
	node := swarm.Node{
		ID:          "noid",
		Description: swarm.NodeDescription{Hostname: "h"},
		Spec:        swarm.NodeSpec{Role: swarm.NodeRoleWorker},
	}

	attrs := nodeAttributes(&node)
	if _, ok := attrs["node.platform.os"]; ok {
		t.Error("node.platform.os should be absent when OS is empty")
	}

	if _, ok := attrs["node.platform.arch"]; ok {
		t.Error("node.platform.arch should be absent when arch is empty")
	}
}

func TestCountEligibleNodesForServiceFromNodes(t *testing.T) {
	nodes := []swarm.Node{
		makeSchedulableNode("n1", "host1"),
		makeSchedulableNode("n2", "host2"),
		makeSchedulableNode("n3", "host3"),
	}
	// Mark n3 as drain.
	nodes[2].Spec.Availability = swarm.NodeAvailabilityDrain

	t.Run("no placement all schedulable", func(t *testing.T) {
		svc := &swarm.Service{}

		got := countEligibleNodesForServiceFromNodes(nodes, svc)
		if got != 2 { // n1, n2 only (n3 is drain)
			t.Errorf("got %d, want 2", got)
		}
	})

	t.Run("hostname constraint", func(t *testing.T) {
		svc := &swarm.Service{
			Spec: swarm.ServiceSpec{
				TaskTemplate: swarm.TaskSpec{
					Placement: &swarm.Placement{
						Constraints: []string{"node.hostname == host1"},
					},
				},
			},
		}

		got := countEligibleNodesForServiceFromNodes(nodes, svc)
		if got != 1 {
			t.Errorf("got %d, want 1", got)
		}
	})

	t.Run("platform constraint excludes", func(t *testing.T) {
		nodes2 := []swarm.Node{makeSchedulableNode("n1", "h1")}
		nodes2[0].Description.Platform = swarm.Platform{OS: "linux", Architecture: "amd64"}

		svc := &swarm.Service{
			Spec: swarm.ServiceSpec{
				TaskTemplate: swarm.TaskSpec{
					Placement: &swarm.Placement{
						Platforms: []swarm.Platform{{OS: "windows", Architecture: "amd64"}},
					},
				},
			},
		}

		got := countEligibleNodesForServiceFromNodes(nodes2, svc)
		if got != 0 {
			t.Errorf("got %d, want 0", got)
		}
	})
}

// ---- Docker-bound tests (use fakeDocker) ----

func makeReplicatedService(id, stack, name string, replicas uint64) swarm.Service {
	return swarm.Service{
		ID: id,
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{
				Name:   stack + "_" + name,
				Labels: map[string]string{"com.docker.stack.namespace": stack},
			},
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &replicas},
			},
		},
	}
}

func makeGlobalService(id, stack, name string) swarm.Service {
	return swarm.Service{
		ID: id,
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{
				Name:   stack + "_" + name,
				Labels: map[string]string{"com.docker.stack.namespace": stack},
			},
			Mode: swarm.ServiceMode{Global: &swarm.GlobalService{}},
		},
	}
}

func TestProcessEvent_NodeUpdate_TriggersNodeListRefresh(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installLocalNodeGauge(t)

	fd := &fakeDocker{nodes: []swarm.Node{makeSchedulableNode("n1", "h1")}}

	evt := &events.Message{Type: "node", Action: events.ActionUpdate, Actor: events.Actor{ID: "n1"}}

	err := processEvent(context.Background(), fd, evt)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	fd.mu.Lock()
	calls := fd.nodeListCalls
	fd.mu.Unlock()

	if calls == 0 {
		t.Error("expected NodeList to be called on node event")
	}
}

func TestProcessEvent_ServiceRemove_WithCachedMetadata(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)

	md := makeTestMetadata("stack", "svc", serviceModeReplicated)
	setServiceMetadata("svc1", &md)

	evt := &events.Message{
		Type:   "service",
		Action: events.ActionRemove,
		Actor:  events.Actor{ID: "svc1"},
	}

	err := processEvent(context.Background(), &fakeDocker{}, evt)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	_, ok := getServiceMetadata("svc1")
	if ok {
		t.Error("metadata should have been deleted after service remove event")
	}
}

func TestProcessEvent_ServiceRemove_WithoutCachedMetadata(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)

	evt := &events.Message{
		Type:   "service",
		Action: events.ActionRemove,
		Actor:  events.Actor{ID: "svc_gone"},
	}

	err := processEvent(context.Background(), &fakeDocker{}, evt)
	if err == nil {
		t.Error("expected ErrNoCachedMetadata for unknown service remove")
	}
}

func TestProcessEvent_ServiceUpdate_PopulatesMetadata(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)

	svc := makeReplicatedService("svc1", "stack", "web", 2)
	fd := &fakeDocker{serviceByID: map[string]swarm.Service{"svc1": svc}}
	fd.nodes = []swarm.Node{makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2")}
	setCachedNodes(fd.nodes)

	evt := &events.Message{
		Type:   "service",
		Action: events.ActionUpdate,
		Actor:  events.Actor{ID: "svc1"},
	}

	err := processEvent(context.Background(), fd, evt)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	md, ok := getServiceMetadata("svc1")
	if !ok {
		t.Fatal("metadata should be present after service update event")
	}

	if md.service != "web" {
		t.Errorf("service = %q, want 'web'", md.service)
	}

	if md.serviceMode != serviceModeReplicated {
		t.Errorf("serviceMode = %q, want %q", md.serviceMode, serviceModeReplicated)
	}
}

func TestRefreshNodesAndRecomputeGlobals_RecomputesGlobalService(t *testing.T) {
	resetCollectorState(t)
	desired := installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installLocalNodeGauge(t)

	// Seed a global service.
	glbSvc := makeGlobalService("glb1", "stack", "worker")
	glbMd := makeTestMetadata("stack", "worker", serviceModeGlobal)
	setServiceMetadata("glb1", &glbMd)

	// Start with 2 schedulable nodes.
	nodes2 := []swarm.Node{makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2")}
	fd := &fakeDocker{
		nodes:       nodes2,
		serviceByID: map[string]swarm.Service{"glb1": glbSvc},
	}

	err := refreshNodesAndRecomputeGlobals(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	lbls := prometheus.Labels{
		labelStack: "stack", labelService: "worker",
		labelServiceMode: serviceModeGlobal,
		labelDisplayName: displayName("stack", "worker"),
	}

	got := testutil.ToFloat64(desired.With(lbls))
	if got != 2 {
		t.Errorf("desired_replicas = %v, want 2 (one per schedulable node)", got)
	}
}

func TestRefreshNodesAndRecomputeGlobals_GoneService_SkippedNotError(t *testing.T) {
	resetCollectorState(t)
	installDesiredReplicasGauges(t)
	installServiceUpdateGauges(t)
	installLocalNodeGauge(t)

	// Seed a global service that has disappeared from Docker.
	glbMd := makeTestMetadata("stack", "gone", serviceModeGlobal)
	setServiceMetadata("glb_gone", &glbMd)

	fd := &fakeDocker{
		nodes:             []swarm.Node{makeSchedulableNode("n1", "h1")},
		serviceInspectErr: errdefs.ErrNotFound,
	}

	err := refreshNodesAndRecomputeGlobals(context.Background(), fd)
	if err != nil {
		t.Errorf("gone service during refresh should not error, got: %v", err)
	}
}

func TestCountActiveNodes_NodeListError(t *testing.T) {
	resetCollectorState(t)

	fd := &fakeDocker{nodeListErr: errdefs.ErrUnavailable}

	_, err := countActiveNodes(context.Background(), fd)
	if err == nil {
		t.Error("expected error when NodeList fails")
	}
}

func TestCountActiveNodes_CountsSchedulable(t *testing.T) {
	resetCollectorState(t)

	nodes := make([]swarm.Node, 0, 3)
	nodes = append(nodes, makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2"))
	// n3: ready but drain — not schedulable.
	n3 := makeSchedulableNode("n3", "h3")
	n3.Spec.Availability = swarm.NodeAvailabilityDrain
	nodes = append(nodes, n3)

	fd := &fakeDocker{nodes: nodes}

	count, err := countActiveNodes(context.Background(), fd)
	if err != nil {
		t.Fatal(err)
	}

	if count != 2 {
		t.Errorf("count = %d, want 2", count)
	}
}
