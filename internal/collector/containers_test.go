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
	"errors"
	"testing"

	"github.com/docker/docker/api/types/container"
)

var errInspectFailed = errors.New("inspect failed")

func TestFirstContainerName(t *testing.T) {
	cases := []struct {
		names []string
		want  string
	}{
		{nil, ""},
		{[]string{}, ""},
		{[]string{"/mycontainer"}, "mycontainer"},
		{[]string{"mycontainer"}, "mycontainer"},
		{[]string{"/first", "/second"}, "first"},
	}
	for _, tc := range cases {
		got := firstContainerName(tc.names)
		if got != tc.want {
			t.Errorf("firstContainerName(%v) = %q, want %q", tc.names, got, tc.want)
		}
	}
}

func TestClassifyOrchestrator(t *testing.T) {
	cases := []struct {
		name             string
		labels           map[string]string
		wantProject      string
		wantService      string
		wantStack        string
		wantOrchestrator string
	}{
		{
			name: "compose labels",
			labels: map[string]string{
				"com.docker.compose.project": "myproj",
				"com.docker.compose.service": "web",
			},
			wantProject:      "myproj",
			wantService:      "web",
			wantOrchestrator: "compose",
		},
		{
			name: "swarm labels with stack",
			labels: map[string]string{
				"com.docker.swarm.service.name": "mystack_web",
				"com.docker.stack.namespace":    "mystack",
			},
			wantStack:        "mystack",
			wantService:      "web", // shortServiceName strips stack prefix
			wantOrchestrator: "swarm",
		},
		{
			name: "swarm with explicit compose service",
			labels: map[string]string{
				"com.docker.swarm.service.name": "mystack_web",
				"com.docker.stack.namespace":    "mystack",
				"com.docker.compose.service":    "api",
			},
			wantStack:        "mystack",
			wantService:      "api", // compose.service takes precedence
			wantOrchestrator: "swarm",
		},
		{
			name:             "no labels",
			labels:           map[string]string{},
			wantOrchestrator: "none",
		},
		{
			name: "spaces trimmed",
			labels: map[string]string{
				"com.docker.compose.project": "  proj  ",
				"com.docker.compose.service": " svc ",
			},
			wantProject:      "proj",
			wantService:      "svc",
			wantOrchestrator: "compose",
		},
	}
	for _, tc := range cases {
		project, service, stack, orchestrator := classifyOrchestrator(tc.labels)
		if project != tc.wantProject {
			t.Errorf("[%s] project = %q, want %q", tc.name, project, tc.wantProject)
		}

		if service != tc.wantService {
			t.Errorf("[%s] service = %q, want %q", tc.name, service, tc.wantService)
		}

		if stack != tc.wantStack {
			t.Errorf("[%s] stack = %q, want %q", tc.name, stack, tc.wantStack)
		}

		if orchestrator != tc.wantOrchestrator {
			t.Errorf("[%s] orchestrator = %q, want %q", tc.name, orchestrator, tc.wantOrchestrator)
		}
	}
}

func TestDisplayNameForContainer(t *testing.T) {
	cases := []struct {
		group         string
		service       string
		containerName string
		want          string
	}{
		{"", "", "mycontainer", "mycontainer"},
		{"", "svc", "mycontainer", "mycontainer"},
		{"grp", "", "mycontainer", "mycontainer"},
		{"grp", "svc", "mycontainer", "grp svc"},
		{"same", "same", "mycontainer", "same"},
	}
	for _, tc := range cases {
		got := displayNameForContainer(tc.group, tc.service, tc.containerName)
		if got != tc.want {
			t.Errorf("displayNameForContainer(%q,%q,%q) = %q, want %q",
				tc.group, tc.service, tc.containerName, got, tc.want)
		}
	}
}

func makeRow(state string) row {
	return row{
		id:    "cid",
		state: state,
		labels: map[string]string{
			labelState:  "",
			"exit_code": "",
		},
	}
}

// buildInspectWithHealth constructs a container.InspectResponse with config+state set.
func buildInspectWithHealth(healthStatus string) container.InspectResponse {
	r := container.InspectResponse{
		Config: &container.Config{Healthcheck: &container.HealthConfig{}},
	}
	r.ContainerJSONBase = &container.ContainerJSONBase{
		State: &container.State{
			Health: &container.Health{Status: healthStatus},
		},
	}

	return r
}

