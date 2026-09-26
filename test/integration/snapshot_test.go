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
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/moby/moby/api/types/swarm"
)

const (
	steadyStateGolden = "steady_state.golden"
	snapshotStack     = "it-snapshot"
	// snapshotStablePolls is how many consecutive identical snapshots count as converged when
	// rewriting the golden file.
	snapshotStablePolls = 3
	timestampMask       = "<timestamp>"
	goldenHeader        = "# Rewrite with: make go-test-integration UPDATE=1 RUN=TestSnapshot_"
)

var updateGolden = flag.Bool("update", false, "rewrite the golden files under testdata")

var errSnapshotDiffers = errors.New("snapshot differs from golden file")

// TestSnapshot_SteadyState deploys a fixed mix of service shapes and compares everything the
// exporter says about them, once they have converged, with a golden file: any change to a family,
// a label or a value in these shapes shows up as a diff.
func TestSnapshot_SteadyState(t *testing.T) {
	const teamLabel = "team"

	web := serviceKey{stack: snapshotStack, service: "web"}
	agent := serviceKey{stack: snapshotStack, service: "agent"}
	idle := serviceKey{stack: snapshotStack, service: "idle"}
	nowhere := serviceKey{stack: snapshotStack, service: "nowhere"}
	oneshot := serviceKey{stack: snapshotStack, service: "oneshot"}
	custom := serviceKey{stack: snapshotStack, service: "custom"}
	nodeCount := float64(len(cluster.Nodes))

	deployService(t, snapshotStack, web.service, &serviceOpts{replicas: 2})
	deployService(t, snapshotStack, agent.service, &serviceOpts{global: true})
	deployService(t, snapshotStack, idle.service, &serviceOpts{zeroReplicas: true})
	deployService(t, snapshotStack, nowhere.service, &serviceOpts{
		global:      true,
		constraints: []string{"node.labels.it-unsatisfiable==true"},
	})
	deployService(t, snapshotStack, oneshot.service, &serviceOpts{
		command:          []string{"true"},
		restartCondition: swarm.RestartPolicyConditionNone,
	})
	deployService(
		t,
		snapshotStack,
		custom.service,
		&serviceOpts{labels: map[string]string{teamLabel: "platform"}},
	)

	all := []serviceKey{web, agent, idle, nowhere, oneshot, custom}
	baseURL := startExporter(t, all, "-label", teamLabel)

	eventually(t, 90*time.Second, metricsMatch(
		baseURL,
		snapshotStack,
		wantService(metricRunning, web, 2),
		wantService(metricRunning, agent, nodeCount),
		wantService(metricDesired, idle, 0),
		wantService(metricDesired, nowhere, 0),
		want(
			metricReplicasState,
			map[string]string{
				labelStack:   snapshotStack,
				labelService: oneshot.service,
				"state":      "complete",
			},
			1,
		),
		wantService(metricRunning, custom, 1),
		want(
			metricDesired,
			map[string]string{
				labelStack:   snapshotStack,
				labelService: custom.service,
				teamLabel:    "platform",
			},
			1,
		),
	))

	goldenPath := filepath.Join("testdata", steadyStateGolden)

	if *updateGolden {
		snapshot := stableSnapshot(t, baseURL)

		err := os.WriteFile(
			goldenPath,
			[]byte(goldenHeader+"\n"+strings.Join(snapshot, "\n")+"\n"),
			0o600,
		)
		if err != nil {
			t.Fatalf("write golden file: %v", err)
		}

		t.Logf("rewrote %s (%d series)", goldenPath, len(snapshot))

		return
	}

	golden := readGolden(t, goldenPath)

	eventually(t, 30*time.Second, func(ctx context.Context) error {
		snapshot, err := steadyStateSnapshot(ctx, baseURL)
		if err != nil {
			return err
		}

		return diffSnapshot(golden, snapshot)
	})
}

// steadyStateSnapshot renders the families under test as sorted exposition-style lines: every
// swarm_service_* and swarm_task_replicas_state series of the snapshot stack, and every
// swarm_cluster_nodes_by_state series. Timestamps are masked (zero stays zero: it means "never
// updated").
func steadyStateSnapshot(ctx context.Context, baseURL string) ([]string, error) {
	scraped, err := scrapeMetrics(ctx, baseURL)
	if err != nil {
		return nil, err
	}

	var lines []string

	for _, smp := range scraped.samples {
		inStack := smp.labels[labelStack] == snapshotStack
		serviceFamily := strings.HasPrefix(smp.name, "swarm_service_") ||
			smp.name == metricReplicasState

		if (!inStack || !serviceFamily) && smp.name != metricNodesByState {
			continue
		}

		line := formatSample(smp)
		if strings.HasSuffix(smp.name, "_timestamp_seconds") && smp.value != 0 {
			line = formatSeries(smp.name, smp.labels) + " " + timestampMask
		}

		lines = append(lines, line)
	}

	slices.Sort(lines)

	return lines, nil
}

// stableSnapshot waits until snapshotStablePolls consecutive snapshots are identical, so the
// golden file is never written from a state still settling.
func stableSnapshot(t *testing.T, baseURL string) []string {
	t.Helper()

	var (
		last    []string
		matches int
	)

	eventually(t, 30*time.Second, func(ctx context.Context) error {
		snapshot, err := steadyStateSnapshot(ctx, baseURL)
		if err != nil {
			return err
		}

		if slices.Equal(snapshot, last) {
			matches++
		} else {
			last = snapshot
			matches = 1
		}

		if matches < snapshotStablePolls {
			return fmt.Errorf("snapshot seen %d times in a row: %w", matches, errNotYet)
		}

		return nil
	})

	return last
}

func readGolden(t *testing.T, path string) []string {
	t.Helper()

	content, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf(
			"read golden file (rewrite it with make go-test-integration UPDATE=1 RUN=TestSnapshot_): %v",
			err,
		)
	}

	var lines []string

	for line := range strings.SplitSeq(string(content), "\n") {
		if line != "" && !strings.HasPrefix(line, "#") {
			lines = append(lines, line)
		}
	}

	return lines
}

// diffSnapshot lists the lines only in the golden file (-) and only in the snapshot (+). Both
// sides are sorted, so a set difference is a complete diff.
func diffSnapshot(golden, snapshot []string) error {
	var diff []string

	for _, line := range golden {
		if !slices.Contains(snapshot, line) {
			diff = append(diff, "- "+line)
		}
	}

	for _, line := range snapshot {
		if !slices.Contains(golden, line) {
			diff = append(diff, "+ "+line)
		}
	}

	if len(diff) == 0 {
		return nil
	}

	return fmt.Errorf("%w (- golden, + actual):\n%s", errSnapshotDiffers, strings.Join(diff, "\n"))
}
