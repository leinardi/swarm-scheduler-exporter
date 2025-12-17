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

// containers exposes an info-style gauge per container with a 'state' label,
// covering Docker runtime states plus health overlay states. It also provides
// the exit code (as a label) when state == "exited" (empty otherwise).

import (
	"context"
	"fmt"
	"maps"
	"strconv"
	"strings"
	"time"

	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/client"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
	"github.com/prometheus/client_golang/prometheus"
)

// Known Docker container states (runtime) + health overlay states.
// We emit exhaustively across this set per container.
var knownContainerStates = []string{
	"created",
	"restarting",
	"running",
	"removing",
	"paused",
	"exited",
	"dead",
	// Health overlay (when running and healthcheck present)
	"healthy",
	"unhealthy",
	"health_starting",
}

const (
	// Cap the number of container inspects per poll to bound overhead.
	// We only inspect running+healthcheck (to read health) and exited (to read exit code).
	containersInspectCapDefault = 300

	// Short timeout per poll just for container listing/inspection.
	containersPollTimeout = 5 * time.Second
)

var (
	// Enabled via CLI (set in main). When false, this collector is inert.
	containersEnabled bool

	// includeSwarmTaskContainers selects whether containers belonging to Swarm tasks
	// should be included. Default false to avoid duplication with task metrics.
	containersIncludeSwarm bool

	// Optional hard cap override; kept private with a sensible default.
	containersInspectCap = containersInspectCapDefault

	// Metric: info-style gauge with a 'state' label (exactly one is 1, others 0).
	// Labels:
	//   project      (compose) or empty
	//   stack        (swarm)   or empty
	//   service      (compose/swarm) or empty
	//   container    (sanitized first name)
	//   orchestrator compose|swarm|none
	//   display_name if project/stack and service present:
	//                - if eq -> that value
	//                - else   -> "<stack|project> <service>"
	//                Else fallback to container name.
	//   state        (see knownContainerStates)
	//   exit_code    string exit code when state="exited", else ""
	containersStateGauge *prometheus.GaugeVec
)

// inspectNeed enumerates enrichment needs per container.
type inspectNeed int

const (
	needNone inspectNeed = iota
	needHealth
	needExit
)

// row is the working record we enrich per container during a poll.
type row struct {
	id     string
	state  string // base state (from list)
	labels prometheus.Labels
	need   inspectNeed
}

// ConfigureContainersStateGauge registers the container state gauge.
// No-ops if already configured.
func ConfigureContainersStateGauge() {
	if containersStateGauge != nil {
		return
	}

	baseLabels := []string{
		"project",
		"stack",
		"service",
		"container",
		"orchestrator",
		"display_name",
		"state",
		"exit_code",
	}

	containersStateGauge = prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace:   prometheusNamespace,
		Subsystem:   "container",
		Name:        "state",
		Help:        "Container state info (exactly one state=1 per container). Includes exit_code for exited.",
		ConstLabels: nil,
	}, labelutil.SanitizeLabelNames(baseLabels))

	prometheus.MustRegister(containersStateGauge)
}

// EnableContainersMetrics toggles the container collector (wired from main).
func EnableContainersMetrics(enable, includeSwarm bool) {
	containersEnabled = enable
	containersIncludeSwarm = includeSwarm
}

