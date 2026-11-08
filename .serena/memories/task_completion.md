# Task completion

Run the rows of the AGENTS.md validation matrix that match the edited paths; `make check` applies to
every row. Most common:

- `**/*.rs`, `Cargo.toml`, `Cargo.lock`: `make verify`; after `testenv/**` or anything the
  integration suite exercises, `make test-integration` (Docker).
- `Cargo.toml`/`Cargo.lock`/`deny.toml`: `make rust-deny` (network).
- Dockerfile/droast.toml/.dockerignore: `cargo test -p meta --test dockerfiles --test toolchain`,
  `make dist-static image-build-prebuilt image-test` (amd64 full; arm64
  `IMAGE_PLATFORM=linux/arm64 IMAGE_TEST_TIER=reduced` under QEMU), `make docker-build`.
- `scripts/image-test.sh`, `.mk/dist.mk`, `.mk/image.mk`: the same image runs; keep
  `docs/release.md#what-ci-proves` true.
- `.github/**`: `pre-commit run actionlint --all-files`,
  `cargo test -p meta --test toolchain --test dockerfiles`.
- `meta/**`: `cargo test -p meta`.
- `**/*.md`: `pre-commit run lychee markdownlint-cli2 --files <file>`; prose wraps at 100 columns.
- New file: MIT license header with the year of first commit (`scripts/check-license-headers.sh`).
- New files must be staged before `make check`: pre-commit skips untracked files.
