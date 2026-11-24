# Getting started

Deploy the exporter on a Docker Swarm behind a socket proxy, check that it is healthy and point
Prometheus at it.

## Requirements

- Docker Engine 19.03 or later (Engine API 1.40) in Swarm mode.
- A shell on a manager node.
- IPsec ESP (IP protocol 50) allowed between the nodes: the stack's overlay network is encrypted.

## Deploy the stack

The stack runs the exporter and a socket proxy that holds the Docker socket on a manager node:

```bash
git clone https://github.com/leinardi/swarm-scheduler-exporter.git
cd swarm-scheduler-exporter/deployments/docker
docker stack deploy -c docker-compose.socket-proxy.yaml monitoring
```

The proxy makes the Docker API read-only for the exporter, but not least-privilege: read the
[scope note](deployment.md#behind-a-socket-proxy-recommended) before you use it on a cluster whose
logs or service specs hold secrets.

The stack starts the exporter with `--containers`. If you do not need container metrics, remove
that flag from `docker-compose.socket-proxy.yaml` and set `CONTAINERS=0` in `socket-proxy.env`.

## Check that it works

The exporter publishes port 8888. It reports healthy once its first resync and task poll are done:

```bash
curl -s -o /dev/null -w "%{http_code}\n" http://<host>:8888/healthz
curl -s http://<host>:8888/metrics | grep '^swarm_service_desired_replicas'
```

`/healthz` answers `200` when healthy and `503` with a short reason when not. See
[Health](operations.md#health) for the rules.

## Scrape it with Prometheus

```yaml
scrape_configs:
  - job_name: 'swarm-scheduler-exporter'
    static_configs:
      - targets: ['swarm-manager:8888']
```

Keep the endpoint on an internal network: `/metrics` and `/healthz` have no authentication.

## Next steps

- [Deployment](deployment.md): a single host, a socket mount, TCP/TLS and Compose.
- [Configuration](configuration.md): flags, Docker client environment and custom labels.
- [Metrics](metrics.md): every metric, its labels and its semantics.
- [Example alerts](alerts.md): Prometheus rules to start from.