// PollContainersState lists containers and returns a slice of per-container label/value rows.
// It uses a bounded number of inspects to enrich:
//   - running+healthcheck → health overlay state (healthy/unhealthy/health_starting)
//   - exited              → exit_code
func PollContainersState(
	parentCtx context.Context,
	cli *client.Client,
) ([]prometheus.Labels, error) {
	if !containersEnabled {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(parentCtx, containersPollTimeout)
	defer cancel()

	list, listErr := cli.ContainerList(ctx, container.ListOptions{
		All:  true,  // include exited
		Size: false, // avoid size overhead
	})
	if listErr != nil {
		return nil, fmt.Errorf("container list: %w", listErr)
	}

	rows := make([]row, 0, len(list))

	for idx := range list {
		cnt := &list[idx]

		// Skip swarm task containers unless explicitly requested.
		isSwarm := strings.TrimSpace(cnt.Labels["com.docker.swarm.service.name"]) != ""
		if isSwarm && !containersIncludeSwarm {
			continue
		}

		// Classify orchestrator + labels.
		project, service, stack, orchestrator := classifyOrchestrator(cnt.Labels)

		// Container name: pick the first Names entry; strip leading '/'.
		containerName := firstContainerName(cnt.Names)

		// Display name:
		// - If we have a (project||stack) and service, use the "<proj|stack> <service>" rule (or equal-folding rule).
		// - Else fallback to the container name.
		var group string
		if project != "" {
			group = project
		} else {
			group = stack
		}

		friendly := displayNameForContainer(group, service, containerName)

		base := prometheus.Labels{
			"project":      project,
			"stack":        stack,
			"service":      service,
			"container":    containerName,
			"orchestrator": orchestrator,
			"display_name": friendly,
			// 'state' and 'exit_code' are added during enrichment/emission
		}

		// Determine if we need an inspect:
		need := needNone

		switch cnt.State {
		case "running":
			// Only inspect running containers that *might* have a healthcheck.
			// We don’t know healthcheck presence from the list; inspect to confirm.
			need = needHealth
		case "exited":
			// Capture the exit code.
			need = needExit
		default:
			// No inspect needed for other states.
		}

		rows = append(rows, row{
			id:     cnt.ID,
			state:  cnt.State,
			labels: base,
			need:   need,
		})
	}

	// Enrich a bounded subset via inspect.
	enrichContainers(parentCtx, cli, rows)

	// Convert to prometheus.Labels rows, one per container (we write exhaustive states later).
	out := make([]prometheus.Labels, 0, len(rows))
	for rowIdx := range rows {
		out = append(out, rows[rowIdx].labels)
	}

	return out, nil
}

// UpdateContainersStateGauge emits one-hot state series per container.
// It resets the vector first to drop vanished containers.
func UpdateContainersStateGauge(rows []prometheus.Labels) {
	if !containersEnabled || containersStateGauge == nil {
		return
	}

	containersStateGauge.Reset()

	for rowIdx := range rows {
		// We expect these prefilled:
		// - base fields (project/stack/service/container/orchestrator/display_name)
		// - computed 'state' (may be health overlay) and 'exit_code' (empty unless exited)
		base := rows[rowIdx]

		// Emit exhaustively across knownContainerStates.
		for si := range knownContainerStates {
			state := knownContainerStates[si]

			lbls := make(prometheus.Labels, len(base)+2)
			maps.Copy(lbls, base)

			lbls["state"] = state

			// exit_code must exist (even when empty) to keep label cardinality constant.
			exit := base["exit_code"]
			lbls["exit_code"] = exit

			value := 0.0
			if base["state"] == state {
				value = 1.0
			}

			containersStateGauge.With(labelutil.SanitizeMetricLabels(lbls)).Set(value)
		}
	}
}

// --- helpers ---

// classifyOrchestrator inspects labels to decide orchestrator + names.
func classifyOrchestrator(lbls map[string]string) (project, service, stack, orchestrator string) {
	project = strings.TrimSpace(lbls["com.docker.compose.project"])
	service = strings.TrimSpace(lbls["com.docker.compose.service"])
	stack = strings.TrimSpace(lbls["com.docker.stack.namespace"])
	swarmSvc := strings.TrimSpace(lbls["com.docker.swarm.service.name"])

	switch {
	case project != "":
		orchestrator = "compose"
	case swarmSvc != "":
		orchestrator = "swarm"

		if service == "" {
			service = shortServiceName(stack, swarmSvc)
		}
	default:
		orchestrator = "none"
		// Best effort: some plain containers still carry labels like "org.opencontainers.image.title".
	}

	return project, service, stack, orchestrator
}

func firstContainerName(names []string) string {
	if len(names) == 0 {
		return ""
	}

	name := names[0]

	return strings.TrimPrefix(name, "/")
}

// displayNameForContainer applies the same rule you requested for stack/service,
// but falls back to the container name when there’s no group/service pair.
func displayNameForContainer(group, service, containerName string) string {
	if group != "" && service != "" {
		if group == service {
			return group
		}

		return group + " " + service
	}

	// Fallback
	return containerName
}

// enrichContainers performs bounded ContainerInspect calls to read:
//   - health overlay for running containers with healthcheck (healthy/unhealthy/health_starting)
//   - exit_code for exited containers
//
// NOTE: we always ensure 'state' and 'exit_code' labels are set on each row:
//   - 'state' is either the base docker state or a health overlay
//   - 'exit_code' is "" unless state == "exited"
func enrichContainers(parentCtx context.Context, cli *client.Client, rows []row) {
	inspected := 0

	for rowIdx := range rows {
		// Pre-fill labels for consistent emission.
		rows[rowIdx].labels["exit_code"] = ""

		// If no enrichment is needed, use base state and continue.
		if rows[rowIdx].need == needNone {
			rows[rowIdx].labels["state"] = rows[rowIdx].state

			continue
		}

		// Respect the inspect cap for the poll.
		if inspected >= containersInspectCap {
			logger.L().Debug("containers inspect cap reached, skipping remaining enrichments",
				"cap", containersInspectCap,
			)

			rows[rowIdx].labels["state"] = rows[rowIdx].state

			continue
		}

		ctx, cancel := context.WithTimeout(parentCtx, containersPollTimeout)
		inspectedJSON, inspectErr := cli.ContainerInspect(ctx, rows[rowIdx].id)

		cancel()

		if inspectErr != nil {
			// Keep base state; leave exit_code empty.
			logger.L().Warn("container inspect failed; using base state",
				"container_id", rows[rowIdx].id, "err", inspectErr,
			)
			rows[rowIdx].labels["state"] = rows[rowIdx].state

			continue
		}

		inspected++

		// Delegate per-need logic to reduce complexity.
		switch rows[rowIdx].need {
		case needHealth:
			applyHealthOverlay(&rows[rowIdx], inspectedJSON)
		case needExit:
			applyExitCode(&rows[rowIdx], inspectedJSON)
		case needNone:
			// No enrichment; keep the base docker state.
			rows[rowIdx].labels["state"] = rows[rowIdx].state
		}
	}
}

// applyHealthOverlay overlays a health state if the container has a healthcheck.
func applyHealthOverlay(rowRef *row, inspected types.ContainerJSON) {
	if inspected.Config != nil &&
		inspected.Config.Healthcheck != nil &&
		inspected.State != nil &&
		inspected.State.Health != nil {
		switch strings.ToLower(strings.TrimSpace(inspected.State.Health.Status)) {
		case "healthy":
			rowRef.labels["state"] = "healthy"

			return
		case "unhealthy":
			rowRef.labels["state"] = "unhealthy"

			return
		case "starting":
			rowRef.labels["state"] = "health_starting"

			return
		}
	}
	// No healthcheck or unknown status → keep base.
	rowRef.labels["state"] = rowRef.state
}

// applyExitCode annotates the exit code for exited containers.
func applyExitCode(rowRef *row, inspected types.ContainerJSON) {
	rowRef.labels["state"] = rowRef.state

	if inspected.State == nil {
		return
	}

	if !strings.EqualFold(rowRef.state, "exited") {
		return
	}

	rowRef.labels["exit_code"] = strconv.Itoa(inspected.State.ExitCode)
}
