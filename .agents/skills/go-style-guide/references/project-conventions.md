# Project conventions — layout, flags, dependency injection, key dependencies

Back to [SKILL.md](../SKILL.md). Read this before adding a package, a file, a flag, a
dependency or a Docker SDK call.

## Contents

- Project layout, naming, license header
- Flags and configuration
- Dependency injection
- Key dependencies
- Docker client and SDK calls

---

## Project layout, naming, license header

```text
swarm-scheduler-exporter/
├── cmd/swarm-scheduler-exporter/   # wiring only: flags, Docker client, goroutines, server lifecycle, -healthcheck
│   └── version.go                  # version/commit/date, injected via -ldflags
├── internal/
│   ├── collector/                  # Prometheus collectors, the Reconciler and Swarm state logic
│   │   ├── metrics_ids.go          # namespace/subsystem and shared label constants
│   │   ├── docker_api.go           # DockerAPI: the only way into Docker
│   │   ├── types.go                # shared types, service and node caches (package doc lives here)
│   │   ├── snapshot_gauge.go       # snapshot collectors for families rebuilt as a whole
│   │   └── <family>.go             # one metric family (group) per file
│   ├── labels/                     # label sanitization and validation
│   ├── logger/                     # slog configuration, global accessor, plain handler
│   ├── server/                     # mux for /metrics and /healthz
│   └── testenv/                    # DinD Swarm harness (integration build tag)
├── test/integration/               # integration-tagged suite
├── deployments/docker/             # Dockerfile, compose files, socket-proxy allowlist
└── .mk/                            # Makefile snippets included by Makefile
```

- All application code lives under `internal/`. `cmd/` holds no business logic.
- Each `internal/` package has one responsibility; shared types of a package live in its
  `types.go`. A new metric family gets its own file in `internal/collector/`.
- Package names are lowercase single words (`collector`, `logger`, `server`, `labels`); file
  names are lowercase with underscores (`desired_replicas.go`, `plain_handler.go`).
- Every `.go` file starts with the MIT license block (`Copyright (c) 2025 Roberto Leinardi`),
  after the `//go:build` line where there is one, followed by the `package` line. A package doc
  comment (`// Package server owns …`) goes between them, once per package — `types.go` carries
  it for `collector`. A file may add a comment below `package` explaining its own
  responsibility.

---

## Flags and configuration

- stdlib `flag` only (no cobra/pflag), declared as package-level variables in `main.go`.
  Repeated flags implement `flag.Value` (`stringSlice`, returning `ErrEmptyFlagValue` for an
  empty value).
- Validate every flag right after `flag.Parse()`, before any resource is created. On invalid
  input print to stderr and return a non-zero exit code from `run()`.
- `version`, `commit`, `date` in `version.go` are `var`, not `const`, so `-ldflags -X` can set
  them (`make go-build` does).
- Every flag has a row in the README's flag table, and every documented flag exists.

---

## Dependency injection

Manual constructor injection; no framework. `cmd/` owns construction and passes dependencies down
as explicit parameters (context, Docker client, poll delay). Collector functions take the
`DockerAPI` interface, never `*client.Client`, so tests can pass `fakeDocker`; `main` passes the
real `*client.Client`, which satisfies it with no adapter. The logger is the one global
exception (see *Logging* in SKILL.md).

---

## Key dependencies

| Dependency | Purpose | Notes |
| --- | --- | --- |
| `log/slog` (stdlib) | Structured logging | Only logger allowed; logrus is banned by `depguard` |
| `flag` (stdlib) | CLI flags | No cobra/pflag |
| `sync`, `sync/atomic` (stdlib) | Concurrency | Preferred over external sync libraries |
| `github.com/moby/moby/client`, `github.com/moby/moby/api` | Docker Swarm API client and types | Only in `internal/collector` and `main` (`depguard` rule `docker-sdk-boundary`) |
| `github.com/containerd/errdefs` | Docker error classification | `errdefs.IsNotFound` (the client still maps status codes to these errors) |
| `github.com/prometheus/client_golang` | Metrics exposition | `prometheus.MustRegister` for every metric |

---

## Docker client and SDK calls

The Docker client is always built from the environment (`DOCKER_HOST`, `DOCKER_API_VERSION`,
`DOCKER_CERT_PATH`, `DOCKER_TLS_VERIFY`), so socket, TCP/TLS and API version are configured
externally. `newDockerClient` (`cmd/swarm-scheduler-exporter/docker_client.go`) applies the
parts of `client.FromEnv` itself onto a transport whose connection setup is bounded; its doc
comment explains why `client.FromEnv` and `client.WithTimeout` cannot be used directly. Build
the client only through it:

```go
dockerClient, newClientErr := newDockerClient()
if newClientErr != nil {
    loggerInstance.Error("docker client init failed", "err", newClientErr)

    return 1
}
defer dockerClient.Close()

versionErr := validateClientAPIVersion(dockerClient)
```

API version negotiation is lazy: the client pings the daemon before its first request and
refuses a daemon below `client.MinAPIVersion`. `validateClientAPIVersion` covers the other
path — a `DOCKER_API_VERSION` pin, which skips negotiation — by refusing a version outside
`client.MinAPIVersion`..`client.MaxAPIVersion` at startup.

Every SDK call takes an `…Options` struct and returns a `…Result` (`NodeList` → `.Items`,
`ServiceInspect` → `.Service`, `ContainerInspect` → `.Container`, `Events` → `.Messages` /
`.Err`). `client.Filters` is a map: leave it `nil` for no filter, and allocate it before adding
(`make(client.Filters).Add("type", "service", "node")`) — `Add` on a nil `Filters` panics.
