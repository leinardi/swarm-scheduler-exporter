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
	"fmt"
	"testing"
	"time"

	"github.com/moby/moby/api/types/swarm"

	"github.com/leinardi/swarm-scheduler-exporter/internal/testenv"
)

const (
	// nodeChangeTimeout bounds the waits around a node change. A paused node is only marked down
	// once its heartbeats have lapsed, which takes Swarm tens of seconds.
	nodeChangeTimeout = 120 * time.Second
)

// nodeScenario is the shared setup of the node tests: a global service on every node, a
// two-replica service with one replica on each worker, an exporter watching both, and the victim —
// the worker hosting slot 1 of the replicated service.
type nodeScenario struct {
	stack        string
	baseURL      string
	global       serviceKey
	replicated   serviceKey
	replicatedID string
	victim       testenv.Node
	survivor     testenv.Node // the other worker, where the victim's replica must land
}

func setUpNodeScenario(t *testing.T, stack string) *nodeScenario {
	t.Helper()

	workers := cluster.Workers()
	if len(workers) != 2 {
		t.Skipf("the node scenarios need exactly 2 workers, the cluster has %d", len(workers))
	}

	scenario := &nodeScenario{
		stack:      stack,
		global:     serviceKey{stack: stack, service: "agent"},
		replicated: serviceKey{stack: stack, service: "web"},
	}

	globalID := deployService(t, stack, scenario.global.service, &serviceOpts{global: true})

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		tasks, err := listTasks(ctx, globalID)
		if err != nil {
			return err
		}

		return runsOnEveryNode(runningTasks(tasks))
	})

	scenario.replicatedID = deployService(t, stack, scenario.replicated.service, &serviceOpts{
		replicas:    2,
		constraints: []string{"node.role==worker"},
	})

	var running []taskInfo

	eventually(t, 60*time.Second, func(ctx context.Context) error {
		tasks, err := listTasks(ctx, scenario.replicatedID)
		if err != nil {
			return err
		}

		running = runningTasks(tasks)
		if len(running) != 2 {
			return fmt.Errorf(
				"want 2 running replicas, have:\n%s: %w",
				describeTasks(running),
				errNotYet,
			)
		}

		return nil
	})

	if running[0].nodeID == running[1].nodeID {
		t.Fatalf(
			"Swarm placed both replicas on node %s; the node scenarios need one replica per worker:\n%s",
			running[0].nodeID,
			describeTasks(running),
		)
	}

	for _, task := range running {
		if task.slot != 1 {
			continue
		}

		victim, err := cluster.NodeBySwarmID(task.nodeID)
		if err != nil {
			t.Fatal(err)
		}

		scenario.victim = victim
	}

	if scenario.victim.SwarmNodeID == "" {
		t.Fatalf("no running replica in slot 1:\n%s", describeTasks(running))
	}

	for _, worker := range workers {
		if worker.SwarmNodeID != scenario.victim.SwarmNodeID {
			scenario.survivor = worker
		}
	}

	scenario.baseURL = startExporter(t, []serviceKey{scenario.global, scenario.replicated})

	nodeCount := float64(len(cluster.Nodes))

	eventually(t, 60*time.Second, metricsMatch(scenario.baseURL, stack,
		wantService(metricDesired, scenario.global, nodeCount),
		wantService(metricDesired, scenario.replicated, 2),
		wantService(metricSchedulable, scenario.replicated, 2),
		wantService(metricRunning, scenario.replicated, 2),
	))

	t.Logf(
		"victim %s (%s), survivor %s (%s)",
		scenario.victim.Hostname,
		scenario.victim.SwarmNodeID,
		scenario.survivor.Hostname,
		scenario.survivor.SwarmNodeID,
	)

	// Registered last, so it runs first: the cluster is whole again before the services go.
	t.Cleanup(func() { restoreNode(t, &scenario.victim) })

	return scenario
}

// restoreNode unpauses the node and makes it active again, then waits until every node is Ready
// and Active: the state the next test expects.
func restoreNode(t *testing.T, node *testenv.Node) {
	t.Helper()

	ctx, cancel := teardownCtx()
	defer cancel()

	err := cluster.UnpauseNode(ctx, node)
	if err != nil {
		t.Errorf("cleanup: %v", err)
	}

	err = updateNodeSpec(ctx, node.SwarmNodeID, func(spec *swarm.NodeSpec) {
		spec.Availability = swarm.NodeAvailabilityActive
	})
	if err != nil {
		t.Errorf("cleanup: %v", err)
	}

	err = waitNodesReadyActive(ctx)
	if err != nil {
		t.Errorf("cleanup: %v", err)
	}
}

