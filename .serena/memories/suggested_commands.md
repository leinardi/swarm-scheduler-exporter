# Suggested commands

- `make help` — lists every target (project targets live in `.mk/*.mk`, never as Makefile recipes).
- `make go-build`, `make go-test` (`-race`), `make go-vet`, `make go-lint`; `make verify-go` runs
  build, test, vet, integration vet and lint.
- Single test: `go test ./internal/collector -run TestReconciler -v`.
- Meta-tests: `go test ./test/...`; `SSE_REQUIRE_TOOLS=1 go test ./test/...` turns a missing
  golangci-lint into a failure.
- Integration (Docker, privileged): `make go-test-integration [RUN='^TestNode_']`, golden rewrite
  `make go-test-integration UPDATE=1 RUN=TestSnapshot_`, cleanup `make sweep-test-leaks`.
- `make check` (pre-commit, all files; the CI gate) / `make check-stage` (staged only).
- `make audit-deps` (govulncheck, network), `make go-tidy`.
- `make docker-build` / `make docker-run` (dhi.io login required).
- Suppression baseline refresh: `scripts/check-suppressions.sh --update`.
- Workflow lint: `pre-commit run actionlint --all-files`.
