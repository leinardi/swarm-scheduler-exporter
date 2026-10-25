# Task completion

Run the rows of the AGENTS.md validation matrix that match the edited paths; `make check` applies to
every row. Most common:

- `**/*.rs`, `Cargo.toml`, `Cargo.lock`: `make verify`; after `testenv/**` or anything the
  integration suite exercises, `make test-integration` (Docker).
- `Cargo.toml`/`Cargo.lock`/`deny.toml`: `make rust-deny` (network).
- Dockerfile/droast.toml/.dockerignore: `cargo test -p meta --test dockerfiles --test toolchain`,
  `make docker-build`.
- `.github/**`: `pre-commit run actionlint --all-files`,
  `cargo test -p meta --test toolchain --test dockerfiles`.
- `meta/**`: `cargo test -p meta`.
- `**/*.md`: `cargo test -p meta --test docs`; prose wraps at 100 columns.
- New file: MIT license header with the year of first commit (`scripts/check-license-headers.sh`).
- New files must be staged before `make check`: pre-commit skips untracked files.
