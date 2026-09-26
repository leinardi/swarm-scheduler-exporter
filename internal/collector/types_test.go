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
	"slices"
	"testing"

	"github.com/moby/moby/api/types/swarm"
)

// resetCollectorState clears package-level caches between tests.
func resetCollectorState(t *testing.T) {
	t.Helper()
	t.Cleanup(func() {
		metadataMu.Lock()
		metadataCache = make(map[string]serviceMetadata)
		metadataMu.Unlock()

		customLabelDefs = nil

		nodesMu.Lock()
		cachedNodes = nil
		nodesMu.Unlock()
	})
	metadataMu.Lock()
	metadataCache = make(map[string]serviceMetadata)
	metadataMu.Unlock()

	customLabelDefs = nil
}

func TestServiceMode(t *testing.T) {
	n := uint64(3)
	replicated := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &n},
			},
		},
	}
	global := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Mode: swarm.ServiceMode{Global: &swarm.GlobalService{}},
		},
	}

	if got := serviceMode(replicated); got != serviceModeReplicated {
		t.Errorf("replicated service: got %q, want %q", got, serviceModeReplicated)
	}

	if got := serviceMode(global); got != serviceModeGlobal {
		t.Errorf("global service: got %q, want %q", got, serviceModeGlobal)
	}

	replicatedJob := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Mode: swarm.ServiceMode{ReplicatedJob: &swarm.ReplicatedJob{}},
		},
	}
	if got := serviceMode(replicatedJob); got != serviceModeReplicatedJob {
		t.Errorf("replicated-job service: got %q, want %q", got, serviceModeReplicatedJob)
	}

	globalJob := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Mode: swarm.ServiceMode{GlobalJob: &swarm.GlobalJob{}},
		},
	}
	if got := serviceMode(globalJob); got != serviceModeGlobalJob {
		t.Errorf("global-job service: got %q, want %q", got, serviceModeGlobalJob)
	}
}

func TestIsJobMode(t *testing.T) {
	cases := map[string]bool{
		serviceModeReplicated:    false,
		serviceModeGlobal:        false,
		serviceModeReplicatedJob: true,
		serviceModeGlobalJob:     true,
		"":                       false,
	}
	for mode, want := range cases {
		if got := isJobMode(mode); got != want {
			t.Errorf("isJobMode(%q) = %v, want %v", mode, got, want)
		}
	}
}

func TestJobTotalCompletions_Defaults(t *testing.T) {
	maxConcurrent := uint64(4)
	totalCompletions := uint64(7)

	cases := []struct {
		name string
		job  swarm.ReplicatedJob
		want float64
	}{
		{name: "neither set", job: swarm.ReplicatedJob{}, want: 1},
		{
			name: "max concurrent only",
			job:  swarm.ReplicatedJob{MaxConcurrent: &maxConcurrent},
			want: 4,
		},
		{
			name: "total completions only",
			job:  swarm.ReplicatedJob{TotalCompletions: &totalCompletions},
			want: 7,
		},
		{
			name: "both set",
			job: swarm.ReplicatedJob{
				MaxConcurrent:    &maxConcurrent,
				TotalCompletions: &totalCompletions,
			},
			want: 7,
		},
	}
	for _, tc := range cases {
		if got := jobTotalCompletions(&tc.job); got != tc.want {
			t.Errorf("%s: jobTotalCompletions = %v, want %v", tc.name, got, tc.want)
		}
	}
}

func TestBuildMetadata_ReplicatedJob(t *testing.T) {
	resetCollectorState(t)

	maxConcurrent := uint64(2)
	totalCompletions := uint64(5)
	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{Name: "job"},
			Mode: swarm.ServiceMode{
				ReplicatedJob: &swarm.ReplicatedJob{
					MaxConcurrent:    &maxConcurrent,
					TotalCompletions: &totalCompletions,
				},
			},
		},
		JobStatus: &swarm.JobStatus{JobIteration: swarm.Version{Index: 42}},
	}

	md := buildMetadata(svc)
	if md.serviceMode != serviceModeReplicatedJob {
		t.Errorf("serviceMode = %q, want %q", md.serviceMode, serviceModeReplicatedJob)
	}

	if md.configuredReplicas != 5 {
		t.Errorf("configuredReplicas = %v, want 5 (TotalCompletions)", md.configuredReplicas)
	}

	if md.jobIteration != 42 {
		t.Errorf("jobIteration = %d, want 42", md.jobIteration)
	}
}