// buildInspectNoHealthcheck returns an InspectResponse with Config but no healthcheck.
func buildInspectNoHealthcheck() container.InspectResponse {
	return container.InspectResponse{Config: &container.Config{}}
}

// buildInspectWithExitCode returns an InspectResponse with the given exit code.
func buildInspectWithExitCode(code int) container.InspectResponse {
	r := container.InspectResponse{}
	r.ContainerJSONBase = &container.ContainerJSONBase{
		State: &container.State{ExitCode: code},
	}

	return r
}

func TestApplyHealthOverlay(t *testing.T) {
	cases := []struct {
		name      string
		inspected container.InspectResponse
		wantState string
	}{
		{
			name:      "nil config falls back to base",
			inspected: container.InspectResponse{},
			wantState: "running",
		},
		{
			name:      "nil healthcheck falls back to base",
			inspected: buildInspectNoHealthcheck(),
			wantState: "running",
		},
		{
			name:      "healthy",
			inspected: buildInspectWithHealth("healthy"),
			wantState: "healthy",
		},
		{
			name:      "unhealthy",
			inspected: buildInspectWithHealth("unhealthy"),
			wantState: "unhealthy",
		},
		{
			name:      "starting",
			inspected: buildInspectWithHealth("starting"),
			wantState: "health_starting",
		},
		{
			name:      "case insensitive",
			inspected: buildInspectWithHealth("HEALTHY"),
			wantState: "healthy",
		},
		{
			name:      "whitespace trimmed",
			inspected: buildInspectWithHealth("  unhealthy  "),
			wantState: "unhealthy",
		},
		{
			name:      "unknown status falls back to base",
			inspected: buildInspectWithHealth("unknown"),
			wantState: "running",
		},
	}
	for _, tc := range cases {
		r := makeRow("running")
		applyHealthOverlay(&r, tc.inspected)

		if r.labels[labelState] != tc.wantState {
			t.Errorf("[%s] state = %q, want %q", tc.name, r.labels[labelState], tc.wantState)
		}
	}
}

func TestApplyExitCode(t *testing.T) {
	t.Run("nil state leaves exit_code empty", func(t *testing.T) {
		r := makeRow("exited")
		// ContainerJSONBase must be non-nil (promoted field access), but State within it is nil.
		resp := container.InspectResponse{}
		resp.ContainerJSONBase = &container.ContainerJSONBase{}
		applyExitCode(&r, resp)

		if r.labels["exit_code"] != "" {
			t.Errorf("expected empty exit_code, got %q", r.labels["exit_code"])
		}
	})

	t.Run("non-exited state leaves exit_code empty", func(t *testing.T) {
		r := makeRow("running")
		applyExitCode(&r, buildInspectWithExitCode(1))

		if r.labels["exit_code"] != "" {
			t.Errorf(
				"expected empty exit_code for running container, got %q",
				r.labels["exit_code"],
			)
		}
	})

	t.Run("exited with code 0", func(t *testing.T) {
		r := makeRow("exited")
		applyExitCode(&r, buildInspectWithExitCode(0))

		if r.labels["exit_code"] != "0" {
			t.Errorf("exit_code = %q, want '0'", r.labels["exit_code"])
		}

		if r.labels[labelState] != "exited" {
			t.Errorf("state = %q, want 'exited'", r.labels[labelState])
		}
	})

	t.Run("exited with code 137 (OOM kill)", func(t *testing.T) {
		r := makeRow("exited")
		applyExitCode(&r, buildInspectWithExitCode(137))

		if r.labels["exit_code"] != "137" {
			t.Errorf("exit_code = %q, want '137'", r.labels["exit_code"])
		}
	})

	t.Run("case-insensitive exited match", func(t *testing.T) {
		r := makeRow("Exited")
		applyExitCode(&r, buildInspectWithExitCode(1))

		if r.labels["exit_code"] != "1" {
			t.Errorf("expected exit_code '1' for 'Exited' state, got %q", r.labels["exit_code"])
		}
	})
}

// ---- Docker-bound tests (use fakeDocker) ----

