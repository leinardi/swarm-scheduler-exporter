# Verifying releases

Every release publishes two static Linux binaries, a `checksums.txt` over them and a multi-arch
image. You can check that what you downloaded was built by this repository's release workflow.

In the commands below, `<version>` is a release tag such as `v3.1.1`.

## What a release publishes

- `swarm-scheduler-exporter-linux-amd64` and `swarm-scheduler-exporter-linux-arm64`, each with a
  build provenance attestation.
- `checksums.txt`, the SHA-256 of both binaries.
- `ghcr.io/leinardi/swarm-scheduler-exporter:<version>` for `linux/amd64` and `linux/arm64`, with
  an SBOM, a build provenance attestation and a keyless [cosign](https://docs.sigstore.dev/)
  signature. Each platform's image contains that platform's release binary, byte for byte.

Version tags never move. `:latest` and `:<major>` follow the highest release.

## Check the binaries

Download the release assets and compare their checksums:

```bash
gh release download <version> --repo leinardi/swarm-scheduler-exporter
sha256sum -c checksums.txt
```

Then verify a binary's attestation:

```bash
gh attestation verify swarm-scheduler-exporter-linux-amd64 --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml
```

## Check the image

Verify the image's attestation and its signature:

```bash
gh attestation verify oci://ghcr.io/leinardi/swarm-scheduler-exporter:<version> --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml
cosign verify ghcr.io/leinardi/swarm-scheduler-exporter:<version> \
  --certificate-identity https://github.com/leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml@refs/heads/master \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The workflow signs with cosign v3, which stores the signature as a Sigstore bundle in an OCI 1.1
referring artifact. Verifying it needs cosign v3, or v2.6 or later with `--new-bundle-format`.