func TestBuildMetadata_GlobalJob(t *testing.T) {
	resetCollectorState(t)

	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{Name: "job"},
			Mode:        swarm.ServiceMode{GlobalJob: &swarm.GlobalJob{}},
		},
		JobStatus: &swarm.JobStatus{JobIteration: swarm.Version{Index: 9}},
	}

	md := buildMetadata(svc)
	if md.serviceMode != serviceModeGlobalJob {
		t.Errorf("serviceMode = %q, want %q", md.serviceMode, serviceModeGlobalJob)
	}

	if md.configuredReplicas != 0 {
		t.Errorf("configuredReplicas = %v, want 0 for global-job", md.configuredReplicas)
	}

	if md.jobIteration != 9 {
		t.Errorf("jobIteration = %d, want 9", md.jobIteration)
	}

	// A service without JobStatus (not a job, or not reported) leaves the iteration at 0.
	svc.JobStatus = nil
	if md = buildMetadata(svc); md.jobIteration != 0 {
		t.Errorf("jobIteration = %d, want 0 without JobStatus", md.jobIteration)
	}
}

func TestShortServiceName(t *testing.T) {
	cases := []struct {
		stack    string
		fullName string
		want     string
	}{
		{"", "myapp", "myapp"},
		{"mystack", "mystack_web", "web"},
		{"mystack", "other_web", "other_web"},
		{"mystack", "mystack_", ""},
		{"mystack", "mystack", "mystack"},
	}
	for _, tc := range cases {
		got := shortServiceName(tc.stack, tc.fullName)
		if got != tc.want {
			t.Errorf("shortServiceName(%q, %q) = %q, want %q", tc.stack, tc.fullName, got, tc.want)
		}
	}
}

func TestDisplayName(t *testing.T) {
	cases := []struct {
		stack   string
		service string
		want    string
	}{
		{"", "myservice", "myservice"},
		{"mystack", "mystack", "mystack"},
		{"mystack", "web", "mystack web"},
		{"", "", ""},
	}
	for _, tc := range cases {
		got := displayName(tc.stack, tc.service)
		if got != tc.want {
			t.Errorf("displayName(%q, %q) = %q, want %q", tc.stack, tc.service, got, tc.want)
		}
	}
}

func TestBuildMetadata_StackAndService(t *testing.T) {
	resetCollectorState(t)

	n := uint64(2)
	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{
				Name: "mystack_web",
				Labels: map[string]string{
					"com.docker.stack.namespace": "mystack",
				},
			},
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &n},
			},
		},
	}

	md := buildMetadata(svc)
	if md.stack != "mystack" {
		t.Errorf("stack = %q, want 'mystack'", md.stack)
	}

	if md.service != "web" {
		t.Errorf("service = %q, want 'web'", md.service)
	}

	if md.serviceMode != serviceModeReplicated {
		t.Errorf("serviceMode = %q, want %q", md.serviceMode, serviceModeReplicated)
	}

	if md.configuredReplicas != 2 {
		t.Errorf("configuredReplicas = %v, want 2", md.configuredReplicas)
	}
}

func TestBuildMetadata_PlacementCopied(t *testing.T) {
	resetCollectorState(t)

	constraints := []string{"node.role == manager"}
	platforms := []swarm.Platform{{OS: "linux", Architecture: "amd64"}}

	n := uint64(1)
	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{Name: "svc"},
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &n},
			},
			TaskTemplate: swarm.TaskSpec{
				Placement: &swarm.Placement{
					Constraints: constraints,
					Platforms:   platforms,
				},
			},
		},
	}

	md := buildMetadata(svc)

	if len(md.constraints) != 1 || md.constraints[0] != "node.role == manager" {
		t.Errorf("constraints = %v", md.constraints)
	}
	// Mutating the source slice must not affect the copy in metadata.
	constraints[0] = "mutated"
	if md.constraints[0] == "mutated" {
		t.Error("constraints slice was not defensively copied")
	}

	if len(md.platforms) != 1 || md.platforms[0].OS != "linux" {
		t.Errorf("platforms = %v", md.platforms)
	}
}

