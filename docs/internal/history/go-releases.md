# Historical notes: the Go releases (1.x)

> **Status:** Historical: how the Go 1.x releases were built, up to `v1.3.2`. Current runbook:
> [release.md](../release.md).

Releases up to and including `v1.3.2` were built from the Go implementation, which the 2.0 cutover
replaced. What differed then, for anyone recovering or auditing one of them:

- The binaries were built by `go build` with `CGO_ENABLED=0`, `-trimpath` and `-buildvcs=false`,
  the version stamped through `-ldflags -X`, and the Go version taken from `go.mod`; the image's
  build stage was `dhi.io/golang`.
- The `test` job ran `make go-vet` and `go test -race ./...`, and the `audit` job ran govulncheck;
  CI's required `ci-result` also needed `repo-gates`, `go-test` and `go-vulncheck`.
- `svu` was installed with `go install`, and the Go module and build caches were restored by
  `actions/setup-go`.
- Dependabot's `gomod` updates were the dependency bumps committed as `fix(deps)`.

A recovery of a 1.x release runs on that release's commit, whose workflows still describe all of
this, so nothing in this section is needed to run one.
