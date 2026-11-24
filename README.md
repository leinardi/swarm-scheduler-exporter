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
  reconnects. A node-only refresh also runs about every minute to keep node and manager status
  fresh.
  Event stream details: [docs/operations.md](docs/operations.md#event-stream-and-refresh).
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

## 📋 Requirements

- Docker Engine **19.03+** (Engine API **1.40**) in Swarm mode; tested on Docker **29.x**.
- The exporter negotiates the API version with the daemon (`GET /version`). If
  `DOCKER_API_VERSION` is set, it must be within **1.40–1.56**; otherwise the exporter exits at
  startup with an error.

## 🚀 Quick Start

The recommended deployment is a Swarm stack with the exporter behind a socket proxy:

```bash
cd deployments/docker
docker stack deploy -c docker-compose.socket-proxy.yaml monitoring
```

This makes the Docker API read-only for the exporter, but not least-privilege: see the
[scope note](docs/deployment.md#behind-a-socket-proxy-recommended).

<a id="-finding-the-correct-socket-group"></a>
<a id="-docker-single-host"></a>
<a id="-swarm-service-socket-mount"></a>
<a id="-behind-a-socket-proxy-recommended"></a>
<a id="-alternative-tcptls-no-socket-mount"></a>
<a id="-docker-compose-example"></a>
For a single host, a socket mount, TCP/TLS, Compose or the socket group, see
[docs/deployment.md](docs/deployment.md).

## 📚 Documentation

- [Metrics](docs/metrics.md): every metric, its labels and its semantics.
- [Configuration](docs/configuration.md): flags, Docker client environment, custom label rules.
- [Deployment](docs/deployment.md): single host, Swarm, socket proxy, TCP/TLS and Compose.
- [Operations](docs/operations.md): health checks and the event stream's behaviour.
- [Example alerts](docs/alerts.md): Prometheus alert rules to start from.
- [Migrating](docs/migration.md): upgrading from 2.x or 1.x, and changes from the original project.
- [Releasing](docs/internal/release.md): how a release is cut and recovered.
- [Contributing](CONTRIBUTING.md): the hook setup and the commit-message rules.

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
  a socket proxy](docs/deployment.md#behind-a-socket-proxy-recommended) for a tested example,
  and its scope note (read-only, not least-privilege).
- Needs the Docker API of a **manager** node to receive cluster-wide events and inspect services:
  run it on a manager, or reach one through the socket proxy, as the recommended stack does.
- Avoid exposing the exporter to untrusted networks; it exposes metrics only, but your scrape
  endpoint should be internal.

<!-- Moved sections: these stubs keep the old README anchors working. -->

## 📊 Metrics

<a id="service-level"></a>
<a id="service-updaterollback-info-style"></a>
<a id="cluster--node-visibility"></a>
<a id="node-level"></a>
<a id="exporter-self-metrics"></a>
<a id="runtime-metrics"></a>
<a id="container-level-opt-in"></a>
Moved to [docs/metrics.md](docs/metrics.md).

## ✅ Health

Moved to [docs/operations.md](docs/operations.md).

## ⚙️ Configuration

<a id="flags"></a>
<a id="environment-docker-client"></a>
<a id="custom-label-guardrails"></a>
Moved to [docs/configuration.md](docs/configuration.md).

## 🔔 Example Alerts

Moved to [docs/alerts.md](docs/alerts.md).

## 🛠 Addressed vs Original Project

Moved to [docs/migration.md](docs/migration.md).

## 🔄 Migrating

<a id="upgrading-from-2x-to-30"></a>
<a id="coming-from-1x-go"></a>
Moved to [docs/migration.md](docs/migration.md).

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
