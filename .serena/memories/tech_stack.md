# Tech stack

- Go: one version line across the `go.mod` `go` directive and the Dockerfile's `dhi.io/golang`
  tag (`test/toolchain` enforces it); `CGO_ENABLED=0` static binaries; version strings via
  `-ldflags -X main.version/commit/date`.
- Libraries: moby/moby client v0.6.0 + api v1.56.0 (API 1.40–1.56, negotiated via `/_ping`),
  prometheus/client_golang (default registry, `promhttp.Handler()`), stdlib `log/slog`, `flag`,
  `net/http`.
- Lint: golangci-lint v2 (`.golangci.yaml`, all linters) via pre-commit; govulncheck for advisories.
- Build/infra: Makefile + `.mk/*.mk`; make-common snippets
  (`.mk/{docker,help,go,password,pre-commit}.mk`) are vendored from `leinardi/make-common` — never
  hand-edit, refresh with `make mk-common-update`.
- Image: `dhi.io` hardened bases (`dhi.io/golang` build stage, `dhi.io/static` runtime, non-root,
  `HEALTHCHECK` via `-healthcheck`), multi-arch amd64/arm64, `# syntax` frontend pinned by digest.
- Tests: DinD (`docker:29.8.1-dind`) Swarm harness; socket-proxy example
  (`tecnativa/docker-socket-proxy`, allowlist in `deployments/docker/socket-proxy.env`).
- Release: svu derives the version from Conventional Commits; Trivy scan, cosign keyless signing,
  provenance attestations (see `docs/release.md`).
