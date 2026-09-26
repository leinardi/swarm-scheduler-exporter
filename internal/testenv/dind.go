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

package testenv

// Ported from runhold's internal/testenv (DinD provider), cut down to DinD only: no provider
// interface, no Multipass, a single manager.

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/netip"
	"strconv"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/network"
	"github.com/moby/moby/api/types/swarm"
	dockerclient "github.com/moby/moby/client"
)

const (
	defaultDinDImage = "docker:29.8.1-dind"
	dindDaemonPort   = "2375/tcp"
	swarmManagePort  = "2377"
	daemonReadyWait  = 60 * time.Second
	swarmReadyWait   = 90 * time.Second
	sweepMaxAge      = time.Hour
	pollInterval     = 500 * time.Millisecond

	// opTimeout bounds one ordinary Docker call, so a hung daemon fails that call instead of
	// stalling the run until the suite deadline.
	opTimeout = 30 * time.Second
	// swarmOpTimeout bounds swarm init and join, which wait for Raft and TLS bootstrap.
	swarmOpTimeout = 90 * time.Second
	// imageTransferTimeout bounds one image pull, save or load, which move whole images.
	imageTransferTimeout = 5 * time.Minute
	// teardownTimeout bounds the removal of a failed bring-up.
	teardownTimeout = 2 * time.Minute
)

// Up brings up a fresh cluster for spec: a bridge network, one manager and spec.Workers workers as
// privileged DinD containers, the workload image loaded on every node, and a Swarm whose nodes are
// all Ready. If Up fails, everything it created is removed unless spec.KeepOnFailure is set.
func Up(ctx context.Context, spec Spec) (_ *Cluster, retErr error) {
	normalizeSpec(&spec)

	log := spec.Logger.With(slog.String("envid", spec.EnvID))

	// Logged before anything exists, so a run killed at any later point can still be cleaned up
	// by hand.
	log.Info(
		"bringing up test swarm; manual cleanup",
		slog.String("cmd", CleanupCommand(spec.EnvID)),
	)

	hostClient, err := dockerclient.New(dockerclient.FromEnv)
	if err != nil {
		return nil, fmt.Errorf("connect to host Docker daemon: %w", err)
	}

	cluster := &Cluster{Spec: spec, StartedAt: time.Now(), hostClient: hostClient}

	defer func() {
		if retErr == nil {
			return
		}

		if spec.KeepOnFailure {
			log.Warn(
				"bring-up failed; keeping the environment",
				slog.String("cmd", CleanupCommand(spec.EnvID)),
			)
		} else {
			teardownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), teardownTimeout)
			defer cancel()

			cleanupErr := cleanup(teardownCtx, hostClient, spec.EnvID)
			if cleanupErr != nil {
				log.Warn(
					"bring-up failed and cleanup was incomplete",
					slog.String("err", cleanupErr.Error()),
				)
			}
		}

		_ = hostClient.Close()
	}()

	sweep(ctx, hostClient, sweepMaxAge, log)

	err = bringUp(ctx, cluster, log)
	if err != nil {
		return nil, fmt.Errorf("bring up test swarm %s: %w", spec.EnvID, err)
	}

	return cluster, nil
}

// Down removes every container and network of the cluster and closes its host client. Pass a
// context that is not the one that may have expired during the tests: teardown must still run.
func (c *Cluster) Down(ctx context.Context) error {
	if c == nil {
		return nil
	}

	cleanupErr := cleanup(ctx, c.hostClient, c.Spec.EnvID)
	closeErr := c.hostClient.Close()

	return errors.Join(cleanupErr, closeErr)
}

// PauseNode freezes the node's DinD container. Its daemon stops answering, so the manager marks
// the node down once heartbeats lapse, while the tasks it ran keep their last reported state.
func (c *Cluster) PauseNode(ctx context.Context, node *Node) error {
	_, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerPauseResult, error) {
			return c.hostClient.ContainerPause(
				opCtx,
				node.ContainerID,
				dockerclient.ContainerPauseOptions{},
			)
		},
	)
	if err != nil {
		return fmt.Errorf("pause node %q: %w", node.Name, err)
	}

	return nil
}

