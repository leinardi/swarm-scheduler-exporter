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
	"testing"
	"time"

	"github.com/moby/moby/api/types/swarm"
)

const (
	labelServiceMode = "service_mode"
	labelState       = "state"

	// jobCompletionTimeout bounds the wait for both jobs to finish: the replicated job runs its
	// completions one at a time (Swarm's default MaxConcurrent is 1).
	jobCompletionTimeout = 120 * time.Second
)

// exitOK is a job workload: it exits 0 at once, so each task ends in the complete state.
var exitOK = []string{"true"}

// TestJobs_Finished checks that replicated-job and global-job services are reported under their
// own service_mode, with desired_replicas at the replicated job's completions or the global job's
// eligible nodes, and that a finished job reads as at desired, with schedulable_replicas at 0 so
// "running below schedulable" never fires for it.
func TestJobs_Finished(t *testing.T) {
	const (
		stack       = "it-job"
		completions = 5
	)

	nodeCount := float64(len(cluster.Nodes))
	replicated := serviceKey{stack: stack, service: "batch"}
	global := serviceKey{stack: stack, service: "sweep"}

	deployService(
		t,
		stack,
		replicated.service,
		&serviceOpts{replicatedJob: true, completions: completions, command: exitOK},
	)
	deployService(t, stack, global.service, &serviceOpts{globalJob: true, command: exitOK})

	baseURL := startExporter(t, []serviceKey{replicated, global})

	eventually(t, jobCompletionTimeout, metricsMatch(baseURL, stack,
		wantJob(metricDesired, replicated, "replicated-job", completions),
		wantJobState(replicated, "replicated-job", swarm.TaskStateComplete, completions),
		wantJob(metricRunning, replicated, "replicated-job", 0),
		wantJob(metricAtDesired, replicated, "replicated-job", 1),
		wantJob(metricSchedulable, replicated, "replicated-job", 0),
		wantJob(metricDesired, global, "global-job", nodeCount),
		wantJobState(global, "global-job", swarm.TaskStateComplete, nodeCount),
		wantJob(metricRunning, global, "global-job", 0),
		wantJob(metricAtDesired, global, "global-job", 1),
		wantJob(metricSchedulable, global, "global-job", 0),
	))
}

// jobLabels matches the series of svc reported under service mode mode.
func jobLabels(svc serviceKey, mode string) map[string]string {
	labels := svc.labels()
	labels[labelServiceMode] = mode

	return labels
}

func wantJob(name string, svc serviceKey, mode string, value float64) metricWant {
	return metricWant{name: name, labels: jobLabels(svc, mode), value: value}
}

func wantJobState(svc serviceKey, mode string, state swarm.TaskState, value float64) metricWant {
	labels := jobLabels(svc, mode)
	labels[labelState] = string(state)

	return metricWant{name: metricReplicasState, labels: labels, value: value}
}
