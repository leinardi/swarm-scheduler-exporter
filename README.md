# Swarm Scheduler Exporter

Prometheus exporter for Docker Swarm focused on **task state visibility**, **accurate desired replicas**, and **operability at scale**.
Fork of (and huge thanks to) **[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**.

## 💡 What is “Swarm Scheduler Exporter”?

**Swarm Scheduler Exporter** surfaces what Docker Swarm’s **scheduler** is doing right now, and *why*. It reports **desired replicas** (including accurate
eligibility for `global` services), **live task state** per service (latest per slot), **service update/rollback** state + timestamps, **cluster node**
availability, and now **SLO-friendly service readiness** signals.

## 📦 What This Exporter Does

- Watches Swarm **service** and **node** events to keep metrics fresh (resilient reconnect, bounded worker pool).
- Periodically polls **tasks** and aggregates **current** states per service (latest per slot, exhaustive zero-emission).
- Computes **desired replicas** precisely for `global` services (eligible nodes only: status/availability/constraints/platforms).
- Emits **exporter** and **cluster** health metrics for alerting & SLOs.
- Sanitizes and validates **custom labels** for Prometheus compliance and safe cardinality.

## 🪶 Resource usage

Swarm Scheduler Exporter is designed to be lightweight. In typical Docker deployments it can sit around **~25 MiB RAM** when idle (exact usage depends on
platform, Go version, and container runtime settings).

## 📊 Metrics

All metrics live under the `swarm_` namespace.

### Service-level

- `swarm_service_desired_replicas{stack,service,service_mode,...custom}`
  Desired replicas (**replicated**: configured replicas; **global**: eligible nodes).

- `swarm_task_replicas_state{stack,service,service_mode,state,...custom}`
  **Latest-per-slot** task count by state (always emits zeros for all known states per current service).

- `swarm_service_running_replicas{stack,service,service_mode,...custom}`
  Number of **currently running** tasks per service (latest-per-slot view, same snapshot as `replicas_state`).

- `swarm_service_at_desired{stack,service,service_mode,...custom}`
  `1` if `running_replicas == desired_replicas`, else `0`. Useful for dead-simple SLOs and alerting.

- `swarm_service_schedulable_replicas{stack,service,service_mode,...custom}`
  Number of replicas that can currently be scheduled given node availability and placement constraints.
  **Replicated**: `min(configured_replicas, eligible_nodes)` — drops to `0` when the only eligible node for a constrained service is offline.
  **Global**: equal to `desired_replicas` (eligible-node count).
  Prefer this over `desired_replicas` in alert expressions to suppress false positives when a pinned node is down:

  ```promql
  swarm_service_running_replicas < on(stack,service,service_mode,display_name) swarm_service_schedulable_replicas
  ```

> ℹ️ **Global services with 0 eligible nodes:** `desired_replicas=0`, `running_replicas` usually `0` ⇒ `at_desired=1`.

### Service update/rollback (info-style)

- `swarm_service_update_state_info{stack,service,service_mode,state}` = `1` for the *current* state, else `0`.
  States: `updating`, `completed`, `paused`, `rollback_started`, `rollback_completed`.

- `swarm_service_update_started_timestamp_seconds{...}`
- `swarm_service_update_completed_timestamp_seconds{...}`

### Cluster / node visibility

- `swarm_cluster_nodes_by_state{role,availability,status}`
  Count of nodes by manager/worker, active/pause/drain, and ready/down/… .

### Exporter self-metrics

- `swarm_exporter_health` — `1` healthy / `0` unhealthy.
- `swarm_exporter_build_info{version,commit,date}` — `1`.
- `swarm_exporter_polls_total` / `swarm_exporter_poll_errors_total`.
- `swarm_exporter_poll_duration_seconds` (histogram).
- `swarm_exporter_events_reconnects_total`.

### Container-level (opt-in)

If you also run non-Swarm workloads (e.g. plain Docker Compose or standalone containers),
the exporter can expose **container state** metrics when started with `-containers`.

- `swarm_container_state{project,stack,service,container,orchestrator,display_name,state,exit_code}`
  Emits an **info-style one-hot series** per container across all known states.
  Exactly one time series per container has `1`, the rest are `0`.

    - `project` / `service` — from Compose labels (`com.docker.compose.*`), if present
    - `stack` — from Swarm labels (`com.docker.stack.namespace`), if present
    - `container` — sanitized container name
    - `orchestrator` — `compose`, `swarm`, or `none`
    - `display_name` — friendly name (`stack service` or `stack` if identical)
    - `state` — one of
      `created`, `restarting`, `running`, `removing`, `paused`, `exited`, `dead`, `healthy`, `unhealthy`, `health_starting`
    - `exit_code` — string exit code (only when `state="exited"`, otherwise empty)

> ℹ️ The exporter inspects only a **bounded subset** of containers per poll:
> running containers with healthchecks (for health state) and exited containers (for exit code).
> Swarm task containers are skipped unless `-containers-include-swarm` is set.

## ✅ Health

- HTTP: `/healthz` responds `200` when the exporter is healthy.
- Metric: `swarm_exporter_health` mirrors health for scraping/alerting.

## 🚀 Quick Start

When running the exporter inside a container, it needs permission to talk to the Docker Engine.
On most systems, this means allowing access to the Docker UNIX socket at `/var/run/docker.sock`.

### 🔍 Finding the correct socket group

Docker’s socket is owned by a specific group (e.g., `docker` or `root`).
Check the numeric group ID (GID) on your system:

```bash
stat -c %g /var/run/docker.sock
```

Use that GID in the `--group` or `--group-add` flag so the container’s user
(in the distroless image it’s a nonroot user, UID 65532) can connect to the socket.

If you skip this step, you’ll see errors like:

```
permission denied while trying to connect to the Docker daemon socket
```

> ⚠️ The GID must be the same on **all Swarm manager nodes** if you use a bind mount for the socket.
> If GIDs differ, use the **TCP/TLS approach** below instead of the socket.

### 🐳 Docker (single host)

```bash
docker run --rm \
 -p 8888:8888 \
 -v /var/run/docker.sock:/var/run/docker.sock:ro \
 --group-add 140 \
 ghcr.io/leinardi/swarm-scheduler-exporter:latest \
 -log-format text \
 -log-level warn
```

Replace `140` with the value from `stat -c %g /var/run/docker.sock`.

### 🐝 Swarm service (recommended)

```bash
docker service create \
 --name swarm-scheduler-exporter \
 --mode replicated --replicas 1 \
 --constraint 'node.role == manager' \
 --group 140 \
 --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock \
 --read-only \
 --mount type=tmpfs,dst=/tmp,tmpfs-size=16m \
 --publish published=8888,target=8888 \
 ghcr.io/leinardi/swarm-scheduler-exporter:latest \
 -poll-delay 10s
```

> ℹ️ **Why the `manager` constraint?**
> Only manager nodes can access cluster-wide service, node, and event data required by the exporter.

### 🔐 Alternative: TCP/TLS (no socket mount)

If your nodes have mismatched socket GIDs or you prefer not to expose `/var/run/docker.sock`,
you can use Docker’s authenticated API instead:

```bash
docker service create \
 --name swarm-scheduler-exporter \
 --mode replicated --replicas 1 \
 --constraint 'node.role == manager' \
 --read-only \
 --mount type=tmpfs,dst=/tmp,tmpfs-size=16m \
 --publish published=8888,target=8888 \
 --env DOCKER_HOST=tcp://manager.example.internal:2376 \
 --env DOCKER_TLS_VERIFY=1 \
 --mount type=bind,src=/path/to/certs,dst=/run/certs,ro \
 --env DOCKER_CERT_PATH=/run/certs \
 ghcr.io/leinardi/swarm-scheduler-exporter:latest \
 -poll-delay 10s
```

This avoids group and permission issues, relying instead on proper TLS authentication.

### 🧩 Docker Compose Example

A complete Compose setup (replicated mode, manager constraint, and environment hints)
is available at:
[`deployments/docker/docker-compose.yaml`](deployments/docker/docker-compose.yaml)

## ⚙️ Configuration

### Flags

```
  -containers
        Expose container state metrics (opt-in).
  -containers-include-swarm
        Include containers belonging to Swarm tasks.
  -help
        Display help message
  -label value
        Name of custom service labels to add to metrics
  -listen-addr string
        IP address and port to bind (default "0.0.0.0:8888")
  -log-format string
        Either json, text or plain (default "text")
  -log-level string
        Either debug, info, warn, error, fatal, panic (default "info")
  -log-time
        Include timestamp in logs
  -poll-delay duration
        How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s. (default 10s)
```

### Environment (Docker client)

- `DOCKER_HOST` — Docker daemon URL
- `DOCKER_CERT_PATH` — Path to TLS certs
- `DOCKER_TLS_VERIFY` — Enable TLS verification (set to `1`)

### Custom label guardrails

- Names are validated & **sanitized** to Prometheus label rules
  (e.g., `app.kubernetes.io/name` → `app_kubernetes_io_name`).
- Duplicate/colliding sanitized names are rejected at startup.
- Max number of custom label keys is bounded (sane default).
- Suspicious **high-cardinality values** log a one-time warning.

## 🔔 Example Alerts

Prometheus Alert rule:

```yaml
# ─────────────────────────────────────────────────────────────
# Swarm service not at desired replicas (and not just updating)
# ─────────────────────────────────────────────────────────────
- alert: SwarmServiceNotAtDesired
  expr: |
    (swarm_service_at_desired == 0)
    and on (stack, service)
    (swarm_service_update_state_info{state="updating"} == 0)
  for: 5m
  labels:
    severity: warning
    service: swarm
  annotations:
    summary: "Service {{ $labels.stack }}/{{ $labels.service }} not at desired replicas"
    description: |
      The Swarm service {{ $labels.stack }}/{{ $labels.service }} is not running
      at its desired replica count for more than 5 minutes and is not in
      an 'updating' state.
      Check the service tasks, node availability, and recent changes.

# ─────────────────────────────────────────────────────────────
# Swarm service in rollback state
# ─────────────────────────────────────────────────────────────
- alert: SwarmServiceRollbackOngoing
  expr: |
    sum by (stack, service, display_name) (
      swarm_service_update_state_info{state=~"rollback_(started|completed)"}
    ) > 0
  for: 1m
  labels:
    severity: warning
    service: swarm
  annotations:
    summary: "Service rollback detected for {{ $labels.display_name }}"
    description: |
      The Swarm service {{ $labels.display_name }} is in a rollback state
      (rollback_started or rollback_completed).
      Investigate the deployment history and task failures.

# ─────────────────────────────────────────────────────────────
# Containers in an unhealthy / failed state (Compose/standalone)
#   - Includes: unhealthy, exited, dead
#   - Excludes: one-shot containers that exited/dead with exit_code=0
# ─────────────────────────────────────────────────────────────
- alert: ContainerUnhealthy
  expr: |
    (
      sum by (display_name, stack, service) (
        swarm_container_state{
          orchestrator=~"compose|none",
          state=~"unhealthy|exited|dead"
        }
      ) > 0
    )
    UNLESS
    (
      sum by (display_name, stack, service) (
        swarm_container_state{
          orchestrator=~"compose|none",
          state=~"exited|dead",
          exit_code="0"
        }
      ) > 0
    )
  for: 5m
  labels:
    severity: warning
    service: containers
  annotations:
    summary: "Container unhealthy or failed: {{ $labels.display_name }}"
    description: |
      The container {{ $labels.display_name }} is unhealthy, exited, or dead
      (and not a clean one-shot exit with code 0) for more than 5 minutes.
      Check logs and recent changes to this container.

# ─────────────────────────────────────────────────────────────
# Swarm cluster node(s) not ready
# ─────────────────────────────────────────────────────────────
- alert: SwarmClusterNodeNotReady
  expr: swarm_cluster_nodes_by_state{status!="ready"} > 0
  for: 5m
  labels:
    severity: warning
    service: swarm
  annotations:
    summary: "Swarm cluster has node(s) not ready"
    description: |
      One or more Swarm nodes are not in 'ready' status.
      Check 'docker node ls' and node availability / connectivity.
```

## 🧪 Quick checks

- **Metrics**: `curl http://<host>:8888/metrics`
- **Health**: `curl -s -o /dev/null -w "%{http_code}\n" http://<host>:8888/healthz` (200 healthy)

## 🔍 Example Prometheus scrape config

```yaml
scrape_configs:
  - job_name: 'swarm-scheduler-exporter'
    static_configs:
      - targets: [ 'swarm-manager:8888' ]
```

## 🔐 Security & Permissions

- Only needs **read-only** access to the Docker API (`/var/run/docker.sock:ro`).
- Must run on a **manager** node in Swarm to receive cluster-wide events and inspect services.
- Avoid exposing the exporter to untrusted networks; it exposes metrics only, but your scrape endpoint should be internal.

## 🛠 Addressed vs Original Project

- **Data races**: guarded metadata cache; removed global `nodeCount`; added worker pool; no per-event goroutines.
- **Event resiliency**: reconnect with capped backoff; bounded workers; fixed pointer-to-loop-var; per-worker panic recovery.
- **Series lifecycle**: `replicas_state` now `Reset()`s each publish; exhaustive zero emission per current service; delete series on service remove.
- **Global desired replicas accuracy**: evaluate **eligible nodes** (status/availability/constraints/platforms), not total nodes.
- **Label sanitation & validation**: full Prometheus regex, collision checks, max label keys, high-cardinality warning, raw→sanitized mapping.
- **Operability**: graceful shutdown; `/healthz`; health/build/exporter metrics; quieter default logs; validated `-poll-delay`.
- **Performance**: node snapshot cache; on node events recompute **only** global services; task poll optimized to “latest per slot”; worker pool.
- **Metrics namespace**: consistent `swarm_*` names & labels aligned with Prometheus best practices.
- **Service update visibility**: `swarm_service_update_state_info` + timestamps for rollbacks/paused/update flows.
- **SLO helpers**: `swarm_service_running_replicas` and `swarm_service_at_desired` for direct alerting/dashboards.

## 🤝 Contributing

Issues and PRs are welcome! Please run linters and keep changes modular:

- `pre-commit run`
- Keep labels/metrics backward-considerate unless the change is clearly an improvement

## 🙏 Acknowledgements

This project stands on the shoulders of **[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**.
Thank you for the original implementation and the inspiration to monitor Swarm task health with Prometheus.
