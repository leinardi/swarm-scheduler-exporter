# Operations

How the exporter reports its health and how it follows the Docker event stream.

## Health

The exporter is healthy once its first resync (service and node list) has completed, a task poll has
been published, and the latest published poll is no older than `max(3 × --poll-delay, 30s)` (30s
with the default 10s poll delay). A requested resync that stays outstanding for longer than that
same window, for example because listing services keeps failing, makes it unhealthy too.

On a Docker daemon that is not a Swarm manager (a worker, or a host with Swarm inactive), Docker
refuses the service, node and task lists with `503 This node is not a swarm manager`. The
exporter reads that as a state, not as a failing resync: it logs one warning, exports no Swarm
series, and retries the service list on the resync backoff (up to 30s), so `docker swarm init`
or a promotion is picked up without a restart. Its health then depends on `--containers`:

- without it there is nothing to export, and the exporter is unhealthy with the reason
  `not a swarm manager`;
- with it, the exporter is healthy once a container poll has been published and the latest one is
  no older than the same `max(3 × --poll-delay, 30s)` window, so a global service on every node
  stays healthy on its workers.

A manager that is demoted drops every Swarm series and stops every manager-only call except that
retried service list, until it is a manager again.

- HTTP: `/healthz` responds `200` with body `ok` when healthy, and `503` with a short reason
  (`initial resync not completed`, `no successful poll yet`, `last poll too old`,
  `resync outstanding` or `not a swarm manager`) when not.
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

## Event stream and refresh

  A reconnect resumes the stream from just before the last event seen; the resync covers
  what the daemon no longer replays, such as the events of a restart. A stream that ends cleanly is
  logged at info, any other stream end at warn. A socket proxy cutting an idle stream (HAProxy does
  so after its client timeout, about 10 minutes, on a quiet cluster) ends it without the chunked
  body's terminating chunk, so that is logged at warn too, followed by the reconnect.
  An event record may hold up to 1 MiB, every byte before its newline counted, leading whitespace
  and JSON-sequence record separators included. A longer record ends the stream at warn, and the
  reconnect then subscribes to live events only, with no `since` (logged as `next_since=none`), so
  the record is skipped rather than replayed; the reconnect's resync covers the skipped window, and
  the next event seen restores the usual resume point.
