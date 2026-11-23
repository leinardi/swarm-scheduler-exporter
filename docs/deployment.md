# Deployment

When running the exporter inside a container, it needs permission to talk to the Docker Engine.
On most systems, this means allowing access to the Docker UNIX socket at `/var/run/docker.sock`.

## Finding the correct socket group

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

## Docker (single host)

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

## Swarm service (socket mount)

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

## Behind a socket proxy (recommended)

The exporter only reads, but whatever holds the Docker socket can do anything. Put
[tecnativa/docker-socket-proxy](https://github.com/Tecnativa/docker-socket-proxy) in front of it:
the exporter then reaches the Docker API over an internal overlay network, and the proxy refuses
every write and every API section the allowlist leaves off. A ready Swarm stack is in
[`deployments/docker/docker-compose.socket-proxy.yaml`](../deployments/docker/docker-compose.socket-proxy.yaml),
with the allowlist in [`deployments/docker/socket-proxy.env`](../deployments/docker/socket-proxy.env):

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

## Alternative: TCP/TLS (no socket mount)

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

## Docker Compose Example

A complete Compose setup (replicated mode, manager constraint, and environment hints) is available
at: [`deployments/docker/docker-compose.yaml`](../deployments/docker/docker-compose.yaml). The
socket-proxy stack is in
[`deployments/docker/docker-compose.socket-proxy.yaml`](../deployments/docker/docker-compose.socket-proxy.yaml).