// UnpauseNode resumes a node frozen by PauseNode. Unpausing a node that is not paused is not an
// error, so cleanup can call it unconditionally.
func (c *Cluster) UnpauseNode(ctx context.Context, node *Node) error {
	inspected, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerInspectResult, error) {
			return c.hostClient.ContainerInspect(
				opCtx,
				node.ContainerID,
				dockerclient.ContainerInspectOptions{},
			)
		},
	)
	if err != nil {
		return fmt.Errorf("inspect node %q: %w", node.Name, err)
	}

	if inspected.Container.State == nil || !inspected.Container.State.Paused {
		return nil
	}

	_, err = withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerUnpauseResult, error) {
			return c.hostClient.ContainerUnpause(
				opCtx,
				node.ContainerID,
				dockerclient.ContainerUnpauseOptions{},
			)
		},
	)
	if err != nil {
		return fmt.Errorf("unpause node %q: %w", node.Name, err)
	}

	return nil
}

// Client returns a client for the node's daemon. It is built from the node's address alone, never
// from the environment, so a DOCKER_* variable in the caller's shell (TLS, API version pin,
// context) cannot redirect or break it. The caller closes it.
func (n *Node) Client() (*dockerclient.Client, error) {
	cli, err := dockerclient.New(dockerclient.WithHost(n.DockerHost))
	if err != nil {
		return nil, fmt.Errorf("create Docker client for node %q: %w", n.Name, err)
	}

	return cli, nil
}

// CleanupCommand returns a shell command that removes every container (with its volumes) and
// network of the run envID from the host.
func CleanupCommand(envID string) string {
	filter := "label=" + LabelFilter(envID)

	return fmt.Sprintf(
		"docker ps -aq --filter %s | xargs -r docker rm -fv; docker network ls -q --filter %s | xargs -r docker network rm",
		filter,
		filter,
	)
}

// --- Bring-up ---

func normalizeSpec(spec *Spec) {
	if spec.Logger == nil {
		spec.Logger = NewLogger()
	}

	if spec.DinDImage == "" {
		spec.DinDImage = defaultDinDImage
	}
}

func bringUp(ctx context.Context, cluster *Cluster, log *slog.Logger) error {
	spec := &cluster.Spec
	hostClient := cluster.hostClient

	err := ensureHostImage(ctx, hostClient, spec.DinDImage, log)
	if err != nil {
		return err
	}

	networkName := ResourceName(spec.EnvID, "net")

	err = createBridgeNetwork(ctx, hostClient, networkName, spec.EnvID)
	if err != nil {
		return err
	}

	nodes, err := provisionNodes(ctx, hostClient, spec, networkName)
	if err != nil {
		return err
	}

	log.Info("DinD containers started", slog.Int("count", len(nodes)))

	err = waitForDaemons(ctx, nodes)
	if err != nil {
		return err
	}

	log.Info("inner daemons ready")

	cluster.WorkloadImageID, err = loadWorkloadImage(ctx, hostClient, nodes, log)
	if err != nil {
		return err
	}

	workerToken, err := initSwarm(ctx, &nodes[0])
	if err != nil {
		return err
	}

	err = joinWorkers(ctx, nodes[0].bridgeIP, nodes[1:], workerToken)
	if err != nil {
		return err
	}

	err = waitForSwarmReady(ctx, &nodes[0], len(nodes))
	if err != nil {
		return err
	}

	err = recordSwarmNodeIDs(ctx, nodes)
	if err != nil {
		return err
	}

	log.Info("all swarm nodes ready", slog.Int("nodes", len(nodes)))

	cluster.Nodes = nodes
	cluster.Manager = nodes[0]

	return nil
}

