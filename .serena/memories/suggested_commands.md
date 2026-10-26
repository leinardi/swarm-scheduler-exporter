# Suggested commands

- `make help` — lists every target (project targets live in `.mk/*.mk`, never as Makefile recipes).
- `make verify` — build (stamped), fmt check, clippy (`-D warnings`, all features), tests.
- `make rust-build`, `make rust-test`, `make rust-clippy`, `make rust-fmt`, `make rust-run ARGS=...`.
- Single test: `cargo test --lib collector::reconciler -- --nocapture`, or
  `cargo test --test engine_wire <name>`.
- Meta-tests: `cargo test -p meta` (none skips on a missing tool).
- Integration (Docker, privileged): `make test-integration [RUN='^TestNode_']`, golden rewrite
  `make test-integration UPDATE=1 RUN=TestSnapshot_`, cleanup `make sweep-test-leaks`.
- `make check` (pre-commit, all files; the CI gate) / `make check-stage` (staged only).
- `make audit-deps` (cargo-deny advisories, network), `make rust-deny` (every cargo-deny check).
- `make docker-build` / `make docker-run` (dhi.io login required).
- Suppression baseline refresh: `scripts/check-suppressions.sh --update`.
- Workflow lint: `pre-commit run actionlint --all-files`.
