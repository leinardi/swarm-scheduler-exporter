# Example Alerts

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
