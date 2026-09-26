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

// Package testenv brings up a throwaway multi-node Docker Swarm for the integration tests: one
// manager and N workers, each a privileged docker:dind container on the host daemon, joined over
// a dedicated bridge network. Everything it creates carries LabelEnvID, so a run can be torn down
// (or swept after a crash) without touching anything else on the host, including a host that is
// itself a Swarm node.
//
// The package mutates Docker on purpose — it creates and removes containers, networks and a Swarm —
// which is why it is confined to the integration build tag and never imported by the exporter.
package testenv

import (
	"errors"
	"fmt"
	"log/slog"
	"os"
	"strconv"
	"time"

	dockerclient "github.com/moby/moby/client"
)

// Environment variables read by SpecFromEnv.
const (
	// EnvBinary is the absolute path of the exporter binary the tests run against the cluster.
	EnvBinary = "SSE_IT_BINARY"
	// EnvDinDImage overrides the pinned docker:dind image used for every node.
	EnvDinDImage = "SSE_IT_DIND_IMAGE"
	// EnvWorkers overrides the number of worker nodes (default 2).
	EnvWorkers = "SSE_IT_WORKERS"
	// EnvKeepOnFailure keeps the cluster running when bring-up or a test fails, for debugging.
	EnvKeepOnFailure = "SSE_IT_KEEP_ON_FAILURE"
)

const defaultWorkerCount = 2

// Sentinel errors, so callers match them with errors.Is instead of comparing message text.
var (
	errNotAttached      = errors.New("network attachment missing")
	errNoHostPort       = errors.New("host port binding missing")
	errNotReady         = errors.New("not ready in time")
	errImageNotLoaded   = errors.New("workload image not loaded")
	errImageIDMismatch  = errors.New("workload image ID differs between nodes")
	errNegativeWorkers  = errors.New("worker count must not be negative")
	errUnknownNode      = errors.New("node is not part of the cluster")
	errWorkloadLoadFail = errors.New("workload image load reported an error")
)

// NodeRole distinguishes the Swarm manager from the workers.
type NodeRole string

const (
	// RoleManager identifies the Swarm manager node.
	RoleManager NodeRole = "manager"
	// RoleWorker identifies a Swarm worker node.
	RoleWorker NodeRole = "worker"
)

// Spec is the requested topology and runtime settings of a test cluster.
type Spec struct {
	// EnvID namespaces every resource of the run; SpecFromEnv generates one.
	EnvID string
	// Binary is the exporter binary under test (EnvBinary); the harness itself does not use it.
	Binary string
	// DinDImage is the docker:dind image every node runs.
	DinDImage string
	// Workers is the number of worker nodes; there is always exactly one manager.
	Workers int
	// KeepOnFailure leaves a failed bring-up in place instead of removing it.
	KeepOnFailure bool
	// Logger receives the harness's progress; NewLogger() when nil.
	Logger *slog.Logger
}

// Node is one member of the cluster.
type Node struct {
	// Name is the DinD container name on the host.
	Name string
	// ContainerID is the DinD container ID on the host, the target of PauseNode.
	ContainerID string
	// Hostname is the container hostname, which Swarm reports as the node hostname.
	Hostname string
	// Role is the Swarm role the node was given at bring-up.
	Role NodeRole
	// SwarmNodeID is the node's ID inside the test Swarm.
	SwarmNodeID string
	// HostPort is the 127.0.0.1 port on the host mapped to the node's daemon.
	HostPort int
	// DockerHost is the DOCKER_HOST value that reaches the node's daemon from the host.
	DockerHost string

	// bridgeIP is the node's address on the run's bridge network, which Swarm advertises.
	bridgeIP string
}

// Cluster is a running test Swarm returned by Up. Callers own it and must call Down.
type Cluster struct {
	Spec    Spec
	Manager Node
	// Nodes lists the manager first, then the workers in order.
	Nodes []Node
	// WorkloadImageID is the ID (sha256:…) of the workload image loaded on every node. Services
	// reference it by ID so no node ever contacts a registry.
	WorkloadImageID string
	// StartedAt is when Up began, the lower bound for reading the nodes' event logs.
	StartedAt time.Time

	hostClient *dockerclient.Client
}

// SpecFromEnv returns a Spec built from the SSE_IT_* environment variables, with a freshly
// generated EnvID.
func SpecFromEnv() (Spec, error) {
	workers := defaultWorkerCount

	rawWorkers := os.Getenv(EnvWorkers)
	if rawWorkers != "" {
		parsed, err := strconv.Atoi(rawWorkers)
		if err != nil {
			return Spec{}, fmt.Errorf("parse %s: %w", EnvWorkers, err)
		}

		if parsed < 0 {
			return Spec{}, fmt.Errorf("parse %s=%d: %w", EnvWorkers, parsed, errNegativeWorkers)
		}

		workers = parsed
	}

	keep := false

	rawKeep := os.Getenv(EnvKeepOnFailure)
	if rawKeep != "" {
		parsed, err := strconv.ParseBool(rawKeep)
		if err != nil {
			return Spec{}, fmt.Errorf("parse %s: %w", EnvKeepOnFailure, err)
		}

		keep = parsed
	}

	dindImage := os.Getenv(EnvDinDImage)
	if dindImage == "" {
		dindImage = defaultDinDImage
	}

	envID, err := GenerateEnvID()
	if err != nil {
		return Spec{}, err
	}

	return Spec{
		EnvID:         envID,
		Binary:        os.Getenv(EnvBinary),
		DinDImage:     dindImage,
		Workers:       workers,
		KeepOnFailure: keep,
		Logger:        NewLogger(),
	}, nil
}

// Workers returns the worker nodes, in bring-up order.
func (c *Cluster) Workers() []Node {
	workers := make([]Node, 0, len(c.Nodes))

	for idx := range c.Nodes {
		if c.Nodes[idx].Role == RoleWorker {
			workers = append(workers, c.Nodes[idx])
		}
	}

	return workers
}

// NodeBySwarmID returns the node whose Swarm node ID is swarmNodeID.
func (c *Cluster) NodeBySwarmID(swarmNodeID string) (Node, error) {
	for idx := range c.Nodes {
		if c.Nodes[idx].SwarmNodeID == swarmNodeID {
			return c.Nodes[idx], nil
		}
	}

	return Node{}, fmt.Errorf("swarm node %q: %w", swarmNodeID, errUnknownNode)
}
