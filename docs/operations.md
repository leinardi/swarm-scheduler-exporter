# Operations

How the exporter reports its health and how it follows the Docker event stream.

## Health

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

## Event stream and refresh

  A reconnect resumes the stream from just before the last event seen; the resync covers
  what the daemon no longer replays, such as the events of a restart. A stream that ends cleanly is
  logged at info, any other stream end at warn. A socket proxy cutting an idle stream (HAProxy does
  so after its client timeout, about 10 minutes, on a quiet cluster) ends it without the chunked
  body's terminating chunk, so that is logged at warn too, followed by the reconnect.
  Individual event JSON records are limited to 1 MiB, excluding leading whitespace and JSON-sequence
  record separators. An oversized record ends the stream at warn, followed by a reconnect and resync.