func createBridgeNetwork(
	ctx context.Context,
	hostClient *dockerclient.Client,
	name, envID string,
) error {
	_, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.NetworkCreateResult, error) {
			return hostClient.NetworkCreate(opCtx, name, dockerclient.NetworkCreateOptions{
				Driver: "bridge",
				Labels: map[string]string{LabelEnvID: envID},
			})
		},
	)
	if err != nil {
		return fmt.Errorf("create bridge network %q: %w", name, err)
	}

	return nil
}

func provisionNodes(
	ctx context.Context,
	hostClient *dockerclient.Client,
	spec *Spec,
	networkName string,
) ([]Node, error) {
	nodes := make([]Node, 0, spec.Workers+1)

	manager, err := startNode(ctx, hostClient, spec, networkName, "manager-0", RoleManager)
	if err != nil {
		return nil, err
	}

	nodes = append(nodes, manager)

	for idx := range spec.Workers {
		worker, workerErr := startNode(
			ctx,
			hostClient,
			spec,
			networkName,
			fmt.Sprintf("worker-%d", idx),
			RoleWorker,
		)
		if workerErr != nil {
			return nil, workerErr
		}

		nodes = append(nodes, worker)
	}

	return nodes, nil
}

func startNode(
	ctx context.Context,
	hostClient *dockerclient.Client,
	spec *Spec,
	networkName, hostname string,
	role NodeRole,
) (Node, error) {
	name := ResourceName(spec.EnvID, hostname)
	portSpec := network.MustParsePort(dindDaemonPort)

	created, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerCreateResult, error) {
			return hostClient.ContainerCreate(opCtx, dockerclient.ContainerCreateOptions{
				Config: &container.Config{
					Image:    spec.DinDImage,
					Hostname: hostname,
					// An empty DOCKER_TLS_CERTDIR makes the entrypoint serve plain TCP on 2375. The
					// explicit --tls=false matters too: without it dockerd deliberately stalls its
					// start-up for seconds to warn about unauthenticated TCP.
					Env:          []string{"DOCKER_TLS_CERTDIR="},
					Cmd:          []string{"--tls=false"},
					Labels:       map[string]string{LabelEnvID: spec.EnvID},
					ExposedPorts: network.PortSet{portSpec: struct{}{}},
				},
				HostConfig: &container.HostConfig{
					Privileged: true,
					PortBindings: network.PortMap{
						portSpec: []network.PortBinding{
							{HostIP: netip.AddrFrom4([4]byte{127, 0, 0, 1}), HostPort: "0"},
						},
					},
				},
				NetworkingConfig: &network.NetworkingConfig{
					EndpointsConfig: map[string]*network.EndpointSettings{networkName: {}},
				},
				Name: name,
			})
		},
	)
	if err != nil {
		return Node{}, fmt.Errorf("create node %q: %w", name, err)
	}

	_, err = withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerStartResult, error) {
			return hostClient.ContainerStart(
				opCtx,
				created.ID,
				dockerclient.ContainerStartOptions{},
			)
		},
	)
	if err != nil {
		return Node{}, fmt.Errorf("start node %q: %w", name, err)
	}

	return inspectNode(ctx, hostClient, created.ID, name, hostname, role, networkName)
}

func inspectNode(
	ctx context.Context,
	hostClient *dockerclient.Client,
	containerID, name, hostname string,
	role NodeRole,
	networkName string,
) (Node, error) {
	inspected, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerInspectResult, error) {
			return hostClient.ContainerInspect(
				opCtx,
				containerID,
				dockerclient.ContainerInspectOptions{},
			)
		},
	)
	if err != nil {
		return Node{}, fmt.Errorf("inspect node %q: %w", name, err)
	}

	settings := inspected.Container.NetworkSettings
	if settings == nil {
		return Node{}, fmt.Errorf("node %q on network %q: %w", name, networkName, errNotAttached)
	}

	endpoint, ok := settings.Networks[networkName]
	if !ok || endpoint == nil || !endpoint.IPAddress.IsValid() {
		return Node{}, fmt.Errorf("node %q on network %q: %w", name, networkName, errNotAttached)
	}

	bindings := settings.Ports[network.MustParsePort(dindDaemonPort)]
	if len(bindings) == 0 {
		return Node{}, fmt.Errorf("node %q port %s: %w", name, dindDaemonPort, errNoHostPort)
	}

	hostPort, err := strconv.Atoi(bindings[0].HostPort)
	if err != nil {
		return Node{}, fmt.Errorf(
			"node %q: parse host port %q: %w",
			name,
			bindings[0].HostPort,
			err,
		)
	}

	return Node{
		Name:        name,
		ContainerID: containerID,
		Hostname:    hostname,
		Role:        role,
		HostPort:    hostPort,
		DockerHost:  fmt.Sprintf("tcp://127.0.0.1:%d", hostPort),
		bridgeIP:    endpoint.IPAddress.String(),
	}, nil
}