// slotOneMoved holds when slot 1's only running, wanted task sits on the survivor.
func (s *nodeScenario) slotOneMoved(ctx context.Context) error {
	tasks, err := listTasks(ctx, s.replicatedID)
	if err != nil {
		return err
	}

	for _, task := range runningTasks(tasks) {
		if task.slot == 1 && task.nodeID == s.survivor.SwarmNodeID {
			return nil
		}
	}

	return fmt.Errorf(
		"slot 1 not running on %s yet:\n%s: %w",
		s.survivor.Hostname,
		describeTasks(tasks),
		errNotYet,
	)
}

// TestNode_Drain drains the victim: the global service loses a node, and the replicated service
// keeps its desired count but can now be scheduled on one worker only, while its slot-1 replica
// moves to the survivor. Setting the victim active again restores both.
func TestNode_Drain(t *testing.T) {
	scenario := setUpNodeScenario(t, "it-drain")
	nodeCount := float64(len(cluster.Nodes))

	updateNode(t, scenario.victim.SwarmNodeID, func(spec *swarm.NodeSpec) {
		spec.Availability = swarm.NodeAvailabilityDrain
	})

	eventually(t, nodeChangeTimeout, scenario.slotOneMoved)

	// The placement above comes straight from the manager; make sure the exporter has polled it
	// too, or a scrape from before the move could satisfy the checks below.
	waitForFreshPoll(t, scenario.baseURL)

	eventually(t, nodeChangeTimeout, metricsMatch(scenario.baseURL, scenario.stack,
		want(metricNodesByState, map[string]string{"availability": "drain"}, 1),
		wantService(metricDesired, scenario.global, nodeCount-1),
		wantService(metricDesired, scenario.replicated, 2),
		wantService(metricSchedulable, scenario.replicated, 1),
		wantService(metricRunning, scenario.replicated, 2),
		wantService(metricAtDesired, scenario.replicated, 1),
	))

	updateNode(t, scenario.victim.SwarmNodeID, func(spec *swarm.NodeSpec) {
		spec.Availability = swarm.NodeAvailabilityActive
	})

	eventually(t, nodeChangeTimeout, metricsMatch(scenario.baseURL, scenario.stack,
		wantSum(metricNodesByState, map[string]string{"availability": "drain"}, 0),
		wantService(metricDesired, scenario.global, nodeCount),
		wantService(metricSchedulable, scenario.replicated, 2),
	))
}

// TestNode_Down freezes the victim's DinD container, so the manager marks it down while its
// tasks are still reported running. Swarm starts a replacement for slot 1 on the survivor, and
// the exporter must count one task for that slot — the replacement — not both.
func TestNode_Down(t *testing.T) {
	scenario := setUpNodeScenario(t, "it-down")
	nodeCount := float64(len(cluster.Nodes))

	err := cluster.PauseNode(testCtx(t), &scenario.victim)
	if err != nil {
		t.Fatal(err)
	}

	// The slot must hold both tasks at once — the stale one on the victim, still reported running
	// but retired, and its replacement running on the survivor — or the dedupe is not exercised.
	eventually(t, nodeChangeTimeout, func(ctx context.Context) error {
		tasks, listErr := listTasks(ctx, scenario.replicatedID)
		if listErr != nil {
			return listErr
		}

		var stale, replacement bool

		for _, task := range tasks {
			if task.slot != 1 || task.state != swarm.TaskStateRunning {
				continue
			}

			switch {
			case task.nodeID == scenario.victim.SwarmNodeID && task.desiredState == swarm.TaskStateShutdown:
				stale = true
			case task.nodeID == scenario.survivor.SwarmNodeID && task.desiredState == swarm.TaskStateRunning:
				replacement = true
			}
		}

		if !stale || !replacement {
			return fmt.Errorf(
				"slot 1: stale task on victim %v, replacement on survivor %v:\n%s: %w",
				stale,
				replacement,
				describeTasks(tasks),
				errNotYet,
			)
		}

		return nil
	})

	// A scrape from before the replacement was running would show running 2 even without the
	// dedupe; only a poll of the state above tells counting one task per slot from counting both.
	waitForFreshPoll(t, scenario.baseURL)

	eventually(t, nodeChangeTimeout, metricsMatch(scenario.baseURL, scenario.stack,
		want(metricNodesByState, map[string]string{"status": "down"}, 1),
		wantService(metricDesired, scenario.global, nodeCount-1),
		wantService(metricDesired, scenario.replicated, 2),
		wantService(metricSchedulable, scenario.replicated, 1),
		wantService(metricRunning, scenario.replicated, 2),
		wantService(metricAtDesired, scenario.replicated, 1),
	))

	err = cluster.UnpauseNode(testCtx(t), &scenario.victim)
	if err != nil {
		t.Fatal(err)
	}

	eventually(t, nodeChangeTimeout, waitNodesReadyActive)

	eventually(t, nodeChangeTimeout, metricsMatch(scenario.baseURL, scenario.stack,
		wantSum(metricNodesByState, map[string]string{"status": "down"}, 0),
		wantService(metricDesired, scenario.global, nodeCount),
		wantService(metricSchedulable, scenario.replicated, 2),
	))
}
