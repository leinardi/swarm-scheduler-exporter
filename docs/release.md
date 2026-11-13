# Release

How a swarm-scheduler-exporter release is versioned, built and published, what to do when one fails
halfway, and what has to be set up by hand for all of it to hold. The pipeline is
[`.github/workflows/release.yaml`](../.github/workflows/release.yaml), built from the shared
actions of [gh-composite-actions](https://github.com/leinardi/gh-composite-actions)
(`release-version`, `require-ci-green`, `release-mode`, `toolchain-record`, `toolchain-recover`,
`check-pinned-bases`, `trivy-gate`, `binary-attestation-status`); this page is why it is shaped that
way.

## What a release produces

- Two static binaries, `swarm-scheduler-exporter-linux-amd64` and `swarm-scheduler-exporter-linux-arm64`,
  cross-compiled for the `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` targets by
  `make rust-release-binaries` (`cargo zigbuild --release --locked`), with the build paths remapped
  and the version, commit and commit date compiled in, plus `checksums.txt` over both. All three are
  attached to the GitHub release, and each binary has a
  [build provenance attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations)
  from the release workflow.
- A multi-arch (`linux/amd64`, `linux/arm64`) image at
  `ghcr.io/leinardi/swarm-scheduler-exporter:<version>`, built from the Dockerfile's `prebuilt` target:
  **each platform's image binary is that platform's release binary, byte for byte**. It carries an
  SBOM and `mode=max` BuildKit provenance in its index, a build provenance attestation and a keyless
  [cosign](https://docs.sigstore.dev/) signature made with the release workflow's own OIDC
  identity. Before the version was tagged, CI had passed on the commit (the full image suite on
  amd64 and the reduced one on arm64, [What CI proves](#what-ci-proves)), and both platform images
  passed the release's short smoke test and a [Trivy](https://trivy.dev/) scan with no fixable
  `HIGH` or `CRITICAL` vulnerability outside [`.trivyignore.yaml`](../.trivyignore.yaml).
- The moving image tags `:<major>` and `:latest`, and the GitHub "Latest" release marker, when the
  version is the highest released one.
- A GitHub release whose notes name the Rust toolchain the binaries were built with and the base
  digest each platform image was built on (read from the build's provenance), followed by notes
  generated from the merged pull requests.

Verify what was published:

```bash
gh attestation verify swarm-scheduler-exporter-linux-amd64 --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml
gh attestation verify oci://ghcr.io/leinardi/swarm-scheduler-exporter:<version> --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml
cosign verify ghcr.io/leinardi/swarm-scheduler-exporter:<version> \
  --certificate-identity https://github.com/leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml@refs/heads/master \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The workflow signs with cosign v3, which stores the signature as a Sigstore bundle in an OCI 1.1
referring artifact, so verifying it needs cosign v3, or v2.6 or later with `--new-bundle-format`.

## How to release

Run the **Release swarm-scheduler-exporter** workflow from the Actions tab (or
`gh workflow run release.yaml`) on `master`. It takes two inputs:

- `version`: optional, `X.Y.Z` or `vX.Y.Z`. Left empty, the version is derived from the commits
  since the last release (see [Versioning](#versioning)). Give it explicitly to force a version, or
  to recover a failed run with a fresh dispatch (see
  [Recovering a failed publish](#recovering-a-failed-publish)).
- `dry_run`: boolean, default `false`. See [Dry runs](#dry-runs).

Dispatch it once CI and the integration suite have run on the commit's push to `master`: the
`ci-green` job waits for each run for up to 30 minutes, and fails if one did not pass
([What CI proves](#what-ci-proves)).

**One release runs at a time.** The workflow takes a `release` concurrency group with
`cancel-in-progress: false`, so a second dispatch queues behind the first rather than racing it. Two
runs in flight together would each check their version against the same latest tag, both pass, and
then publish in whatever order they finished.

### Dependabot releases

A merge of a Dependabot pull request releases the next patch on its own, with no dispatch, when
everything since the last release is Dependabot's and at least one of its commits is a `fix(deps)`
(a new runtime base image or a crate update). The workflow also runs on every push to `master`: its
`dependabot` job calls gh-reusable-workflows' `dependabot-release` with `ecosystems: docker cargo`,
which checks from the API alone that every unreleased commit is Dependabot's own (committed by
GitHub) or the merge of its pull request, that the change only moves action pins and the versions of
those ecosystems' files, and that CI passed on the push. Then the release runs as a dispatch would,
with that patch as an explicit version. Anything else is a green no-op with a notice: a
`chore(deps)` merge (actions, pre-commit, development dependencies) ships nothing, and a human
commit waiting to be released leaves the release to a dispatch.

A push that merges a Dependabot pull request takes the same `release` group as a dispatch, so the
two never publish at once and a lower version never moves `latest` after a higher one. Every other
push runs in a group of its own and only skips the `dependabot` job. GitHub keeps one pending run
per group: a Dependabot merge that arrives while a release runs and a dispatch waits replaces that
dispatch, which shows as cancelled; dispatch it again.

## Versioning

Semantic versioning, derived from
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) by
[`svu`](https://github.com/caarlos0/svu), as [AGENTS.md](../AGENTS.md#commit-messages) describes:

- `feat` is a minor;
- `fix` is a patch;
- `!` before the colon, or a `BREAKING CHANGE:` footer, is a major. The project is already at `2.x`,
  so the `0.x` caveat of `svu` (a breaking change before `1.0.0` jumping straight to `1.0.0`) does
  not apply here.

Everything else (`build`, `chore`, `ci`, `docs`, `perf`, `refactor`, `revert`, `style`, `test`)
bumps nothing, so a performance improvement, refactor or revert that users should receive is
committed as a `fix` (or `feat`). When no commit since the last release is a `feat`, a `fix` or a
breaking change, a run with no version **fails** with "nothing to bump". Dependabot's updates of
production crates and of the image's base (`dhi.io/static`) are committed as `fix(deps)`, so either
alone is enough for a patch release; its dev-dependency (test-only), GitHub Actions and pre-commit
updates stay `chore(deps)` and bump nothing.

Whether derived or typed, the version must be **higher than every version already released**: tags
here are immutable, so publishing a `v1.9.9` after `v1.10.0` would create a permanent lower tag. The
only version a run may reuse is the one it is recovering, which is exactly this version already
sitting on `HEAD`. A branch whose history does not contain the highest release derives from an older
base and so fails that check; the failure names the out-of-history tag, and an explicit version
higher than it is the way out.

The version is resolved by the `release-version` action. svu is installed (`setup-svu`, pinned with
its checksum) only when no version is given, so a version typed by hand releases even while
GitHub's release downloads fail. Only strict `vMAJOR.MINOR.PATCH` tags are releases
(`RELEASE_TAG_REGEX` in the workflow, `^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$`, the
actions' default too): an explicit `01.2.3`, `1.2.3-rc.1`, `1.2.3+meta` or `v1.2.3.4` is refused.
Every lookup filters the full tag list through that regex, never through a glob, so pointer tags
(`latest`, `v1`), pre-releases and malformed tags can never become the current version, and `svu`
is given the exact tag to count from, literally.

### Stray local tags

CI fetches only the tags on `origin`. A local `svu next` reads every local tag, so before reasoning
about versions locally, drop the tags `origin` does not have:

```bash
git fetch --prune --prune-tags origin
```

## Why the build happens before the tag

Tags in this repository are immutable. The "Immutable tags" ruleset, managed in
[gh-leinardi-iac](https://github.com/leinardi/gh-leinardi-iac), covers every tag except `v[0-9]`,
`v[1-9][0-9]` and `latest`, blocks update, deletion and non-fast-forward, and has no bypass actors.
A failed release cannot delete its tag, so everything that can fail happens before the tag exists,
and everything after it can be finished by a re-run on the tagged commit.

The run has four jobs:

1. **`guard`**: refuse anything but `master`, failing rather than skipping when `github.ref` is not
   `refs/heads/master`; then resolve the version, as in [Versioning](#versioning), and read the
   stamps from the commit.
2. **`ci-green`**, after `guard`: `require-ci-green` on the commit. CI's run for the push of this
   exact commit to `master` (`.github/workflows/ci.yaml`, by file path, not by a check name any
   workflow could reuse) must have concluded `success`, with its `ci-result` job, and so must the
   integration suite's run for the same push (`.github/workflows/integration.yaml`, its
   `integration` job). This replaces the release-time clippy and test suite, and the full image
   tests ([What CI proves](#what-ci-proves)).
3. **`audit`**, alongside the two: `cargo deny check` (advisories, licences, bans and sources), the
   one time-sensitive check of the code, since advisories are published every day. It needs nothing
   and nothing needs it, so it runs while the release job builds and costs the release almost no
   time: the release job waits for it (`require-job-success`) only right before its first step that
   publishes anything, and stops unless it passed.
4. **`release`**, which checks the Dockerfile pins (`check-pinned-bases`, as CI does), sets up the
   pinned Rust (on a recovery, the one the tag records, through `toolchain-recover`), and then
   **decides the mode** (`release-mode`) from the Git tag on the remote, the image
   `ghcr.io/leinardi/swarm-scheduler-exporter:<version>`, the GitHub release and its assets. Only "not
   found" counts as absent: any other lookup error stops the run.

   | Git tag | Published | Mode |
   | --- | --- | --- |
   | absent | nothing | **Fresh** |
   | absent | the image `:<version>`, or a GitHub release | **Stop before tagging.** The version is held |
   | on another commit |  | **Stop.** Tags are immutable: release a higher version |
   | on this commit | the image, attested and cosign-signed by this workflow for this commit | **Recovery, reuse** |
   | on this commit | no image | **Recovery, publish** |
   | on this commit | anything unattested, unsigned, or differing from the image | **Stop: needs a human** |

   A tag on this commit must be annotated, in the clone, and record the toolchain this run builds
   with (`Rust: 1.98.1`), as this workflow writes it. The tag is then created **locally only**, if
   it does not exist yet.

A **fresh** release then runs:

1. **Build the binaries once**, with `make rust-release-binaries`, the make target CI builds them
   with. The Rust build cache is the release's own (`prefix-key: release`), written only by this
   workflow. `checksums.txt` is written over both (`make rust-release-checksums`).
2. Start a registry inside the job (`registry:3.1.2`, pinned by digest).
3. **Build the image once**, the Dockerfile's `prebuilt` target, for both platforms, with those
   binaries as the `dist` build context, so the image binary is the release asset. It is pushed by
   digest to the local registry, with the SBOM and `mode=max` provenance, every layer recompressed
   as gzip: the `dhi.io` base layers are zstd, and for a zstd image skopeo rewrites the index
   entries with a compression annotation, which the digest-preserving copy in step 9 then refuses.
   There is no layer cache: the target is the base plus one COPY.
4. **Record the base the build used**: the base is pinned by tag, so each platform's provenance
   names the digest the tag resolved to. The build must have pulled exactly one digest of the base
   tag and the `# syntax=` frontend pin, and nothing else (apart from BuildKit's SBOM scanner,
   which generates the SBOM and is not part of the image); the digests go into the release notes
   and the summary, as the exact base the release shipped.
5. **Smoke-test and scan both platform images**, each pulled back from the local registry by its
   own digest: `scripts/image-test.sh --tier smoke` checks that the image binary's sha256 is the
   release asset's, and that the container, started with no Docker socket, logs
   `<version> <short commit> <build date>` in its startup line and serves them as
   `swarm_exporter_build_info` on `/metrics` (arm64 under QEMU). The binary has no `--version`; a
   build that lost a build argument reports the `dev` defaults and fails here, before the tag. Then
   `trivy-gate` scans each one (see [The scan gate](#the-scan-gate)).
6. **Gate the tag.** The highest released version is read again from the remote; a fresh release
   that is no longer the highest fails with nothing pushed.
7. **Check the version image is absent**, right before the tag.
8. **`git push origin <version>`**: the point of no return. The checkout keeps no credentials; the
   token reaches this one command as an HTTP header.
9. **Copy to GHCR, untagged**: `skopeo copy --all --preserve-digests` to
   `ghcr.io/leinardi/swarm-scheduler-exporter@<digest>`, with skopeo pinned by digest.
10. **Check, attest, sign.** The index GHCR serves under the digest must hash to the tested digest;
    then it is attested and signed.
11. **Tag the image** `:<version>`, after checking once more that the tag is absent.

The binary attestations, the release assets and the latest pointers follow, in every mode, as
described under [Recovery guarantees](#recovery-guarantees).

Everything stamped into an artifact comes from the commit rather than from the clock: the version, a
fixed seven-character commit prefix, the commit date as the build date (and the image's `created`
label), and the commit time as BuildKit's `SOURCE_DATE_EPOCH`. The binaries are built with the
pinned Rust toolchain, zig and cargo-zigbuild, `Cargo.lock` (`--locked`), and `--remap-path-prefix`
for the checkout, the cargo home and the toolchain's `rust-src` (when installed, mapped back to the
`/rustc/<commit>` prefix a machine without it writes), so a re-run of the same commit produces the
same bytes, which is what lets a recovery check published assets by sha256. The image is not
byte-reproducible: its provenance records the build itself, so two builds of the same commit have
different index digests. That is why a recovery never compares a rebuilt index with a published one,
and never rebuilds a published one.

## What CI proves

The release runs no clippy, no test suite and only a short smoke test of the image. That is sound
only because CI already ran them on the same inputs, and the `ci-green` job pins that to the commit
being released:

- **The same commit.** `require-ci-green` accepts only the `push` run of `ci.yaml` whose head is
  the release's own SHA, on `master`, and its `ci-result` job must have passed on the run's newest
  attempt.
- **The code was tested on that commit.** `ci-result` needs the `rust` job (rustfmt, clippy, the
  tests, the docs and cargo-deny) and `pre-commit-all` (every hook on every file).
- **The packaging was tested on that commit.** `ci-result` needs the `image` job, which runs on
  every pull request and every push, with no path filter. It builds the static binaries with
  `make rust-release-binaries`, the release's own make target, then the `prebuilt` image target
  from them for both platforms:
  - **linux/amd64, the full suite** (`scripts/image-test.sh --tier full`): the image is the
    platform's; its binary is the `dist/` binary just built, byte for byte; the container, started
    with no Docker socket, logs the stamps in its startup line and serves them as
    `swarm_exporter_build_info` on `/metrics`; the binary is a static x86-64 ELF, `0755 root:root`;
    the image runs as `65532:65532`, with the binary as its entrypoint, port 8888 exposed, the
    binary's own `--healthcheck` as its health check and the labels the stamps; and the container,
    hardened as `docker-compose.yaml` runs it and more (read-only root, no-new-privileges, every
    capability dropped), reports itself not ready on `/healthz` (HTTP 503) with no Docker daemon to
    reach, so its `--healthcheck` probe exits 1, stops cleanly on SIGTERM, and fails to start on an
    invalid flag value.
  - **linux/arm64, the reduced suite** under QEMU (`--tier reduced`): the same up to the image
    config, the container starting and serving its stamps included.

  The exporter against a real Swarm is `integration.yaml`'s job, not this one: it runs the
  integration suite against the release-profile glibc binary on every pull request and every push to
  `master`, and the ruleset requires it on pull requests. `ci-result` cannot need a job of another
  workflow, so `ci-green` reads that push run's `integration` job separately
  ([Required checks](#required-checks)).
- **The same build.** CI and the release build the binaries with the same make target, toolchain,
  zig and cargo-zigbuild (one value each, `meta/tests/toolchain.rs`), and the image from the same
  Dockerfile target and build context shape. The build arguments differ only in the stamps.
- **The same base tag, not always the same base digest.** The `dhi.io` bases are pinned by tag,
  deliberately not by digest: dhi.io rebuilds its tags too often for digest pins to be worth
  following by hand. `check-pinned-bases` (with `tag-only-images: dhi.io`) fails CI and the release
  on a `dhi.io` base with no explicit tag or on `latest`, on any other image without a digest, and
  on any download not followed by a checksum check. So the release may build on a newer dhi.io
  rebuild of `dhi.io/static:<tag>` than the one CI tested. That is the accepted trade-off of
  pinning by tag: a rebuild of a distroless static base carries no code of ours, and what covers
  the base the release ships is the release's own smoke test and `trivy-gate` scan of that exact
  image, both before the tag. The release notes record the digest it shipped.

A pull request **from a fork** gets no `DHI_TOKEN`, and the base cannot be pulled without a login,
so there the `image` job says so in a notice and tests nothing. The push to `master` after the merge
runs it in full, and that run is the one the release gates on. Any other run without the secret
fails the job.

What only the release can see is what changed since: a vulnerability published after CI ran (the
scan and the audit run at release time for that reason), a newer rebuild of the base tag, and a
broken image build, which the smoke test on the release's own build would catch before the tag.

## The scan gate

The gate (`trivy-gate`, on both platform images) fails on a `HIGH` or `CRITICAL` vulnerability
**with a fix upstream**; the findings with no fix yet are left out (`--ignore-unfixed`), since
nothing in this repository can fix them. Trivy is always its latest release, never pinned, so every
scan runs the newest checks; its vulnerability database is fetched at run time, which is the point
of scanning at release time.

The base's findings count the same as the image's own: a fixable finding in `dhi.io/static` fails
the release too. Its fix is usually dhi.io's next rebuild of the tag, which the next release picks
up by itself, or a newer tag, which Dependabot proposes; until it ships, a
reviewed entry in [`.trivyignore.yaml`](../.trivyignore.yaml) is the only way past the gate, scoped
to the paths or purls it was found in, with a statement saying why and an expiry date Trivy
enforces. When an entry expires, bump what carries the fix if it is out, otherwise extend the date
with a fresh reason.

The binary is not built with `cargo auditable`, so Trivy cannot see its crates; cargo-deny, in the
`audit` job and CI, is the check for them.

## Dry runs

`dry_run: true` rehearses a fresh release: it resolves the version and decides the mode (so it
stops, like a real run would, when the version is held or needs a human), then runs the CI gate,
the audit and fresh steps 1 to 7 (the smoke tests and the scans included), and exercises step 9 by
copying the index to a **second job-local registry** instead of GHCR, checking that the copy keeps
its digest and one attestation manifest per platform. It also runs the binary attestation check, as
a rehearsal, but attests nothing. It pushes no tag and publishes nothing. A dry run of a version
whose image already exists stops at step 7.

The job summary shows the version, the previous tag, whether the version was explicit, the mode a
real run would take, the index and platform digests, the base and the checksums; `dist/` is uploaded
as the workflow artifact `dry-run-<version>-<attempt>`. A dry run with no version is also the
quickest way to see what version the commits would produce.

## Recovering a failed publish

A recovery runs on the tagged commit, because that is the only commit the tag check accepts for an
existing tag:

- **"Re-run all jobs" on the failed run.** This always works: a re-run keeps the original run's
  commit and inputs, whatever has been merged since, and that commit's CI already passed. Use "Re-run
  all jobs", not "Re-run failed jobs", unless `audit` itself failed: the release job accepts only an
  `audit` of its own attempt (`require-job-success` never trusts an earlier attempt's pass), and
  "Re-run failed jobs" does not re-run an `audit` that passed, so the release would wait for it until
  the step times out.
- **A fresh dispatch with the same version, given explicitly**, while `master` still points at the
  tagged commit.

`release-mode` then decides **per platform**, so the image binary and the release asset of a
platform never diverge. "Published" means the image (or its platform manifest) is on GHCR, or the
release, draft or not, carries the asset; each must be attested by this workflow for this commit,
and the image signed by it too, or the run stops.

| Published for a platform | The binary comes from | What happens |
| --- | --- | --- |
| Image and asset | the image | they must be the same bytes, or the run stops |
| Image only | the image | it is extracted, attested and uploaded as the asset |
| Asset only | the asset | it is downloaded and verified, and the image is built with it |
| Neither | a rebuild | it is built now, with the recorded toolchain |

- **Recovery, reuse**, when `:<version>` exists and is attested and signed by this workflow for this
  commit: nothing is built; missing assets are extracted from the image. The image is scanned, and
  findings are reported as warnings: it is already published and immutable.
- **Recovery, publish**, when the tag exists and `:<version>` does not: the image is built from the
  published binaries where there are any and rebuilt ones for the other platform, then checked
  against the pins, smoke-tested, scanned, copied, checked, attested, signed and tagged. If the scan
  now fails, because a fixable vulnerability was published since, **the version is abandoned**: its
  tag stays, with no image, and the fix goes out as a higher version.

A published image that lacks a shipped platform stops the run: a manifest list cannot gain a
platform without changing its digest. A tag made before the toolchain was recorded in its
annotation cannot be recovered either (`toolchain-recover` and `release-mode` fail closed on it);
every release since this workflow records it.

The workflow artifacts carry the run attempt in their names (`binaries-<version>-<attempt>`,
`dry-run-<version>-<attempt>`), because a re-run stays inside the same workflow run and
`upload-artifact` refuses to overwrite the first attempt's upload. There is no cleanup step and the
tag is never deleted. If the release is wrong rather than incomplete (the wrong commit, a bad
build), the answer is a new version, not a moved tag. The stop rows of the table are for a human:
find out where the image, the asset or the release came from, and release a higher version.

## Recovery guarantees

One rule governs every publishing step: **a published artifact is never replaced, and it is reused
only once it is proven to be this run's**, never because something with the right name exists.

- **The image.** A new image is copied to GHCR by digest with no tag, attested and signed, and only
  then tagged `:<version>`, so a version tag only ever points at a digest this workflow tested,
  scanned, attested and signed. An untagged digest left by a failed run is inert. A reused image
  must pass `gh attestation verify oci://…@<digest> … --source-ref refs/heads/master --source-digest
  <commit>` and carry this workflow's own cosign signature (identity
  `https://github.com/leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml@refs/heads/master`,
  issuer `https://token.actions.githubusercontent.com`), both checked by `release-mode` first.
- **The release assets.** The release is created as a draft if it does not exist. Each expected
  asset (the two binaries and `checksums.txt`) is inventoried one by one: a present asset is
  downloaded and compared by sha256 with this run's (the rebuilt or verified binary), and with the
  published `checksums.txt` when that is present, and a mismatch fails the run; a missing asset is
  uploaded, never with `--clobber`. An asset that is not expected fails the run. `checksums.txt` is
  regenerated over the final binaries, and must equal a published one.
- **The binary attestations.** The binaries are the same bytes on every run, so an attestation an
  earlier run made for one is reused, not duplicated. `binary-attestation-status` looks up each
  binary's sha256: one with no attestation is attested; one with an attestation must pass
  `gh attestation verify <binary> … --source-ref refs/heads/master --source-digest <commit>` and is
  then left alone. Any other answer stops the run before the release is drafted, for a human.
- **The latest pointers.** `:latest`, `:<major>` and the GitHub "Latest" marker move only if the
  version is still the highest when that pointer is moved: the highest remote tag is read again
  immediately before retagging the image, and again immediately before the release is published out
  of draft.

The last one is **best-effort, not atomic**. GHCR tags and GitHub releases cannot be updated
together, so a tag created outside this workflow in the seconds between the two moves can still
leave them disagreeing. The run then warns rather than moving anything back. Re-dispatch the higher
version, whose own late checks repoint them, or retag by hand after checking the digest:

```bash
IMAGE=ghcr.io/leinardi/swarm-scheduler-exporter
DIGEST=sha256:...   # the digest of the image that should be latest
gh attestation verify "oci://$IMAGE@$DIGEST" --repo leinardi/swarm-scheduler-exporter \
  --signer-workflow leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml --source-ref refs/heads/master
docker buildx imagetools create -t "$IMAGE:latest" -t "$IMAGE:<major>" "$IMAGE@$DIGEST"
```

## Scheduled scan

A release is scanned once, when it is built, but vulnerabilities keep being published.
[`.github/workflows/scheduled-scan.yaml`](../.github/workflows/scheduled-scan.yaml) runs every
Monday, and on demand from the Actions tab. It resolves `:latest` to its index digest once and scans
the `linux/amd64` and `linux/arm64` images by their own digests with the release's gate
(`trivy-gate`, the latest Trivy, the same ignore file), and runs `make rust-deny` over `master`'s
dependencies. Any finding fails the run. The published image is immutable, so the fix is a new
release (for a base finding, on the base tag's newer rebuild), or a reviewed entry in the ignore
file.

## What the workflows trust

**Every action is pinned to a full commit SHA**, with the version in a trailing comment, in every
workflow; Dependabot's `github-actions` ecosystem updates the pins. A tag is a mutable pointer, and
an action that moved would execute inside the release job, which holds `contents: write`,
`packages: write` and an OIDC identity. The one tool deliberately not pinned is Trivy, which
`trivy-gate` installs at its latest release so every scan runs the newest checks
(`meta/tests/toolchain.rs` fails on a Trivy pin). svu, ShellCheck, zig, cargo-zigbuild and
cargo-deny are pinned with their checksums inside the gh-composite-actions that install them.

**Every image the release job runs is pinned by digest**: the job-local registry and skopeo in the
workflow's `env`, the QEMU emulator and the Dockerfile frontend (`# syntax=…@sha256:…`). Bump them
by hand, resolving a new digest with `docker buildx imagetools inspect <image>:<tag>`. **The
`dhi.io` bases are pinned by tag, deliberately not by digest**, because dhi.io rebuilds its tags too
often: `dhi.io/rust` for the default target's build stage and `dhi.io/static` for the shipped base,
each in one literal `FROM <image>:<tag>` with an explicit tag other than `latest`
(`meta/tests/dockerfiles.rs`, `check-pinned-bases`). Each release records the base digest it
shipped (see [What CI proves](#what-ci-proves)). Dependabot's `docker` ecosystem moves the
`dhi.io/static` tag; the `dhi.io/rust` tag is bumped by hand with `RUST_VERSION`, the `rust-std`
digests and `rust-toolchain.toml` (the `toolchain` meta-test fails when the Rust line, the image
patch or the copies of a zig pin disagree).

**The binaries are built outside Docker, once.** The release's `rust-release-binaries` gets zig and
cargo-zigbuild from gh-composite-actions' `setup-zigbuild`, at the versions the Makefile pins and
the Dockerfile's build stage copies (`ZIG_VERSION`, `CARGO_ZIGBUILD_VERSION`), and refuses any other
release on `PATH`; Rust comes from `rust-toolchain.toml` (`rustup toolchain install`), or, on a
recovery, the release the tag records. The image's `prebuilt` target never runs that build stage: it
COPYs the binaries, so `dhi.io/rust` is not part of a release at all.

**Workflow tokens are read-only by default.** Each workflow sets `permissions: contents: read` at
the top; only the release job adds `contents: write`, `packages: write`, `id-token: write`,
`attestations: write` and `actions: read` (to read the `audit` job), in the `release` environment,
`ci-green` adds `actions: read`, and the
reviewdog jobs in `ci.yaml` add `pull-requests: write` to post their review. The warm-up's
calling job adds `actions: write`, so the reusable workflow's prune job can delete the older
pre-commit caches of its ref.

**Caches.** The release's Rust build cache is its own (`prefix-key: release`), restored and saved
only by this workflow, which runs on `master` only; CI's (`shared-key: ci`) is saved only on `master`,
so no pull request can seed a cache the release reads; `integration.yaml` saves its own
(`shared-key: ci`, `key: integration`) on `master` only, too. A cache only speeds the build: the binaries
come from `Cargo.lock` and the remapped, pinned toolchain either way, and a recovery checks them by
sha256 against what was published. No image build uses a layer cache. The pre-commit cache is
written only by the warm-up workflow, on `master`.

## One-time setup

### Environment and credentials

The release job runs in the `release` environment. Create and configure it **before the first
dispatch**, dry runs included: a run that references a missing environment creates it with no
protection rules.

1. **Restrict it to `master`** with a deployment branch rule, as a server-side second line of defence
   behind the `guard` job. Add required reviewers too if a release should wait for a human. Leave
   the "Immutable tags" ruleset exactly as it is; the release job is written for it.
2. **The `dhi.io` token.** The image's base, `dhi.io/static`, cannot be pulled without a login (the
   user name is `leinardi`, written in the workflows), so three places need `DHI_TOKEN`:
   - the **release** job: an Actions secret, best **on the environment**
     (`gh secret set DHI_TOKEN --env release`), so only a job that passed the environment's rules
     reads it;
   - CI's **`image` job**, which runs on pull requests and pushes outside the environment: a
     repository Actions secret (`gh secret set DHI_TOKEN`);
   - **Dependabot**, which does not see Actions secrets: both to update the base (the `dhi`
     registry in [`.github/dependabot.yml`](../.github/dependabot.yml)) and for CI's `image` job on
     its pull requests (`gh secret set DHI_TOKEN -a dependabot`).

   The `DHI_USERNAME` variable is no longer read.

### Required checks

A failing CI job does not block a merge on its own: the `master` ruleset needs a required status
check for that. The rulesets are managed as code in
[`leinardi/gh-leinardi-iac`](https://github.com/leinardi/gh-leinardi-iac), in the
`repo_swarm_scheduler_exporter` module of `stacks/github-repos/repos.tf`, never by hand or with
`gh api`: a change to them is a pull request there.

The checks to require are `ci-result` and `integration`, set there as
`default_branch_required_checks = ["ci-result", "integration"]`. `ci-result` needs every other job
of `ci.yaml` (`pre-commit-all`, `conventional-commits`, `rust` and `image`) and fails
unless each one succeeded; `conventional-commits` alone may be skipped, off pull requests. A new
`ci.yaml` job therefore becomes required by joining `ci-result`'s `needs`, not the ruleset, and
GitHub counting a skipped required job as passing is why the jobs themselves are not listed.
`integration` runs in its own workflow, which `ci-result` cannot need, so it is required by name.
The reviewdog annotation jobs (`actionlint`, `pre-commit-hooks`, `markdownlint`, `shellcheck`,
`yamllint`) stay optional: they duplicate `pre-commit-all` as inline comments. None of these
workflows uses path filters, so every required check reports on every pull request.

The release does not depend on the rule: it reads `ci-result` of the commit's `ci.yaml` push run
and `integration` of its `integration.yaml` push run itself (`require-ci-green`, once for each).

Once the ruleset is applied, open one pull request and confirm the required checks report under
exactly the names `ci-result` and `integration`. A required check that never reports blocks every
merge.

Repository admins keep the ruleset's pull-request bypass, so an admin can still merge a pull request
whose check fails. That is a deliberate escape hatch, not a gap in the check.

### Bumping the Rust toolchain

One exact Rust release builds everything: `rust-toolchain.toml` (which the workflows install with
`rustup toolchain install`, so they name no release of their own) and the
`dhi.io/rust:<version>-debian13-dev` tag of the Dockerfile's build stage, and
`meta/tests/toolchain.rs` fails when they differ, patch included. Dependabot
ignores the `rust` image on purpose: `dhi.io/rust` has no rustup, so the build stage adds each musl
target by unpacking `rust-std-<version>-<triple>.tar.xz` from static.rust-lang.org and checking it
against a sha256 written in the Dockerfile, and those archives must be the image's exact release.
A bump is therefore one hand-made pull request that moves, together:

1. the image tag and `ARG RUST_VERSION` in the Dockerfile;
2. the two `rust-std` sha256s in the Dockerfile (the `.sha256` file published next to each
   archive);
3. `RUST_RELEASE_RUST_STD_VERSION` and the same two sha256s in the Makefile;
4. `rust-toolchain.toml`;
5. the MSRV in `Cargo.toml` when the minor version changes.

zig and cargo-zigbuild follow the same rule between the Makefile (the one record of their versions
and archive digests), the Dockerfile and the `setup-zigbuild` steps of `ci.yaml` and `release.yaml`
(their `zig-version` and `cargo-zigbuild-version` inputs): each version is one value, and each
digest one value between the Makefile and the Dockerfile, checked by the same meta-test. The
Makefile's `RUST_RELEASE_*` settings override the defaults of the shared `.mk/rust-release.mk`,
which is fetched from make-common and never edited here.
gh-composite-actions' `setup-zigbuild` checks the archives it downloads against its `zig-sha256` and
`cargo-zigbuild-sha256` inputs, whose defaults are its default versions' digests, so a version bump
passes the new digests to it too. A release built after such a bump produces new binary digests, as
any toolchain change does; a recovery of an older release must run on its own commit, which still
names its own toolchain.

## Commit messages are checked on every pull request

Every commit must be a Conventional Commit with a scope (see
[CONTRIBUTING.md](../CONTRIBUTING.md#commit-messages)). The local `commit-msg` hook only runs where
somebody ran `make pre-commit-install`, so a commit made without it would otherwise reach `master`
unchecked. The `conventional-commits` job in
[`.github/workflows/ci.yaml`](../.github/workflows/ci.yaml) closes that gap: on every pull request
it runs the repository's own `conventional-pre-commit` hook, `--force-scope` included, over every
commit between the pull request's base and head. Merge commits are skipped.

Only merge commits are enabled on this repository; squash and rebase merging are disabled. Every
commit in a pull request therefore lands on `master` as it is, and the pull-request title never
becomes a commit subject, which is why there is no title check. If squash merging is ever enabled,
the title becomes the commit subject, and a pull-request title check has to be added to CI first.

## Historical notes: the Go releases (1.x)

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
