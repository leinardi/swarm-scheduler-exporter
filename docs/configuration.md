# Configuration

The exporter's command-line flags, the Docker client's environment variables and the limits on
custom labels.

## Flags

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

## Environment (Docker client)

- `DOCKER_HOST` — Docker daemon URL
- `DOCKER_CERT_PATH` — Path to TLS certs
- `DOCKER_TLS_VERIFY` — Enable TLS verification (set to `1`)
- `DOCKER_API_VERSION` — Pin the Engine API version instead of negotiating it (`<major>.<minor>`,
  within 1.40–1.56)

The client connects to `DOCKER_HOST` directly: `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` are not
read. To go through a proxy (a socket proxy, say), point `DOCKER_HOST` at it.

## Custom label guardrails

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