// resetContainersState restores containers package-level vars after a test.
func resetContainersState(t *testing.T) {
	t.Helper()

	prevEnabled := containersEnabled
	prevIncludeSwarm := containersIncludeSwarm
	prevCap := containersInspectCap

	t.Cleanup(func() {
		containersEnabled = prevEnabled
		containersIncludeSwarm = prevIncludeSwarm
		containersInspectCap = prevCap
	})
}

func TestPollContainersState_Disabled_ReturnsNil(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(false, false)

	rows, err := PollContainersState(context.Background(), &fakeDocker{})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if rows != nil {
		t.Errorf("expected nil rows when disabled, got %v", rows)
	}
}

func TestPollContainersState_SwarmContainerExcluded(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, false)

	swarmCnt := container.Summary{
		ID:    "c1",
		State: "running",
		Names: []string{"/svc_web_1"},
		Labels: map[string]string{
			"com.docker.swarm.service.name": "mystack_web",
		},
	}
	plainCnt := container.Summary{
		ID:     "c2",
		State:  "running",
		Names:  []string{"/plainapp"},
		Labels: map[string]string{},
	}

	fd := &fakeDocker{containers: []container.Summary{swarmCnt, plainCnt}}

	rows, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(rows) != 1 {
		t.Errorf("expected 1 row (swarm filtered), got %d", len(rows))
	}
}

func TestPollContainersState_SwarmContainerIncluded(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, true)

	swarmCnt := container.Summary{
		ID:    "c1",
		State: "running",
		Names: []string{"/svc_web_1"},
		Labels: map[string]string{
			"com.docker.swarm.service.name": "mystack_web",
		},
	}

	fd := &fakeDocker{containers: []container.Summary{swarmCnt}}

	rows, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(rows) != 1 {
		t.Errorf("expected 1 row when includeSwarm=true, got %d", len(rows))
	}
}

func TestPollContainersState_HealthOverlay(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, false)

	cnt := container.Summary{
		ID:    "c1",
		State: "running",
		Names: []string{"/myapp"},
	}
	healthyInspect := buildInspectWithHealth("healthy")

	fd := &fakeDocker{
		containers: []container.Summary{cnt},
		inspects:   map[string]container.InspectResponse{"c1": healthyInspect},
	}

	rows, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(rows))
	}

	if rows[0][labelState] != "healthy" {
		t.Errorf("state = %q, want 'healthy'", rows[0][labelState])
	}
}

func TestPollContainersState_ExitCode(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, false)

	cnt := container.Summary{
		ID:    "c1",
		State: "exited",
		Names: []string{"/done"},
	}
	fd := &fakeDocker{
		containers: []container.Summary{cnt},
		inspects:   map[string]container.InspectResponse{"c1": buildInspectWithExitCode(137)},
	}

	rows, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(rows))
	}

	if rows[0]["exit_code"] != "137" {
		t.Errorf("exit_code = %q, want '137'", rows[0]["exit_code"])
	}
}

func TestPollContainersState_InspectFailure_FallsBackToBaseState(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, false)

	cnt := container.Summary{
		ID:    "c1",
		State: "running",
		Names: []string{"/flaky"},
	}
	fd := &fakeDocker{
		containers: []container.Summary{cnt},
		inspectErr: errInspectFailed,
	}

	rows, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error (inspect failure should be non-fatal): %v", err)
	}

	if len(rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(rows))
	}

	if rows[0][labelState] != "running" {
		t.Errorf("state = %q, want 'running' (base state fallback)", rows[0][labelState])
	}
}

func TestPollContainersState_InspectCapHonored(t *testing.T) {
	resetContainersState(t)
	EnableContainersMetrics(true, false)

	containersInspectCap = 1 // cap at 1

	// Two exited containers both need inspect for exit code.
	c1 := container.Summary{ID: "c1", State: "exited", Names: []string{"/a"}}
	c2 := container.Summary{ID: "c2", State: "exited", Names: []string{"/b"}}

	fd := &fakeDocker{
		containers: []container.Summary{c1, c2},
		inspects: map[string]container.InspectResponse{
			"c1": buildInspectWithExitCode(0),
			"c2": buildInspectWithExitCode(1),
		},
	}

	_, err := PollContainersState(context.Background(), fd)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	fd.mu.Lock()
	calls := fd.inspectCalls
	fd.mu.Unlock()

	if calls != 1 {
		t.Errorf("inspect calls = %d, want 1 (cap=1)", calls)
	}
}
