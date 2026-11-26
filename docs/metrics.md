# Metrics

All metrics live under the `swarm_` namespace.

## Service-level

Every service-level metric, including the update/rollback metrics below, carries these service
identity labels. All except `swarm_service_restart_policy` also carry custom labels:

- `stack` — the stack namespace (`com.docker.stack.namespace`), empty for a service not deployed as
  part of a stack
- `service` — the service name without the `<stack>_` prefix
- `service_mode` — `replicated`, `global`, `replicated-job` or `global-job`, as in the `MODE` column
  of `docker service ls`
- `display_name` — `stack service`, or just the stack (or service) name when the two are equal or
  the stack is empty
- one label per `--label` flag (see
  [Custom label guardrails](configuration.md#custom-label-guardrails)), with the
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

## Service update/rollback (info-style)

- `swarm_service_update_state_info{stack,service,service_mode,display_name,...custom,state}` = `1`
  for the *current* state, else `0`. States: `updating`, `completed`, `paused`, `rollback_started`,
  `rollback_completed`.

- `swarm_service_update_started_timestamp_seconds{stack,service,service_mode,display_name,...custom}`
- `swarm_service_update_completed_timestamp_seconds{stack,service,service_mode,display_name,...custom}`

## Cluster / node visibility

- `swarm_cluster_nodes_by_state{role,availability,status}`
  Count of nodes by manager/worker, active/pause/drain, and ready/down/… .

## Node-level

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
node events refresh sooner, and failures can delay updates. The full resync still runs every 5 minutes.
Missing status or reachability maps to `unknown`. Unknown enum values invented by a newer daemon
fail the Docker call during decoding, rather than passing through as labels
(see [Q16](migration.md#coming-from-1x-go)).

## Exporter self-metrics

- `swarm_exporter_health` — `1` healthy / `0` unhealthy.
- `swarm_exporter_build_info{version,commit,date}` — `1`.
- `swarm_exporter_swarm_manager` — `1` when the Docker daemon the exporter reads is a Swarm
  manager, `0` when it is not (a worker, or Swarm inactive) or not yet known (before the first
  resync). A daemon that is not a manager exports no `swarm_service_*`, `swarm_task_*`,
  `swarm_node_*` or `swarm_cluster_*` series (see [Health](operations.md#health)).
- `swarm_exporter_polls_total` / `swarm_exporter_poll_errors_total`.
- `swarm_exporter_poll_duration_seconds` (histogram).
- `swarm_exporter_events_reconnects_total`.
- `swarm_exporter_events_dropped_total` — Swarm events ignored because they carried no actor ID.
- `swarm_exporter_poll_rejections_total` — per-service poll counts not published because the service
  changed while its tasks were listed (see **Consistency** above).
- `swarm_exporter_resyncs_total{result}` — resyncs (service and node list) by `result`: `success` or
  `failure`. Both series exist at `0` from startup, so `increase()` and "never failed" rules work
  before the first resync. A resync interrupted by shutdown is not counted, and neither is one a
  daemon that is not a Swarm manager refuses (see [Health](operations.md#health)).
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
  expr: |
    (time() - swarm_exporter_last_poll_success_timestamp_seconds > 120)
    and on (job, instance) (swarm_exporter_swarm_manager == 1)
  labels:
    severity: warning
  annotations:
    summary: "Swarm exporter has not published a task poll for over 2 minutes"
```

`SwarmExporterPollsStale` is limited to managers: an exporter on a worker never polls tasks, so
its poll timestamp stays `0`. `SwarmExporterResyncsFailing` needs no such limit, because a daemon
that is not a manager never counts a failed resync; limiting it would hide a manager whose every
resync fails, whose role then stays unknown (`0`).

## Runtime metrics

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

## Container-level (opt-in)

If you also run non-Swarm workloads (e.g. plain Docker Compose or standalone containers),
the exporter can expose **container state** metrics when started with `--containers`. They work
on any Docker host: a Swarm manager, a worker, or a host with Swarm inactive, where they are the
only metrics exported besides the exporter's own.

- `swarm_container_state{project,stack,service,container,orchestrator,display_name,state,exit_code}`
  Emits an **info-style one-hot series** per container across all known states.
  Exactly one time series per container has `1`, the rest are `0`.

  - `project` / `service` — from Compose labels (`com.docker.compose.*`), if present; without a
    Compose service label, `service` is the Swarm service name without its `<stack>_` prefix
  - `stack` — from Swarm labels (`com.docker.stack.namespace`), if present
  - `container` — the container's first name without its leading `/`, as Docker reports it (not
    sanitized)
  - `orchestrator` — `compose`, `swarm`, or `none`
  - `display_name` — friendly name: `<group> <service>`, or `<group>` if the two are identical,
    where the group is the Compose project, else the stack; the container name when there is no
    group or no service
  - `state` — one of `created`, `restarting`, `running`, `removing`, `paused`, `exited`, `dead`,
      `healthy`, `unhealthy`, `health_starting`
  - `exit_code` — string exit code of an exited container, carried by all of that container's
    `state` series (the `0` ones too); empty for any other container

> ℹ️ The container list does not say which containers have a healthcheck, so the exporter inspects
> **every running container** (to read its health state, if any) and **every exited container** (to
> read its exit code). Inspects are capped at **300 per poll**, failed ones included, and share one
> **10s** deadline per poll; a container past the cap or the deadline keeps its listed state and an
> empty `exit_code`. Swarm task containers are skipped unless `--containers-include-swarm` is set.
