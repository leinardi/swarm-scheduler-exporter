# Tech stack

- Rust: one line (1.98) across rust-toolchain.toml, the MSRV, the Dockerfile's `dhi.io/rust` tag
  and every CI toolchain step (`meta/tests/toolchain.rs`); static musl binaries for amd64 and
  arm64 via cargo-zigbuild and zig (pins shared by the Dockerfile and
  `.github/actions/setup-zigbuild`); version strings from `SWARM_SCHEDULER_EXPORTER_{VERSION,
  COMMIT,DATE}` at compile time.
- Libraries: tokio, hyper + hyper-util (healthcheck client), clap, jiff, serde. From
  leinardi/rust-common (git, pinned by the release tag `v0.3.0`, `version = "0.3"`):
  docker-engine (Docker client, rustls with ring), go-http (HTTP server), go-prom (registry, text
  encoder), go-slog (logger), go-compat (Go library ports), go-clock, service-kit, testenv-core,
  repo-meta.
- Lint and audit: clippy (workspace lints), rustfmt, cargo-machete, cargo-deny (`deny.toml`).
- Build/infra: Makefile + `.mk/*.mk`; make-common snippets (`.mk/{docker,help,password,
  pre-commit,rust}.mk`) are vendored from `leinardi/make-common`: never hand-edit, refresh with
  `make mk-common-update`.
- Image: `dhi.io` hardened bases (`dhi.io/rust` build stage, `dhi.io/static` runtime, non-root,
  `HEALTHCHECK` via `--healthcheck`), multi-arch amd64/arm64, `# syntax` frontend pinned by digest.
- Tests: DinD (`docker:29.8.1-dind`) Swarm harness in `testenv` (bollard behind the `integration`
  feature); socket-proxy example (`tecnativa/docker-socket-proxy`, allowlist in
  `deployments/docker/socket-proxy.env`).
- Release: svu (pinned release asset) derives the version from Conventional Commits; Trivy scan,
  cosign keyless signing, provenance attestations (see `docs/release.md`).