// waitForDaemons is the first readiness phase: every inner daemon answers a ping.
func waitForDaemons(ctx context.Context, nodes []Node) error {
	for idx := range nodes {
		node := &nodes[idx]

		err := withNodeClient(node, func(cli *dockerclient.Client) error {
			return pollUntil(
				ctx,
				daemonReadyWait,
				"daemon on "+node.Name+" answers ping",
				func() error {
					_, pingErr := withTimeout(
						ctx,
						opTimeout,
						func(opCtx context.Context) (dockerclient.PingResult, error) {
							return cli.Ping(opCtx, dockerclient.PingOptions{})
						},
					)

					return pingErr
				},
			)
		})
		if err != nil {
			return err
		}
	}

	return nil
}

func initSwarm(ctx context.Context, manager *Node) (string, error) {
	var workerToken string

	err := withNodeClient(manager, func(cli *dockerclient.Client) error {
		_, initErr := withTimeout(
			ctx,
			swarmOpTimeout,
			func(opCtx context.Context) (dockerclient.SwarmInitResult, error) {
				return cli.SwarmInit(opCtx, dockerclient.SwarmInitOptions{
					ListenAddr:    "0.0.0.0:" + swarmManagePort,
					AdvertiseAddr: manager.bridgeIP,
				})
			},
		)
		if initErr != nil {
			return fmt.Errorf("swarm init on %q: %w", manager.Name, initErr)
		}

		inspected, inspectErr := withTimeout(
			ctx,
			opTimeout,
			func(opCtx context.Context) (dockerclient.SwarmInspectResult, error) {
				return cli.SwarmInspect(opCtx, dockerclient.SwarmInspectOptions{})
			},
		)
		if inspectErr != nil {
			return fmt.Errorf("swarm inspect on %q: %w", manager.Name, inspectErr)
		}

		workerToken = inspected.Swarm.JoinTokens.Worker

		return nil
	})

	return workerToken, err
}

func joinWorkers(
	ctx context.Context,
	managerBridgeIP string,
	workers []Node,
	workerToken string,
) error {
	remoteAddr := managerBridgeIP + ":" + swarmManagePort

	for idx := range workers {
		worker := &workers[idx]

		err := withNodeClient(worker, func(cli *dockerclient.Client) error {
			_, joinErr := withTimeout(
				ctx,
				swarmOpTimeout,
				func(opCtx context.Context) (dockerclient.SwarmJoinResult, error) {
					return cli.SwarmJoin(opCtx, dockerclient.SwarmJoinOptions{
						ListenAddr:    "0.0.0.0:" + swarmManagePort,
						AdvertiseAddr: worker.bridgeIP,
						RemoteAddrs:   []string{remoteAddr},
						JoinToken:     workerToken,
					})
				},
			)

			return joinErr
		})
		if err != nil {
			return fmt.Errorf("swarm join on %q: %w", worker.Name, err)
		}
	}

	return nil
}

