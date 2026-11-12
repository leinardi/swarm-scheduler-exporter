# Swarm Scheduler Exporter

Prometheus exporter for Docker Swarm focused on **task state visibility**, **accurate desired
replicas**, and **operability at scale**. Fork of (and huge thanks to)
**[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**.

## 💡 What is “Swarm Scheduler Exporter”?

**Swarm Scheduler Exporter** surfaces what Docker Swarm’s **scheduler** is doing right now, and
*why*. It reports **desired replicas** (including accurate eligibility for `global` services),
**live task state** per service (current task per slot, or per node for `global` services),
**service update/rollback** state + timestamps, **cluster node** availability, and now
**SLO-friendly service readiness** signals.

## 📦 What This Exporter Does

- Watches Swarm **service** and **node** events to keep metrics fresh: events only mark what
  changed, and a single reconciler inspects it, backed by a full resync (service and node list) at
  startup, every 5 minutes, whenever it had to drop changes, and every time the event stream
  reconnects. A reconnect resumes the stream from just before the last event seen; the resync covers
  what the daemon no longer replays, such as the events of a restart. A stream that ends cleanly is
  logged at info, any other stream end at warn. A socket proxy cutting an idle stream (HAProxy does
  so after its client timeout, about 10 minutes, on a quiet cluster) ends it without the chunked
  body's terminating chunk, so that is logged at warn too, followed by the reconnect.
  Individual event JSON records are limited to 1 MiB, excluding leading whitespace and JSON-sequence
  record separators. An oversized record ends the stream at warn, followed by a reconnect and resync.
- Periodically polls **tasks** and aggregates **current** states per service (current task per slot
  for replicated services or per node for global services, exhaustive zero-emission).
- Computes **desired replicas** precisely for `global` services (eligible nodes only:
  status/availability/constraints/platforms).
- Emits **exporter** and **cluster** health metrics for alerting & SLOs.
- Sanitizes and validates **custom labels** for Prometheus compliance and safe cardinality.

## 🪶 Resource usage

Swarm Scheduler Exporter is designed to be lightweight. Measured side by side against 1.3.2 (the
last Go release) on one single-node Swarm with `--containers`, both exporting the same 964
`swarm_*` series:

|  | 1.x (Go) | 2.x (Rust) |
| --- | --- | --- |
| Binary, linux/amd64 | 11.2 MB | 4.4 MB |
| Binary, linux/arm64 | 10.5 MB | 3.9 MB |
| Image (linux/amd64) | 13.1 MB | 6.3 MB |
| Idle RSS after 10 minutes | 20.1-20.6 MB | 18.6-20.4 MB |

The idle RSS was read from `VmRSS` on a 32-CPU host. The Rust exporter starts one runtime worker
thread per available CPU, so a host with fewer CPUs, or a CPU limit, means fewer threads and less
memory: under `--cpus 2` the same exporter idled at 15.3 MB. Exact usage depends on the platform
and the container runtime settings.

## 📊 Metrics

All metrics live under the `swarm_` namespace.

### Service-level

Every service-level metric, including the update/rollback metrics below, carries these service
identity labels. All except `swarm_service_restart_policy` also carry custom labels:

- `stack` — the stack namespace (`com.docker.stack.namespace`), empty for a service not deployed as
  part of a stack
- `service` — the service name without the `<stack>_` prefix
- `service_mode` — `replicated`, `global`, `replicated-job` or `global-job`, as in the `MODE` column
  of `docker service ls`
- `display_name` — `stack service`, or just the stack (or service) name when the two are equal or
  the stack is empty
- one label per `--label` flag (see [Custom label guardrails](#custom-label-guardrails)), with the
  value of that service label, empty when the service does not set it

`...custom` below stands for those custom labels.

- `swarm_service_desired_replicas{stack,service,service_mode,display_name,...custom}` Desired
  replicas (**replicated**: configured replicas; **global**: number of eligible nodes, e.g. `6` on a
  healthy 6-node cluster with no constraints; **replicated-job**: the job's total completions;
  **global-job**: number of eligible nodes). A node is eligible when it is `ready` + `active` and
  meets the service's placement constraints and platforms. Placement-constraint evaluation follows
  Swarm: values are case-insensitive, a missing label compares as empty, and `node.ip` supports an
  IP or CIDR.

- `swarm_task_replicas_state{stack,service,service_mode,display_name,state,...custom}` Task count by
  state, counting the **current task per slot** for replicated services and replicated jobs or **per
  node** for global services and global jobs (always emits zeros for all known states per current
  service). Jobs count only the tasks of their **current iteration**: tasks left behind by an
  earlier run of the job are skipped.

- `swarm_service_running_replicas{stack,service,service_mode,display_name,...custom}` Number of
  **currently running** tasks per service (current task per slot or node, same snapshot as
  `replicas_state`). A service with no tasks at all (e.g. one never scheduled anywhere) still gets
  `0` here and in `replicas_state`, plus an `at_desired` series, so an alert on `at_desired == 0`
  also covers a service whose tasks were never created.

> ℹ️ **Current task:** the task Swarm still wants (desired state not `shutdown`, `remove`, …),
> preferring a running one, then the newest. A task Swarm has retired that has not reported stopping
> is not counted in any family: a down node keeps showing its tasks' last state (for example
> `running`) for up to 24h, and a live node takes a moment to report a task it is stopping. A
> retired task that did stop (for example `failed` after its restarts ran out) still counts under
> its state. This is why `running_replicas` can differ from the `REPLICAS` column of
> `docker service ls` during node outages and updates.
>
> ℹ️ **Consistency:** each poll counts tasks against one snapshot of the exporter's service and node
> caches, and a service's counts are published only if nothing about it changed while its tasks were
> listed: not its spec (replica count, mode, placement, job iteration), not a pending event for it,
> not the node list for a global service or global job, and no resync was requested. A rejected
> service keeps its previously published `replicas_state`, `running_replicas` and `at_desired`
> series for up to **2** polls in a row (only if its labels did not change); on the 3rd rejection in
> a row its series are dropped and the poll counts as failed, so health turns red instead of the
> exporter publishing stale numbers. Rejections are counted in
> `swarm_exporter_poll_rejections_total`.

- `swarm_service_at_desired{stack,service,service_mode,display_name,...custom}` `1` if
  `running_replicas == desired_replicas`, else `0`. Useful for dead-simple SLOs and alerting.
  **Jobs** are at desired once they have **finished**: every task of the current iteration that
  Swarm still wants is `complete`, and there are at least `desired_replicas` of them for a
  replicated job. For a global job, every node that is eligible **now** must have a completed task
  of the current iteration, so a node that becomes eligible after the job ran keeps it at `0` until
  its own task completes. While a job is still running, `at_desired` is `0`.

- `swarm_service_schedulable_replicas{stack,service,service_mode,display_name,...custom}` Number of
  replicas that can currently be scheduled given node availability and placement constraints.
  **Replicated**: `min(configured_replicas, eligible_nodes)` — drops to `0` when the only eligible
  node for a constrained service is offline. **Global**: equal to `desired_replicas` (eligible-node
  count). **One-shot / cronjob services** (`RestartPolicy.Condition == "none"`): always `0`, since
  the scheduler is not expected to keep tasks running for them. This silences false positives for
  swarm-cronjob–managed services that idle at `running=0` between runs. **Jobs** (`replicated-job`,
  `global-job`): always `0` too, since a job's tasks run to completion and exit. Architecture
  matching for `Placement.Platforms` normalizes kernel-style names (`x86_64`, `aarch64`, `armv7l`,
  `i686`, …) to Docker manifest names ( `amd64`, `arm64`, `arm`, `386`, …) on both sides of the
  comparison, so a manager that reports `x86_64` correctly matches a service requiring `amd64`.
  Prefer this over `desired_replicas` in alert expressions to suppress false positives when a pinned
  node is down:

  ```promql
  swarm_service_running_replicas < on(stack,service,service_mode,display_name) swarm_service_schedulable_replicas
  ```

- `swarm_service_restart_policy{stack,service,service_mode,display_name,restart_condition}`
  Exhaustive `0`/`1` gauges for `none`, `on-failure` and `any`. A missing policy or empty condition
  defaults to `any`. Unlike `schedulable_replicas=0`, the policy identifies intentionally one-shot
  services, but does not prove successful completion. This family carries **no custom labels**,
  so `restart_condition` remains usable as a custom label on existing families. Join using
  `on(stack,service,service_mode,display_name)`, for example:

  ```promql
  swarm_service_running_replicas and on(stack,service,service_mode,display_name) (swarm_service_restart_policy{restart_condition="none"} == 1)
  ```

> ℹ️ **Global services:** `desired_replicas` is the number of eligible nodes, not `0`. It is `0`
> only when no node is eligible: none is `ready` + `active` (a node that is down, disconnected,
> paused, or drained does not count), or none meets the placement constraints or platforms.
> `running_replicas` is then usually `0` too, and `at_desired=1` only when it is. A paused or
> disconnected node can still run an existing task, which gives
> `running_replicas > desired_replicas` ⇒ `at_desired=0`. Tasks Swarm has retired on a down node are
> not counted, even while the node still reports them running; until Swarm retires them (shortly
> after the node goes down) they still count.

### Service update/rollback (info-style)

- `swarm_service_update_state_info{stack,service,service_mode,display_name,...custom,state}` = `1`
  for the *current* state, else `0`. States: `updating`, `completed`, `paused`, `rollback_started`,
  `rollback_completed`.

- `swarm_service_update_started_timestamp_seconds{stack,service,service_mode,display_name,...custom}`
- `swarm_service_update_completed_timestamp_seconds{stack,service,service_mode,display_name,...custom}`

### Cluster / node visibility

- `swarm_cluster_nodes_by_state{role,availability,status}`
  Count of nodes by manager/worker, active/pause/drain, and ready/down/… .

### Node-level

- `swarm_node_info{node_id,node,role}` — identity, value `1`; role is the Docker wire text.
- `swarm_node_status{node_id,node,status}` — exhaustive `0`/`1` gauges for `ready`, `down`,
  `disconnected`, `unknown`.
- `swarm_node_availability{node_id,node,availability}` — exhaustive `0`/`1` gauges for `active`,
  `pause`, `drain`. An empty availability emits all zeros.
- `swarm_node_manager_reachability{node_id,node,reachability}` — managers only, exhaustive `0`/`1`
  gauges for `reachable`, `unreachable`, `unknown`.
- `swarm_node_manager_leader{node_id,node}` — managers only, `1` for the leader, otherwise `0`.
  A missing `Leader` is `0`; workers have no manager series.

`node` is `Description.Hostname`; `node_id` distinguishes nodes with duplicate hostnames (including
nodes that left and rejoined). All five families are published in one snapshot; removed nodes and
old role identities disappear. Node and manager status is normally refreshed about every minute;
node events refresh sooner, and failures can delay updates. The full resync remains five minutes.
Missing status or reachability maps to `unknown`. Unknown enum values invented by a newer daemon
fail the Docker call during decoding, rather than passing through as labels (see Q16).

### Exporter self-metrics

- `swarm_exporter_health` — `1` healthy / `0` unhealthy.
- `swarm_exporter_build_info{version,commit,date}` — `1`.
- `swarm_exporter_polls_total` / `swarm_exporter_poll_errors_total`.
- `swarm_exporter_poll_duration_seconds` (histogram).
- `swarm_exporter_events_reconnects_total`.
- `swarm_exporter_events_dropped_total` — Swarm events ignored because they carried no actor ID.
- `swarm_exporter_poll_rejections_total` — per-service poll counts not published because the service
  changed while its tasks were listed (see **Consistency** above).
- `swarm_exporter_resyncs_total{result}` — resyncs (service and node list) by `result`: `success` or
  `failure`. Both series exist at `0` from startup, so `increase()` and "never failed" rules work
  before the first resync. A resync interrupted by shutdown is not counted.
- `swarm_exporter_last_poll_success_timestamp_seconds` — Unix time of the latest successfully
  published task poll; `0` until the first one.
- `swarm_exporter_last_resync_success_timestamp_seconds` — Unix time of the latest completed resync;
  `0` until the first one.

For example:

```yaml
- alert: SwarmExporterResyncsFailing
  expr: increase(swarm_exporter_resyncs_total{result="failure"}[30m]) > 3
  labels:
    severity: warning
  annotations:
    summary: "Swarm exporter resyncs keep failing"

- alert: SwarmExporterPollsStale
  expr: time() - swarm_exporter_last_poll_success_timestamp_seconds > 120
  labels:
    severity: warning
  annotations:
    summary: "Swarm exporter has not published a task poll for over 2 minutes"
```

### Runtime metrics

Beside the `swarm_*` families, `/metrics` carries the process metrics the Prometheus Go client
exports by default, with the same names, types and help texts:

- `process_cpu_seconds_total`, `process_open_fds`, `process_max_fds`,
  `process_virtual_memory_bytes`, `process_virtual_memory_max_bytes`,
  `process_resident_memory_bytes`, `process_start_time_seconds`,
  `process_network_receive_bytes_total` and `process_network_transmit_bytes_total` (Linux).

There are no `go_*` metrics (the exporter has no Go runtime) and no `promhttp_metric_handler_*`
ones (the scrape handler is not promhttp).

The exposition is always OpenMetrics text
(`application/openmetrics-text; version=1.0.0; charset=utf-8`), uncompressed and ending with
`# EOF`, whatever the scraper's `Accept` and `Accept-Encoding`. Prometheus reads it without a
setting. In it a counter's family is named without `_total` (`# TYPE swarm_exporter_polls counter`),
and its sample carries it (`swarm_exporter_polls_total`): the metric names a query uses are the
same.

### Container-level (opt-in)

If you also run non-Swarm workloads (e.g. plain Docker Compose or standalone containers),
the exporter can expose **container state** metrics when started with `--containers`.

- `swarm_container_state{project,stack,service,container,orchestrator,display_name,state,exit_code}`
  Emits an **info-style one-hot series** per container across all known states.
  Exactly one time series per container has `1`, the rest are `0`.

  - `project` / `service` — from Compose labels (`com.docker.compose.*`), if present
  - `stack` — from Swarm labels (`com.docker.stack.namespace`), if present
  - `container` — the container's first name without its leading `/`, as Docker reports it (not
    sanitized)
  - `orchestrator` — `compose`, `swarm`, or `none`
  - `display_name` — friendly name (`stack service` or `stack` if identical)
  - `state` — one of `created`, `restarting`, `running`, `removing`, `paused`, `exited`, `dead`,
      `healthy`, `unhealthy`, `health_starting`
  - `exit_code` — string exit code (only when `state="exited"`, otherwise empty)

> ℹ️ The container list does not say which containers have a healthcheck, so the exporter inspects
> **every running container** (to read its health state, if any) and **every exited container** (to
> read its exit code). Inspects are capped at **300 per poll**, failed ones included, and share one
> **10s** deadline per poll; a container past the cap or the deadline keeps its listed state and an
> empty `exit_code`. Swarm task containers are skipped unless `--containers-include-swarm` is set.

## ✅ Health

The exporter is healthy once its first resync (service and node list) has completed, a task poll has
been published, and the latest published poll is no older than `max(3 × --poll-delay, 30s)` (30s
with the default 10s poll delay). A requested resync that stays outstanding for longer than that
same window, for example because listing services keeps failing, makes it unhealthy too.

- HTTP: `/healthz` responds `200` with body `ok` when healthy, and `503` with a short reason
  (`initial resync not completed`, `no successful poll yet`, `last poll too old` or
  `resync outstanding`) when not.
- Metric: `swarm_exporter_health` reports the same check as `1` healthy / `0` unhealthy, evaluated
  at every scrape.
- Container health: the image is distroless (no shell, no curl), so it ships a `HEALTHCHECK` that
  runs the binary itself with `--healthcheck`. That mode requests `/healthz` on `--listen-addr` (a
  wildcard host such as `0.0.0.0` or `::` is probed on loopback), never through a proxy and without
  following redirects, and exits `0` on a `200` and `1` otherwise, printing the status and the
  reason. The check allows a 90s start period for the first resync, then probes every 30s; after 3
  failures the container is unhealthy and Swarm replaces the task, which is the intended remedy for
  a stuck exporter.

The image's check uses the default `--listen-addr`. If you change it, override the check with the
same address:

```yaml
services:
  exporter:
    command: [ "--listen-addr", "0.0.0.0:9100" ]
    healthcheck:
      test: [ "CMD", "/usr/local/bin/swarm-scheduler-exporter", "--healthcheck", "--listen-addr", "0.0.0.0:9100" ]
```

To turn it off, set `healthcheck: disable: true` in compose or a stack file, or pass
`--no-healthcheck` to `docker run` or `docker service create`.

## 📋 Requirements

- Docker Engine **19.03+** (Engine API **1.40**) in Swarm mode; tested on Docker **29.x**.
- The exporter negotiates the API version with the daemon (`GET /version`). If
  `DOCKER_API_VERSION` is set, it must be within **1.40–1.56**; otherwise the exporter exits at
  startup with an error.

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
(the image sets an explicit non-root `USER 65532:65532`) can connect to the socket.

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
 --log-format text \
 --log-level warn
```

Replace `140` with the value from `stat -c %g /var/run/docker.sock`.

### 🐝 Swarm service (socket mount)

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
 --poll-delay 10s
```

> ℹ️ **Why the `manager` constraint?**
> Only manager nodes can access cluster-wide service, node, and event data required by the exporter.

### 🔒 Behind a socket proxy (recommended)

The exporter only reads, but whatever holds the Docker socket can do anything. Put
[tecnativa/docker-socket-proxy](https://github.com/Tecnativa/docker-socket-proxy) in front of it:
the exporter then reaches the Docker API over an internal overlay network, and the proxy refuses
every write and every API section the allowlist leaves off. A ready Swarm stack is in
[`deployments/docker/docker-compose.socket-proxy.yaml`](deployments/docker/docker-compose.socket-proxy.yaml),
with the allowlist in [`deployments/docker/socket-proxy.env`](deployments/docker/socket-proxy.env):

```bash
cd deployments/docker
docker stack deploy -c docker-compose.socket-proxy.yaml monitoring
```

The allowlist sets every ACL variable of the pinned proxy image explicitly (the image enables
`EVENTS` and `VERSION` by default) and opens only `VERSION`, `EVENTS`, `SERVICES`, `NODES`, `TASKS`
and `CONTAINERS`. `TestSocketProxy_ExampleAllowlist` runs the pinned image with that file against a
real Swarm, and checks that the exporter works through it and that what is left off is refused.

> ⚠️ **Scope:** this makes the Docker API **read-only** for the exporter, not least-privilege. The
> proxy matches each section by path prefix, so every enabled section allows every `GET` under it,
> and logs, specs and container files can hold secrets:
>
> - `SERVICES=1` and `TASKS=1`, which the exporter always needs, also allow service and task logs
>   (`/services/<id>/logs`, `/tasks/<id>/logs`: the containers' logs, from across the Swarm), and
>   the service list the exporter reads carries every service spec, environment variables included.
> - `CONTAINERS=1` allows every `GET` under `/containers`, including logs, stats, `top`, `archive`
>   and `export`. `CONTAINERS` is needed only with `--containers`: without that flag, set it to `0`.
>   That narrows the exposure, but log access stays.
>
> Exact per-endpoint rules would need a custom `haproxy.cfg`, which the example does not attempt.

Two proxy behaviours to know:

- HAProxy cuts an idle event stream after its client timeout (about 10 minutes; a quiet stream was
  held 660s). It ends the stream without the chunked body's terminating chunk, so the exporter logs
  `event stream ended; will reconnect` at warn, then reconnects (and resyncs). A server timeout, a
  proxy restart and a stopped daemon end the stream the same way.
- The task poll sends no filter in its URL, so a large cluster never runs into HAProxy's request
  size limit.

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
 --poll-delay 10s
```

This avoids group and permission issues, relying instead on proper TLS authentication.

### 🧩 Docker Compose Example

A complete Compose setup (replicated mode, manager constraint, and environment hints) is available
at: [`deployments/docker/docker-compose.yaml`](deployments/docker/docker-compose.yaml). The
socket-proxy stack is in
[`deployments/docker/docker-compose.socket-proxy.yaml`](deployments/docker/docker-compose.socket-proxy.yaml).

## ⚙️ Configuration

### Flags

```
Usage: swarm-scheduler-exporter [OPTIONS]

Options:
      --listen-addr <string>
          IP address and port to bind [default: 0.0.0.0:8888]
      --poll-delay <duration>
          How often to poll tasks (e.g. 10s, 1m, 1m30s). Minimum 1s. [default: 10s]
      --containers[=<BOOL>]
          Expose container state metrics (opt-in).
      --containers-include-swarm[=<BOOL>]
          Include containers belonging to Swarm tasks.
      --log-format <string>
          Either json or text [default: text] [possible values: text, json]
      --log-level <string>
          Either debug, info, warn, error [default: info] [possible values: debug, info, warn, error]
      --log-time[=<BOOL>]
          Include timestamp in logs
      --healthcheck[=<BOOL>]
          Probe this exporter's /healthz on --listen-addr and exit 0 if healthy, 1 otherwise
      --label <value>
          Name of custom service labels to add to metrics
  -h, --help
          Display help message
```

- Flags are long options with two dashes only. A single-dash spelling (`-poll-delay`) is refused
  with a pointer to the `--` one, and so is any positional argument (exit code `2`).
- A value goes after a space or `=` (`--poll-delay 30s`, `--poll-delay=30s`); a repeated flag keeps
  its last value, and `--label` may be repeated (an empty name is refused).
- A boolean flag alone means `true`; turning one off takes `=` (`--containers=false`; only `true` and
  `false` are accepted). `--containers false` is refused: `false` would be a positional
  argument.
- `--log-level` takes `debug`, `info`, `warn` (also `warning`) or `error`, and `--log-format` takes
  `json` or `text`, in any case; anything else is refused at startup (exit code `2`). Logs go to
  standard output, the timestamps (`--log-time`) are UTC RFC 3339, and a line carries its fields as
  `key=value` (text) or as JSON members (json).
- `--poll-delay` takes a duration (`500ms`, `10s`, `1m30s`, `1m 30s`) of at least `1s` (exit code
  `1` otherwise). A bool flag takes `true` or `false` (`--containers`, `--containers=true`).
- `-h` and `--help` print the help to standard output and exit `0`.

### Environment (Docker client)

- `DOCKER_HOST` — Docker daemon URL
- `DOCKER_CERT_PATH` — Path to TLS certs
- `DOCKER_TLS_VERIFY` — Enable TLS verification (set to `1`)
- `DOCKER_API_VERSION` — Pin the Engine API version instead of negotiating it (`<major>.<minor>`,
  within 1.40–1.56)

The client connects to `DOCKER_HOST` directly: `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` are not
read. To go through a proxy (a socket proxy, say), point `DOCKER_HOST` at it.

### Custom label guardrails

- Names are validated & **sanitized** to Prometheus label rules: ASCII `[a-zA-Z0-9_]` only, every
  other character (non-ASCII letters included) becomes `_`, and a name that would start with a digit
  gets a leading `_` (e.g., `app.kubernetes.io/name` → `app_kubernetes_io_name`, `équipe` →
  `_quipe`). Label values are kept as they are.
  > ⚠️ Earlier versions kept non-ASCII letters in label names. If a `--label` name has one, its
  > series label name changes (`tëam` is now `t_am`): update the queries, dashboards and alerts that
  > use it. A name that starts with two or more non-ASCII characters (`команда`, `团队`, `ÜÖteam`) now
  > sanitizes to a name starting with `__`, which is reserved: the exporter rejects it at startup,
  > and the `--label` must be renamed.
- Duplicate/colliding sanitized names are rejected at startup, and so is a sanitized name starting
  with `__` (reserved by Prometheus).
- At most **8** custom label keys; more are rejected at startup.
- A name that sanitizes to a label the exporter already uses (`stack`, `service`, `service_mode`,
  `display_name`, `state`) is rejected at startup.
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

- alert: SwarmNodeNotReady
  expr: swarm_node_status{status="ready"} == 0
  for: 2m
  labels:
    severity: warning
  annotations:
    summary: "Swarm node {{ $labels.node }} ({{ $labels.node_id }}) is not ready"
```

> ℹ️ **Jobs and `SwarmServiceNotAtDesired`:** a job reports `at_desired=0` until it has finished, so
> a job that runs longer than the rule's `for:` fires it. Exclude jobs with
> `swarm_service_at_desired{service_mode!~".*-job"} == 0`, or give them their own rule with a longer
> `for:`.

## 🧪 Quick checks

- **Metrics**: `curl http://<host>:8888/metrics`
- **Health**: `curl -s -o /dev/null -w "%{http_code}\n" http://<host>:8888/healthz` (`200` healthy,
  `503` unhealthy)

## 🔍 Example Prometheus scrape config

```yaml
scrape_configs:
  - job_name: 'swarm-scheduler-exporter'
    static_configs:
      - targets: ['swarm-manager:8888']
```

## 🔐 Security & Permissions

- The exporter only **issues read requests** to the Docker API (list, inspect and events). Mounting
  the socket with `:ro` does **not** make the API read-only: it only stops the container from
  modifying the socket file, and any client holding the socket can still create, update or remove
  resources. To enforce read-only access, put a socket proxy in front of the Docker API: see [Behind
  a socket proxy](#-behind-a-socket-proxy-recommended) for a tested example, and its scope note
  (read-only, not least-privilege).
- Must run on a **manager** node in Swarm to receive cluster-wide events and inspect services.
- Avoid exposing the exporter to untrusted networks; it exposes metrics only, but your scrape
  endpoint should be internal.

## 🛠 Addressed vs Original Project

- **Data races**: one reconciler task owns every cache and per-service metric write; removed
  the shared global node count; no per-event tasks.
- **Event resiliency**: reconnect with capped backoff and a resync on every reconnect; a bounded
  dirty set that falls back to a full resync; periodic resync.
- **Series lifecycle**: `replicas_state` is rebuilt and published as one snapshot per poll, so a
  scrape never sees it empty or partial; exhaustive zero emission per current service; delete series
  on service remove.
- **Global desired replicas accuracy**: evaluate **eligible nodes**
  (status/availability/constraints/platforms), not total nodes.
- **Label sanitation & validation**: full Prometheus regex, collision checks, max label keys,
  high-cardinality warning, raw→sanitized mapping.
- **Operability**: graceful shutdown; `/healthz`; health/build/exporter metrics; quieter default
  logs; validated `--poll-delay`.
- **Performance**: node snapshot cache; on node events recompute every service from cached
  placement, without inspecting it; task poll optimized to “current task per slot/node”; burst
  events coalesced per service.
- **Metrics namespace**: consistent `swarm_*` names & labels aligned with Prometheus best practices.
- **Service update visibility**: `swarm_service_update_state_info` + timestamps for
  rollbacks/paused/update flows.
- **SLO helpers**: `swarm_service_running_replicas` and `swarm_service_at_desired` for direct
  alerting/dashboards.
- **Footprint**: since 2.0 a static Rust binary on a distroless base, with a smaller image and
  binary than the Go 1.x releases (see [Resource usage](#-resource-usage)), and a Docker client
  that can only send read requests.

## 🔄 Migrating

This page covers the two jumps: **2.x to 3.0** (the Rust exporter moves onto shared, standard
libraries and stops reproducing Go's behaviour byte for byte) and, below it, **1.x (Go) to Rust**.
Either way every `swarm_*` metric keeps its name, type, help text, labels and values, `/metrics`
and `/healthz` keep their paths, and the Docker access stays read-only. The metric names, types
and label names are pinned by a committed inventory taken from the 1.3.2 exposition (minus the
`go_*` and `promhttp_*` families), and the values the Docker fixtures feed are checked by tests.

### Upgrading from 2.x to 3.0

Before you roll it out:

- **Scrape config and dashboards.** `/metrics` is OpenMetrics text only (`Content-Type:
  application/openmetrics-text; version=1.0.0; charset=utf-8`), never compressed, and ends with
  `# EOF`. Prometheus 2.5+ and 3.x negotiate it; a custom scraper that needs text 0.0.4 or protobuf
  does not work. A counter's *family* has no `_total` (its sample has), the `promhttp_*` families
  and `?name[]=` filtering are gone, and `process_*` is unchanged. Queries on sample names need no
  change.
- **Docker socket proxy.** The client negotiates the API version with `GET /version`, not
  `/_ping`: set `VERSION=1` (and `PING=0`, which is no longer used) in your proxy's allowlist, or
  pin `DOCKER_API_VERSION` (`<major>.<minor>`, within 1.40 to 1.56). Without either the exporter
  logs that the negotiation was refused and sends requests with no version prefix. The example in
  `deployments/docker/socket-proxy.env` is updated.
- **Log parsers.** Logs are `tracing` lines: text (`level`, `message`, then `key=value` fields) or
  JSON (`level`, `message`, the fields, `target`), timestamps in UTC (`TZ` has no effect), the error
  field is `error` (it was `err`), and durations print as `500ms`, `1m 30s`.
  `--log-format plain` and `--log-level fatal|panic` are refused (exit 2); `warning` is still an
  alias of `warn`.
- **Flag values.** A bool flag takes `true` or `false` only: `--containers=1`, `=t`, `=TRUE`, `=0`
  and the other `strconv.ParseBool` spellings are refused (exit 2). `--poll-delay` takes `10s`,
  `500ms`, `1m30s`, `1m 30s`, `1.5s` (and `2d`), no sign: `-5s` is a parse error (exit 2), not the
  exit-1 minimum check; the minimum is still `1s`.
- **Placement constraints** (`node.ip` with a CIDR such as `10.0.0.0/024` written with a
  leading-zero prefix, constraint keys and values with non-ASCII letters) read slightly differently,
  see Q17.
- **Docker states.** A task, node, container or event value the Docker API documents is read as
  before; one a *newer daemon invents* fails the call that carries it (logged by the collector),
  where 2.x passed it on as text.
- **HTTP edge cases.** Any other path is a bodyless 404, a method other than GET or HEAD is 405, and
  a listen error reads `http listen: listen tcp <address>: <system error>` (Q13). The listener is
  dual-stack on `:port`.

The rows that changed since 2.x are Q1, Q2, Q3, Q4, Q6, Q11, Q13, Q15, Q16 and Q17 in the table
below.

### Coming from 1.x (Go)

2.0 is the same exporter rewritten in Rust; the flags' names and defaults, the health rules and the
Docker access are unchanged, and the wire format is the one thing that moved (Q2).

What to change when upgrading from 1.x, besides the 3.0 list above:

- **Flags take two dashes.** `-poll-delay 10s` becomes `--poll-delay 10s`, `-containers` becomes
  `--containers`, and so on, in `command:`, `docker run` and `docker service create` arguments.
  1.x accepted both spellings; the exporter refuses the single-dash one at startup and names the
  `--` one.
- **Healthchecks you override** run `--healthcheck`:
  `test: [ "CMD", "/usr/local/bin/swarm-scheduler-exporter", "--healthcheck" ]`. The image's own
  `HEALTHCHECK` and the compose examples in `deployments/docker/` already do.
- **Dashboards or alerts on `go_*` metrics** have nothing to read: use `process_*` instead.
- **`DOCKER_CERT_PATH` without `DOCKER_TLS_VERIFY`** no longer connects without verifying the
  daemon's certificate: set `DOCKER_TLS_VERIFY=1`.
- **A TCP `DOCKER_HOST` reached through `HTTP_PROXY` or `HTTPS_PROXY`** is now dialled directly.
  If the daemon is reachable only through that proxy, point `DOCKER_HOST` at a proxy that forwards
  to it (a socket proxy). A unix socket was never proxied.

Every intended difference, 1.x (Go) against this release (the rows above that changed since 2.x
read as written here):

| # | Area | 1.x | 3.0 |
| --- | --- | --- | --- |
| Q1 | Runtime metrics | `go_*`, `process_*` and `promhttp_metric_handler_*` | No `go_*` and no `promhttp_metric_handler_*`; `process_*` keeps the same Linux families, names, types and help texts |
| Q2 | `/metrics` formats | Negotiated from `Accept`: protobuf (delimited, text, compact) or text 0.0.4; gzip by `Accept-Encoding` | Always OpenMetrics text (`application/openmetrics-text; version=1.0.0; charset=utf-8`), never compressed; a counter's family has no `_total` (its sample has); `?name[]=` filtering is gone |
| Q3 | CLI | Go's `flag`: `-x` and `--x` both work, trailing positional arguments are ignored (`-containers false` meant `true`), `-h` printed usage to standard error | `--x` only; a single-dash flag and any positional argument are refused (exit 2); `-h`/`--help` print to standard output and exit 0. Kept: names, defaults, last value wins, `--containers=false`, repeatable `--label`, exit 2 on parse errors. See Q17 for the value spellings |
| Q4 | Log flags | An unknown `-log-level` or `-log-format` silently fell back to `info` / `text` | Refused (exit 2), the message naming the accepted values; `plain` (format) and `fatal`, `panic` (level) are no longer accepted, `warning` stays an alias of `warn` |
| Q5 | Docker TLS | `DOCKER_CERT_PATH` with an empty `DOCKER_TLS_VERIFY` meant TLS without verifying the daemon | Startup fails (`docker client init failed`, exit 1) and asks for `DOCKER_TLS_VERIFY=1`; trust roots stay the system pool plus `ca.pem` |
| Q6 | Events connect | `event stream connected` was logged, and a reconnect's resync requested, even when the connect failed | Both happen only once the engine has answered the events request's headers with a 2xx, however quiet the stream is after: a refused request (non-2xx, no connection) or headers that do not arrive within 15s (the request timeout) is a failed connect and goes straight to the warn path |
| Q7 | Reconnect backoff | The 500ms backoff reset only after a stream that carried an event, so a quiet cluster behind a proxy backed off to 30s | It resets after any stream that connected |
| Q8 | Shutdown | 10s for the HTTP drain, then an unbounded wait for the workers | One 10s budget from the signal to exit, covering the drain and the workers |
| Q9 | Error texts | Library errors in Go's and the Docker SDK's wording | Library errors in Rust's and bollard's wording; every prefix and message the exporter writes itself is unchanged |
| Q10 | Panic recovery | `reconciler recovered from panic` with a Go stack trace | The same record, with the panic message and a Rust backtrace |
| Q11 | Docker wire | User-Agent `moby-client/…`; one version ping per concurrent first request | User-Agent `swarm-scheduler-exporter/<version>`; the version is negotiated once with `GET /version` (a refusal, as a socket proxy with `VERSION=0` gives, is logged and requests then carry no version prefix; set `VERSION=1` in the proxy or pin `DOCKER_API_VERSION`) |
| Q12 | Go runtime environment | `GOMAXPROCS`, `GOMEMLIMIT`, `GOGC` and `GODEBUG` tuned the process | No effect; the worker count follows the CPUs available to the container |
| Q13 | HTTP edge cases | net/http's answers to malformed requests, its header-count and size limits, the 307 for unclean paths, any method on the two routes, four server timeouts (read header 5s, read 10s, write 15s, idle 60s), and `net.Listen`'s wording and service-name ports (`:http`) | axum and hyper's: a bodyless 404 for any other path, 405 for a method other than GET and HEAD, hyper's answers and limits (431 above 100 headers), no redirect for unclean paths; a 5s header-read timeout that also closes an idle keep-alive connection, and a 15s write-stall timeout (no read or idle timeout); a listen error reads `http listen: listen tcp <address>: <system error>`, a port is a number, and a host name binds its first IPv4 address. Paths, status codes and bodies of `/metrics` and `/healthz` are unchanged |
| Q14 | Docker proxy variables | A TCP `DOCKER_HOST` went through `HTTP_PROXY` / `HTTPS_PROXY` (honouring `NO_PROXY` and skipping loopback), as Go's `http.ProxyFromEnvironment` decides | The client dials `DOCKER_HOST` directly and reads no proxy variable; unix sockets are unaffected |
| Q15 | Log output | slog text/JSON/plain lines, timestamps in the local zone (`TZ`) | `tracing` text or JSON lines (`level`, `message`, then the fields; JSON adds `target`), timestamps in UTC; the fields keep their names except `err`, now `error`, and durations print as Rust writes them (`500ms`, `1s`) |
| Q16 | Docker client | moby client v0.6.0 over its own HTTP/1 transport; every state a string; `DOCKER_API_VERSION` within 1.40–1.56 | bollard through `docker-client`: a state or type the daemon documents is decoded, one a newer daemon invents fails the call that carries it (a decode error, logged by the collector); requests carry the `/v<major>.<minor>` prefix of the pinned or negotiated version (none before negotiation, or after a refused one); the container list asks `all=true&size=false`; a pin must be `<major>.<minor>` within 1.40–1.56 |
| Q17 | Value spellings | Bool flags took every `strconv.ParseBool` spelling (`1`, `t`, `T`, `TRUE`, `True`, `0`, `f`, `F`, `FALSE`, `False`); `--poll-delay` took Go durations, a leading sign included; a `node.ip` CIDR took a prefix length with leading zeros (`10.0.0.0/024`); constraints and container labels folded case by Go's simple folding; custom-label errors quoted values with `%q` | Bool flags take `true` or `false` only (exit 2 otherwise); `--poll-delay` takes humantime durations (`10s`, `500ms`, `1m30s`, `1m 30s`, `1.5s`, `5µs`, and now `2d`), but no sign: `-5s` is a parse error (exit 2, where Go's reached the exit-1 minimum check), and a bare number needs a unit as before; a CIDR prefix has no leading zeros (the constraint never matches); constraint keys and values compare by Unicode lowercase, so U+017F (long s) no longer equals `s`; custom-label errors quote with `{:?}`; a poll delay prints as `10s`, `1h 2m`, `1s 500ms` |

## 🤝 Contributing

Issues and PRs are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for the hook setup and the
commit-message rules. Please run linters and keep changes modular:

- `pre-commit run`
- Keep labels/metrics backward-considerate unless the change is clearly an improvement

## 🙏 Acknowledgements

This project stands on the shoulders of
**[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**. Thank
you for the original implementation and the inspiration to monitor Swarm task health with
Prometheus.
