# Image, release and CI invariants — swarm-scheduler-exporter

Back to [SKILL.md](../SKILL.md). These items belong to its §3 "Endpoint and image hardening".
Walk them when the diff touches `deployments/docker/**`, `.trivyignore`, `.github/workflows/**`
or `docs/release.md`.

## The image

`deployments/docker/Dockerfile`:

- Every base is pinned by tag, not by digest, since dhi.io republishes its tags with security
  fixes: the build stage (`dhi.io/golang:…-dev`) and the runtime (`dhi.io/static:…`). The
  `# syntax=` frontend line is pinned by tag and index digest. A base on `latest` or with no
  tag, a frontend without a digest, or a build-stage Go version that differs from the `go`
  directive in `go.mod`, is a finding.
- The runtime stage sets an explicit `USER 65532:65532`, never root, so the uid is pinned in the
  Dockerfile rather than inherited from the base image's default
  (`docker inspect --format '{{.Config.User}}'` on the built image prints `65532:65532`).

A diff must not make this worse: adding root, removing or changing the explicit `USER`, dropping
the static base, adding a shell or package manager to the runtime stage, or widening mounts and
capabilities in the compose files is a finding. Socket access is granted by adding the socket's
group (`--group-add`, `user: "65532:<gid>"`), not by running as root.

## Release and CI

`docs/release.md` is the contract; `.github/workflows/release.yaml` implements it. Tags here are
immutable, so the order of the release job is the safety property:

- The mode is decided from two fail-closed lookups before anything is pushed: the Git tag on the
  remote and `ghcr.io/…:<version>`. Only "not found" may read as absent; a lookup whose error is
  treated as absence is critical.
- The image is built once, into the job-local registry, and only the digest that was scanned is
  copied to GHCR (`skopeo copy --all --preserve-digests`), checked by hashing what GHCR serves,
  attested and signed, and only then tagged `:<version>`. A second image build, a scan of
  anything but the published digest, a copy that can change the digest (the build forces gzip
  layers for this reason), or a version tag added before attest and sign is a finding.
- A reused image is verified by attestation and signature, never by comparing a rebuilt index.
- Trivy exceptions live only in `.trivyignore`, each with a reason and an `exp:` date; an
  `--ignore-unfixed`, a lowered severity or an exception anywhere else is a finding.
- Workflow tokens are `contents: read` at the top of every workflow, and a job asks for more only
  with a comment saying why. Every action is pinned to a full commit SHA and every image a
  workflow runs to an index digest; a new floating `@v…` or `:tag` is a finding.
