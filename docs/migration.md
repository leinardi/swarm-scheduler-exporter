# Migrating

This page covers the two jumps: **2.x to 3.0** (the Rust exporter moves onto shared, standard
libraries and stops reproducing Go's behaviour byte for byte) and, below it, **1.x (Go) to Rust**.
Either way every `swarm_*` metric keeps its name, type, help text, labels and values, `/metrics`
and `/healthz` keep their paths, and the Docker access stays read-only. The metric names, types
and label names are pinned by a committed inventory taken from the 1.3.2 exposition (minus the
`go_*` and `promhttp_*` families), and the values the Docker fixtures feed are checked by tests.

## Upgrading from 2.x to 3.0

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

## Coming from 1.x (Go)

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

## Addressed vs Original Project

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
  binary than the Go 1.x releases (see [Resource usage](../README.md#-resource-usage)), and a
  Docker client that can only send read requests.
