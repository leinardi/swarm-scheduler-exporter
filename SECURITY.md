# Security policy

## Supported versions

Only the latest release gets security fixes. A fix ships as a new version: released tags and image tags are immutable, so an
existing version is never rebuilt or replaced. Upgrade to the latest release, or follow `ghcr.io/leinardi/swarm-scheduler-exporter:latest`
or the `:<major>` tag.

## Reporting a vulnerability

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/leinardi/swarm-scheduler-exporter/security/advisories/new), not in a public
issue, a pull request or a discussion. Include the version or image digest, how the exporter is deployed (socket mount, socket
proxy, TCP/TLS), and the steps or input that trigger the problem.

This is a project maintained in spare time, so reports are handled on a best-effort basis. You will get an answer in the advisory,
and the fix, once released, is credited there unless you prefer otherwise.

## Scope

In scope: the exporter binary, the container image published to GHCR, the release artifacts, and the workflows that build and
publish them.

Out of scope: vulnerabilities in Docker or Swarm themselves, and in the base images, which are reported upstream. A base image or
dependency vulnerability that affects a published release is still worth reporting here if the weekly scheduled scan has not
caught it.

## Security model

- **Read-only by code, not by mount.** The exporter only issues read requests to the Docker API (list, inspect and events), and
  the code enforces that: every Docker call goes through an interface that exposes nothing else. Mounting the socket with `:ro`
  does not make the API read-only, and access to the Docker socket is equivalent to root on the host. To enforce read-only access
  at runtime, put a socket proxy in front of the Docker API: the tested example in
  [Behind a socket proxy](README.md#-behind-a-socket-proxy-recommended) makes the API read-only, not least-privilege: its
  sections match by path prefix, so the `SERVICES` and `TASKS` the exporter needs also allow service and task logs, and
  `CONTAINERS=1` allows every `GET` under `/containers`. See also [Security & Permissions](README.md#-security--permissions).
- **Unauthenticated endpoints.** `/metrics` and `/healthz` have no authentication and are meant for an internal scrape network.
  Neither takes input that turns into work: no request parameter causes a Docker call.
- **Supply chain.** Every GitHub Action is pinned to a commit SHA and every image the workflows and the Dockerfile use to an index
  digest. Each release image is scanned for `HIGH` and `CRITICAL` vulnerabilities before its version is tagged, and the latest
  release is scanned again every week, together with the Go dependencies. The release process is described in
  [docs/release.md](docs/release.md).

## Verifying a release

Release images carry a build provenance attestation and a keyless cosign signature from the release workflow. Verify an image by
digest before you trust it:

```bash
IMAGE=ghcr.io/leinardi/swarm-scheduler-exporter
DIGEST=sha256:...   # from `docker buildx imagetools inspect $IMAGE:<version>`

gh attestation verify "oci://$IMAGE@$DIGEST" --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml --source-ref refs/heads/master

cosign verify "$IMAGE@$DIGEST" \
  --certificate-identity https://github.com/leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml@refs/heads/master \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The signature is stored as a Sigstore bundle in an OCI 1.1 referring artifact, so `cosign verify` needs cosign v3, or v2.6 or later
with `--new-bundle-format`.

The release binaries carry a build provenance attestation from the same workflow. Verify a downloaded binary before you run it:

```bash
gh attestation verify swarm-scheduler-exporter-linux-amd64 --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml --source-ref refs/heads/master
```
