# Swarm Scheduler Exporter

Prometheus exporter for Docker Swarm focused on **task state visibility**, **accurate desired replicas**, and **operability at scale**.
Fork of (and huge thanks to) **[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**.

---

## 💡 What is “Swarm Scheduler Exporter”?

**Swarm Scheduler Exporter** surfaces what Docker Swarm’s **scheduler** is doing right now, and *why*. It reports desired replicas (including accurate
eligibility for `global` services), live task state per service (latest per slot), service update/rollback state and timestamps, and cluster node
availability/health.

---

## 📦 What This Exporter Does

* Watches Swarm **service** and **node** events to keep metrics fresh (resilient reconnect, bounded worker pool).
* Periodically polls **tasks** and aggregates **current** states per service (latest per slot, exhaustive zero-emission).
* Computes **desired replicas** precisely for `global` services (eligible nodes only: status/availability/constraints/platforms).
* Emits exporter and cluster **health** metrics for alerting & SLOs.
* Sanitizes and validates **custom labels** for Prometheus compliance and safe cardinality.

---

## 📊 Metrics

All metrics live under the `swarm_` namespace.

### Service-level

* `swarm_service_desired_replicas{stack,service,service_mode,...custom}`
  Desired replicas (replicated: configured replicas; global: eligible nodes).

* `swarm_task_replicas_state{stack,service,service_mode,state,...custom}`
  **Latest-per-slot** task count by state (always emits zeros for all known states per current service).

### Service update/rollback (info-style)

* `swarm_service_update_state_info{stack,service,service_mode,state}` = `1` for the *current* state, else `0`.
  States: `updating`, `completed`, `paused`, `rollback_started`, `rollback_completed`.

* `swarm_service_update_started_timestamp_seconds{...}`

* `swarm_service_update_completed_timestamp_seconds{...}`

### Cluster / node visibility

* `swarm_cluster_nodes_by_state{role,availability,status}`
  Count of nodes by manager/worker, active/pause/drain, and ready/down/… .

### Exporter self-metrics

* `swarm_exporter_health` — `1` healthy / `0` unhealthy.
* `swarm_exporter_build_info{version,commit,date}` — `1`.
* `swarm_exporter_polls_total` / `swarm_exporter_poll_errors_total`.
* `swarm_exporter_poll_duration_seconds` (histogram).
* `swarm_exporter_events_reconnects_total`.

---

## ✅ Health

* HTTP: `/healthz` responds `200` when the exporter considers itself healthy.
* Metric: `swarm_exporter_health` mirrors health for scraping/alerting.

---

## 🚀 Quick Start

### Docker (single host)

```bash
docker run --rm \
  -p 8888:8888 \
  -v /var/run/docker.sock:/var/run/docker.sock:ro \
  ghcr.io/leinardi/swarm-scheduler-exporter:latest \
  -log-format text \
  -log-level warn
```

### Swarm service (recommended)

```bash
docker service create \
  --name swarm-scheduler-exporter \
  --mode replicated --replicas 1 \
  --constraint 'node.role == manager' \
  --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock,ro \
  --publish published=8888,target=8888 \
  ghcr.io/leinardi/swarm-scheduler-exporter:latest \
  -log-format text \
  -log-level warn \
  -poll-delay 10s
```

> ℹ️ **Why manager constraint?** Access to cluster-wide events and service/node inspection requires a manager.

---

## ⚙️ Configuration

### Flags

* `-listen-addr <ip:port>` — HTTP listen address. *(default `0.0.0.0:8888`)*
* `-poll-delay <duration>` — How often to poll tasks, e.g. `10s`, `1m`. *(min `1s`, default `10s`)*
* `-label <key>` — Add a **service label key** as a metric label. Repeatable.
  Example: `-label app.kubernetes.io/name -label team.name`
* `-log-format <text|json|plain>` — Log format. *(default `plain`)*
* `-log-level <debug|info|warn|error>` — Minimum log level. *(default `warn`)*

### Environment (Docker client)

* `DOCKER_HOST` — Docker daemon URL.
* `DOCKER_CERT_PATH` — TLS certs path.
* `DOCKER_TLS_VERIFY` — Enable TLS verification (set to `1`).

### Custom label guardrails

* Names are validated & **sanitized** to Prometheus label rules
  (e.g., `app.kubernetes.io/name` → `app_kubernetes_io_name`).
* Duplicate/colliding sanitized names are rejected at startup.
* Max number of custom label keys is bounded (sane default).
* Suspicious **high-cardinality values** log a one-time warning.

---

## 🧪 Quick checks

* **Metrics**: `curl http://<host>:8888/metrics`
* **Health**: `curl -s -o /dev/null -w "%{http_code}\n" http://<host>:8888/healthz` (200 healthy)

---

## 🔍 Example Prometheus scrape config

```yaml
scrape_configs:
  - job_name: 'swarm-scheduler-exporter'
    static_configs:
      - targets: [ 'swarm-manager:8888' ]
```

---

## 🔐 Security & Permissions

* Only needs **read-only** access to the Docker API (`/var/run/docker.sock:ro`).
* Must run on a **manager** node in Swarm to receive cluster-wide events and inspect services.
* Avoid exposing the exporter to untrusted networks; it exposes metrics only, but your scrape endpoint should be internal.

---

## 🛠 Addressed vs Original Project

* **Data races**: guarded metadata cache; removed global `nodeCount`; added worker pool; no per-event goroutines.
* **Event resiliency**: reconnect with capped backoff; bounded workers; fixed pointer-to-loop-var; per-worker panic recovery.
* **Series lifecycle**: replicas_state now **Reset()**s each publish; exhaustive zero emission per current service; delete series on service remove.
* **Global desired replicas accuracy**: evaluate **eligible nodes** (status/availability/constraints/platforms), not total nodes.
* **Label sanitation & validation**: full Prometheus regex, collision checks, max label keys, high-cardinality warning, raw→sanitized mapping.
* **Operability**: graceful shutdown; `/healthz`; health/build/exporter metrics; quieter default logs; clearer/validated `-poll-delay`.
* **Performance**: node snapshot cache; on node events recompute **only** global services; task poll optimized to “latest per slot”; worker pool.
* **Metrics namespace**: moved to consistent `swarm_*` names & labels aligned with Prometheus best practices.
* **Service update visibility**: added `swarm_service_update_state_info` and update timestamps for rollbacks/paused/update flows.

---

## 🤝 Contributing

Issues and PRs are welcome! Please run linters and keep changes modular:

* Use `pre-commit run`
* Keep labels/metrics backward-considerate unless the change is clearly an improvement

---

## 🙏 Acknowledgements

This project stands on the shoulders of **[akerouanton/swarm-tasks-exporter](https://github.com/akerouanton/swarm-tasks-exporter)**.
Thank you for the original implementation and the inspiration to monitor Swarm task health with Prometheus.