// waitForSwarmReady is the second readiness phase: the manager lists every node as Ready.
func waitForSwarmReady(ctx context.Context, manager *Node, totalNodes int) error {
	return withNodeClient(manager, func(cli *dockerclient.Client) error {
		return pollUntil(
			ctx,
			swarmReadyWait,
			fmt.Sprintf("%d swarm nodes Ready", totalNodes),
			func() error {
				listed, err := withTimeout(
					ctx,
					opTimeout,
					func(opCtx context.Context) (dockerclient.NodeListResult, error) {
						return cli.NodeList(opCtx, dockerclient.NodeListOptions{})
					},
				)
				if err != nil {
					return fmt.Errorf("node list: %w", err)
				}

				ready := 0

				for idx := range listed.Items {
					if listed.Items[idx].Status.State == swarm.NodeStateReady {
						ready++
					}
				}

				if ready != totalNodes {
					return fmt.Errorf("%d of %d nodes Ready: %w", ready, totalNodes, errNotReady)
				}

				return nil
			},
		)
	})
}

func recordSwarmNodeIDs(ctx context.Context, nodes []Node) error {
	for idx := range nodes {
		node := &nodes[idx]

		err := withNodeClient(node, func(cli *dockerclient.Client) error {
			info, infoErr := withTimeout(
				ctx,
				opTimeout,
				func(opCtx context.Context) (dockerclient.SystemInfoResult, error) {
					return cli.Info(opCtx, dockerclient.InfoOptions{})
				},
			)
			if infoErr != nil {
				return fmt.Errorf("info on %q: %w", node.Name, infoErr)
			}

			node.SwarmNodeID = info.Info.Swarm.NodeID

			return nil
		})
		if err != nil {
			return err
		}
	}

	return nil
}

// ensureHostImage makes imageRef available on the host daemon, pulling it only when it is missing.
func ensureHostImage(
	ctx context.Context,
	hostClient *dockerclient.Client,
	imageRef string,
	log *slog.Logger,
) error {
	_, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ImageInspectResult, error) {
			return hostClient.ImageInspect(opCtx, imageRef)
		},
	)
	if err == nil {
		return nil
	}

	if !cerrdefs.IsNotFound(err) {
		return fmt.Errorf("inspect host image %q: %w", imageRef, err)
	}

	log.Info("pulling image on host", slog.String("image", imageRef))

	pullCtx, cancel := context.WithTimeout(ctx, imageTransferTimeout)
	defer cancel()

	pulled, err := hostClient.ImagePull(pullCtx, imageRef, dockerclient.ImagePullOptions{})
	if err != nil {
		return fmt.Errorf("pull host image %q: %w", imageRef, err)
	}
	defer pulled.Close()

	err = pulled.Wait(pullCtx)
	if err != nil {
		return fmt.Errorf("pull host image %q: %w", imageRef, err)
	}

	return nil
}

// --- Teardown ---

// sweep removes the environments of earlier runs that are older than maxAge — leaks from runs that
// were killed before their teardown ran. Recent environments are left alone: they may belong to a
// run still in progress.
func sweep(
	ctx context.Context,
	hostClient *dockerclient.Client,
	maxAge time.Duration,
	log *slog.Logger,
) {
	filters := make(dockerclient.Filters).Add("label", LabelEnvID)
	cutoff := time.Now().Add(-maxAge)
	stale := make(map[string]bool)

	containers, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerListResult, error) {
			return hostClient.ContainerList(
				opCtx,
				dockerclient.ContainerListOptions{All: true, Filters: filters},
			)
		},
	)
	if err != nil {
		log.Warn("sweep: list containers", slog.String("err", err.Error()))
	}

	for idx := range containers.Items {
		ctr := &containers.Items[idx]
		if time.Unix(ctr.Created, 0).Before(cutoff) {
			stale[ctr.Labels[LabelEnvID]] = true
		}
	}

	networks, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.NetworkListResult, error) {
			return hostClient.NetworkList(opCtx, dockerclient.NetworkListOptions{Filters: filters})
		},
	)
	if err != nil {
		log.Warn("sweep: list networks", slog.String("err", err.Error()))
	}

	for idx := range networks.Items {
		net := &networks.Items[idx]
		if net.Created.Before(cutoff) {
			stale[net.Labels[LabelEnvID]] = true
		}
	}

	for envID := range stale {
		log.Info("sweep: removing leaked environment", slog.String("leaked_envid", envID))

		cleanupErr := cleanup(ctx, hostClient, envID)
		if cleanupErr != nil {
			log.Warn(
				"sweep: cleanup incomplete",
				slog.String("leaked_envid", envID),
				slog.String("err", cleanupErr.Error()),
			)
		}
	}
}