func TestBuildMetadata_RestartConditionNone(t *testing.T) {
	resetCollectorState(t)

	n := uint64(1)
	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{Name: "oneshot"},
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &n},
			},
			TaskTemplate: swarm.TaskSpec{
				RestartPolicy: &swarm.RestartPolicy{
					Condition: swarm.RestartPolicyConditionNone,
				},
			},
		},
	}

	if md := buildMetadata(svc); !md.restartConditionNone {
		t.Errorf("restartConditionNone = false, want true for condition=none")
	}

	svc.Spec.TaskTemplate.RestartPolicy.Condition = swarm.RestartPolicyConditionAny
	if md := buildMetadata(svc); md.restartConditionNone {
		t.Errorf("restartConditionNone = true, want false for condition=any")
	}

	svc.Spec.TaskTemplate.RestartPolicy = nil
	if md := buildMetadata(svc); md.restartConditionNone {
		t.Errorf("restartConditionNone = true, want false for nil RestartPolicy")
	}
}

func TestBuildMetadata_CustomLabels(t *testing.T) {
	resetCollectorState(t)
	SetCustomLabels([]string{"team", "tier"}, []string{"team", "tier"})

	n := uint64(1)
	svc := &swarm.Service{
		Spec: swarm.ServiceSpec{
			Annotations: swarm.Annotations{
				Name:   "svc",
				Labels: map[string]string{"team": "platform"},
			},
			Mode: swarm.ServiceMode{
				Replicated: &swarm.ReplicatedService{Replicas: &n},
			},
		},
	}

	md := buildMetadata(svc)

	if md.customLabels["team"] != "platform" {
		t.Errorf("team label = %q, want 'platform'", md.customLabels["team"])
	}

	if md.customLabels["tier"] != "" {
		t.Errorf("absent tier label should be '' not %q", md.customLabels["tier"])
	}
}

func TestSetServiceMetadata_PreservesDesiredReplicas(t *testing.T) {
	resetCollectorState(t)

	// Create the entry first.
	setServiceMetadata("svc1", &serviceMetadata{
		stack:        "stack",
		service:      "svc",
		serviceMode:  serviceModeReplicated,
		customLabels: map[string]string{},
	})
	// Now set desired replicas (requires entry to exist).
	setServiceDesiredReplicas("svc1", 5.0)

	// Re-set metadata (simulating a service update event) — desiredReplicas must survive.
	setServiceMetadata("svc1", &serviceMetadata{
		stack:        "stack",
		service:      "svc",
		serviceMode:  serviceModeReplicated,
		customLabels: map[string]string{},
	})

	md, ok := getServiceMetadata("svc1")
	if !ok {
		t.Fatal("metadata not found")
	}

	if md.desiredReplicas != 5.0 {
		t.Errorf("desiredReplicas = %v, want 5.0 (should have been preserved)", md.desiredReplicas)
	}
}

func TestGetReplicatedServiceMetadata_FiltersCorrectly(t *testing.T) {
	resetCollectorState(t)

	setServiceMetadata(
		"rep1",
		&serviceMetadata{serviceMode: serviceModeReplicated, customLabels: map[string]string{}},
	)
	setServiceMetadata(
		"glb1",
		&serviceMetadata{serviceMode: serviceModeGlobal, customLabels: map[string]string{}},
	)
	setServiceMetadata(
		"rep2",
		&serviceMetadata{serviceMode: serviceModeReplicated, customLabels: map[string]string{}},
	)

	results := getReplicatedServiceMetadata()
	if len(results) != 2 {
		t.Errorf("expected 2 replicated entries, got %d", len(results))
	}

	for _, md := range results {
		if md.serviceMode != serviceModeReplicated {
			t.Errorf("unexpected mode %q", md.serviceMode)
		}
	}
}

func TestGetNodeDependentServiceIDs_GlobalAndGlobalJobOnly(t *testing.T) {
	resetCollectorState(t)

	for serviceID, mode := range map[string]string{
		"rep":    serviceModeReplicated,
		"glb":    serviceModeGlobal,
		"repjob": serviceModeReplicatedJob,
		"glbjob": serviceModeGlobalJob,
	} {
		setServiceMetadata(
			serviceID,
			&serviceMetadata{serviceMode: mode, customLabels: map[string]string{}},
		)
	}

	got := getNodeDependentServiceIDs()
	slices.Sort(got)

	want := []string{"glb", "glbjob"}
	if !slices.Equal(got, want) {
		t.Errorf("getNodeDependentServiceIDs() = %v, want %v", got, want)
	}
}
