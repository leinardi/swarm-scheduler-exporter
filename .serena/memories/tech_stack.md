# Tech stack

- Rust: one line (1.98) across rust-toolchain.toml, the MSRV, the Dockerfile's `dhi.io/rust` tag
  (`meta/tests/toolchain.rs`); CI installs rust-toolchain.toml with
  `rustup toolchain install`; static musl binaries for amd64 and
  arm64 via cargo-zigbuild and zig (`make dist-static`; pins shared by the Dockerfile and the
  workflows' gh-composite-actions `setup-zigbuild` steps); version strings from
  `SWARM_SCHEDULER_EXPORTER_{VERSION,COMMIT,DATE}` at compile time.
- Libraries: humantime (durations), ipnet (CIDR), tokio, hyper + hyper-util (healthcheck client),
  axum (routes), clap, jiff, serde, prometheus-client (registry and OpenMetrics encoder). From
  leinardi/rust-common (git, pinned by release tag): docker-client (Docker connection over bollard,
  rustls with ring), tracing-setup (logger subscriber),
  clock, service-kit (with
  `runtime`, `process-metrics` and `http`), testenv-core, repo-meta.
- Lint and audit: clippy (workspace lints), rustfmt, cargo-machete, cargo-deny (`deny.toml`).
- Build/infra: Makefile + `.mk/*.mk`; make-common snippets (`.mk/{docker,help,password,
  pre-commit,rust}.mk`) are vendored from `leinardi/make-common`: never hand-edit, refresh with
  `make mk-common-update`.
- Image: `dhi.io` hardened bases pinned by tag, deliberately not by digest (dhi.io rebuilds too
  often): `dhi.io/rust` build stage, `dhi.io/static` base (non-root, `HEALTHCHECK` via
  `--healthcheck`). Multi-arch amd64/arm64, `# syntax` frontend pinned by digest. Two targets:
  `runtime` (default, compiles in Docker) and `prebuilt` (COPYs
  `dist/swarm-scheduler-exporter-linux-<arch>` from `make dist-static`), which CI and the release
  build; `scripts/image-test.sh` tests it in three tiers. Pulling needs a `dhi.io` login.
- Tests: DinD (`docker:29.8.1-dind`) Swarm harness in `testenv` (bollard behind the `integration`
  feature); socket-proxy example (`tecnativa/docker-socket-proxy`, allowlist in
  `deployments/docker/socket-proxy.env`).
- Release: gh-composite-actions (release-version with svu, require-ci-green, release-mode,
  trivy-gate at the latest Trivy); cosign keyless signing + provenance attestations (see
  `docs/release.md`).