// cleanup removes every container (with its anonymous /var/lib/docker volume) and every network
// labeled with envID. It keeps going past individual failures and returns them joined.
func cleanup(ctx context.Context, hostClient *dockerclient.Client, envID string) error {
	filters := make(dockerclient.Filters).Add("label", LabelFilter(envID))

	var errs []error

	containers, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.ContainerListResult, error) {
			return hostClient.ContainerList(
				opCtx,
				dockerclient.ContainerListOptions{All: true, Filters: filters},
			)
		},
	)
	if err != nil {
		errs = append(errs, fmt.Errorf("list containers: %w", err))
	}

	for idx := range containers.Items {
		ctrID := containers.Items[idx].ID

		_, rmErr := withTimeout(
			ctx,
			opTimeout,
			func(opCtx context.Context) (dockerclient.ContainerRemoveResult, error) {
				return hostClient.ContainerRemove(
					opCtx,
					ctrID,
					dockerclient.ContainerRemoveOptions{Force: true, RemoveVolumes: true},
				)
			},
		)
		if rmErr != nil && !cerrdefs.IsNotFound(rmErr) {
			errs = append(errs, fmt.Errorf("remove container %s: %w", ctrID, rmErr))
		}
	}

	networks, err := withTimeout(
		ctx,
		opTimeout,
		func(opCtx context.Context) (dockerclient.NetworkListResult, error) {
			return hostClient.NetworkList(opCtx, dockerclient.NetworkListOptions{Filters: filters})
		},
	)
	if err != nil {
		errs = append(errs, fmt.Errorf("list networks: %w", err))
	}

	for idx := range networks.Items {
		netID := networks.Items[idx].ID

		_, rmErr := withTimeout(
			ctx,
			opTimeout,
			func(opCtx context.Context) (dockerclient.NetworkRemoveResult, error) {
				return hostClient.NetworkRemove(opCtx, netID, dockerclient.NetworkRemoveOptions{})
			},
		)
		if rmErr != nil && !cerrdefs.IsNotFound(rmErr) {
			errs = append(errs, fmt.Errorf("remove network %s: %w", netID, rmErr))
		}
	}

	return errors.Join(errs...)
}

// --- Helpers ---

// withTimeout runs one Docker call under its own deadline derived from ctx.
//
//nolint:ireturn // T is the concrete result type of the wrapped Docker call, not an interface
func withTimeout[T any](
	ctx context.Context,
	timeout time.Duration,
	call func(context.Context) (T, error),
) (T, error) {
	opCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	return call(opCtx)
}

// withNodeClient runs useClient with a client for node's daemon and closes it afterwards.
func withNodeClient(node *Node, useClient func(*dockerclient.Client) error) error {
	cli, err := node.Client()
	if err != nil {
		return err
	}
	defer cli.Close()

	return useClient(cli)
}

// pollUntil calls check every pollInterval until it returns nil, timeout elapses, or ctx is done.
// The error names what was awaited and carries check's last error.
func pollUntil(ctx context.Context, timeout time.Duration, what string, check func() error) error {
	deadline := time.Now().Add(timeout)

	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()

	for {
		lastErr := check()
		if lastErr == nil {
			return nil
		}

		if time.Now().After(deadline) {
			return fmt.Errorf("%s within %s: %w: %w", what, timeout, errNotReady, lastErr)
		}

		select {
		case <-ctx.Done():
			return fmt.Errorf("%s: %w: %w", what, ctx.Err(), lastErr)
		case <-ticker.C:
		}
	}
}
