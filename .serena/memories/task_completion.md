# Task completion

Run the rows of the AGENTS.md validation matrix that match the edited paths; `make check` applies to
every row. Most common:

- `**/*.go`: `make verify-go`; after integration-tagged code (or what it exercises),
  `make go-test-integration`.
- `go.mod`/`go.sum`: `make go-tidy`, `make audit-deps`.
- Dockerfile/.hadolint/.dockerignore: `go test ./test/dockerfiles/ ./test/toolchain/`,
  `make docker-build`.
- `.golangci.yaml`: `go test ./test/boundary/`, `make go-lint`.
- `.github/**`: `pre-commit run actionlint --all-files`, `go test ./test/toolchain/`.
- `test/**`: `SSE_REQUIRE_TOOLS=1 go test ./test/...`.
- `**/*.md`: `go test -count=1 ./test/docs/`; prose wraps at 100 columns.
- New file: MIT license header with the year of first commit (`scripts/check-license-headers.sh`).
- New files must be staged before `make check`: pre-commit skips untracked files.
