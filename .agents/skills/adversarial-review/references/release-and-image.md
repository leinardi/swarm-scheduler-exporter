# Image, release and CI invariants — swarm-scheduler-exporter

Back to [SKILL.md](../SKILL.md). These items belong to its §3 "Endpoint and image hardening".
Walk them when the diff touches `deployments/docker/**`, `.trivyignore.yaml`,
`.github/workflows/**`, `scripts/image-test.sh`, `.mk/dist.mk`, `.mk/image.mk` or `docs/release.md`.

## The image

`deployments/docker/Dockerfile`:

- Every base is pinned by tag, deliberately not by digest, since dhi.io rebuilds its tags too often
  to follow by digest: the build stage (`dhi.io/rust:…-debian13-dev`) and the shared `base` stage
  (`dhi.io/static:…`). The `# syntax=` frontend line is pinned by tag and index digest. A base on
  `latest` or with no tag, a digest pin on a dhi.io base, a frontend without a digest, a build-stage
  Rust release that differs from `rust-toolchain.toml` and the workflows' toolchain steps (patch
  included), or a zig, cargo-zigbuild or `rust-std` download without its sha256 check, is a finding.
- Two targets share the `base` stage, which sets the explicit `USER 65532:65532` (never root, so
  the uid is pinned in the Dockerfile rather than inherited from the base image's default), the
  port, the `--healthcheck` `HEALTHCHECK` and the entrypoint: `runtime`, the default, whose build
  stage cross-compiles the binary, and `prebuilt`, which CI and the release build and which must
  COPY the release asset from the `dist` build context unchanged, so the image binary stays
  byte-identical to it (`scripts/image-test.sh` checks the sha256, `meta/tests/dockerfiles.rs` the
  stages).

A diff must not make this worse: adding root, removing or changing the explicit `USER`, dropping
the static base, adding a shell or package manager to the base stage, or widening mounts and
capabilities in the compose files is a finding. Socket access is granted by adding the socket's
group (`--group-add`, `user: "65532:<gid>"`), not by running as root.

## Release and CI

`docs/release.md` is the contract; `.github/workflows/release.yaml` implements it. Tags here are
immutable, so the order of the release job is the safety property:

- The mode is decided from two fail-closed lookups before anything is pushed: the Git tag on the
  remote and `ghcr.io/…:<version>`. Only "not found" may read as absent; a lookup whose error is
  treated as absence is critical.
- The release runs no test suite of its own: `ci-green` (`require-ci-green`) gates on CI's
  `ci-result` and on `integration.yaml`'s `integration` for the push of the exact commit; CI's
  `image` job builds the release binaries with `make dist-static` and the `prebuilt` target from
  them, and runs the full image suite on amd64 and the reduced one on arm64 (`docs/release.md`,
  "What CI proves"). A change that weakens CI's `image` or `rust` job, or the integration suite,
  or lets one skip on a push, weakens the release and is a finding.
- The `audit` job (cargo-deny) runs alongside the release job's build; `require-job-success`
  waits for it before the first step that publishes. A publishing step moved above that wait, or
  the wait made conditional, is a finding.
- The image is built once, from the binaries the release built (or the published ones
  `release-mode` verified), into the job-local registry, smoke-tested, and only the digest that was
  scanned is copied to GHCR (`skopeo copy --all --preserve-digests`), checked by hashing what GHCR serves,
  attested and signed, and only then tagged `:<version>`. A second image build, a scan of
  anything but the published digest, a copy that can change the digest (the build forces gzip
  layers for this reason), or a version tag added before attest and sign is a finding.
- A reused image is verified by attestation and signature, never by comparing a rebuilt index.
- The scan is `trivy-gate`, at the latest Trivy (never pinned), failing on fixable `HIGH` and
  `CRITICAL` findings (`--ignore-unfixed` by design). Exceptions live only in `.trivyignore.yaml`,
  each scoped to paths or purls, with a `statement` and an `expired_at`; a lowered severity, a
  Trivy pin or an exception anywhere else is a finding.
- Workflow tokens are `contents: read` at the top of every workflow, and a job asks for more only
  with a comment saying why. Every action is pinned to a full commit SHA and every image a
  workflow runs to an index digest; a new floating `@v…` or `:tag` is a finding.
