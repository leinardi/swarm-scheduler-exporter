# Task completion

Run the rows of the AGENTS.md validation matrix that match the edited paths; `make check` applies to
every row. Most common:

- `**/*.go`: `make verify-go`; after integration-tagged code (or what it exercises),
  `make go-test-integration`.
- `go.mod`/`go.sum`: `make go-tidy`, `make audit-deps`.
- Dockerfile/.hadolint/.dockerignore: `go test ./test/dockerfiles/ ./test/toolchain/`,
  `cargo test -p meta --test dockerfiles`, `make docker-build`.
- `.golangci.yaml`: `go test ./test/boundary/`, `make go-lint`.
- `.github/**`: `pre-commit run actionlint --all-files`, `go test ./test/toolchain/`,
  `cargo test -p meta --test toolchain --test dockerfiles`.
- `test/**`, `meta/**`: `SSE_REQUIRE_TOOLS=1 go test ./test/...`,
  `SWARM_SCHEDULER_EXPORTER_REQUIRE_TOOLS=1 cargo test -p meta`.
- `**/*.md`: `cargo test -p meta --test docs`; prose wraps at 100 columns.
- New file: MIT license header with the year of first commit (`scripts/check-license-headers.sh`).
- New files must be staged before `make check`: pre-commit skips untracked files.
