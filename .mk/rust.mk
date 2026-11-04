#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
#

ifndef MK_COMMON_RUST_INCLUDED
MK_COMMON_RUST_INCLUDED := 1

# A Cargo project or workspace. Every command runs workspace-wide (one package is a workspace of
# one), so a lint set declared once in [workspace.lints] reaches every member, and --locked
# everywhere: Cargo.lock is the pin, and a command that rewrote it would build a dependency graph
# nobody reviewed.

CARGO    ?= cargo
BIN_NAME ?= app
DIST_DIR ?= dist

# Version metadata (generic, derived from git when available), as go.mk has it.
GIT_COMMIT  ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo "none")
GIT_VERSION ?= $(shell git describe --tags --dirty --always 2>/dev/null || echo "v0.0.0")
BUILD_DATE  ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")

# RUST_STAMP_PREFIX: the binary reads its version at compile time from <prefix>_VERSION,
# <prefix>_COMMIT and <prefix>_DATE (option_env! or env!), e.g. RUST_STAMP_PREFIX := MYAPP.
# Empty: no stamps. Only the builds and rust-run stamp, so the tests see the binary's defaults.
# RUST_VERSION_ENV is the assignment list, for a consumer's own recipes.
RUST_STAMP_PREFIX ?=
RUST_VERSION_ENV = $(if $(strip $(RUST_STAMP_PREFIX)), \
  $(RUST_STAMP_PREFIX)_VERSION="$(GIT_VERSION)" \
  $(RUST_STAMP_PREFIX)_COMMIT="$(GIT_COMMIT)" \
  $(RUST_STAMP_PREFIX)_DATE="$(BUILD_DATE)")

# The features rust-build, rust-clippy and rust-doc compile; empty for the default features. The
# tests take the default features, so a feature whose tests need more than cargo (Docker, a
# device) stays out of rust-test while its code is still built and linted.
RUST_FEATURES ?= --all-features

# cargo-deny's scope; --workspace checks every member's graph, not only the root package's.
RUST_DENY_ARGS ?= --workspace

# rust-run's binary, for a package with more than one; empty lets cargo pick the only one.
RUST_RUN_BIN ?=

# Where cargo puts its output, so rust-dist finds the binary under a custom CARGO_TARGET_DIR.
RUST_TARGET_DIR ?= $(or $(CARGO_TARGET_DIR),target)

.PHONY: rust-build
rust-build: ## Build the Rust workspace, every target, with the version stamps
	$(RUST_VERSION_ENV) $(CARGO) build --workspace --all-targets $(RUST_FEATURES) --locked

.PHONY: rust-dist
rust-dist: ## Build the release binary BIN_NAME, with the version stamps, into DIST_DIR
	$(RUST_VERSION_ENV) $(CARGO) build --release --locked --bin "$(BIN_NAME)"
	@mkdir -p "$(DIST_DIR)"
	cp "$(RUST_TARGET_DIR)/release/$(BIN_NAME)" "$(DIST_DIR)/$(BIN_NAME)"

.PHONY: rust-run
rust-run: ## Run the binary with the version stamps (ARGS="...")
	$(RUST_VERSION_ENV) $(CARGO) run --locked $(if $(strip $(RUST_RUN_BIN)),--bin "$(RUST_RUN_BIN)") -- $(ARGS)

.PHONY: rust-test
rust-test: ## Run the Rust tests
	$(CARGO) test --workspace --locked

.PHONY: rust-clippy
rust-clippy: ## Lint the Rust workspace with clippy, warnings as errors
	$(CARGO) clippy --workspace --all-targets $(RUST_FEATURES) --locked -- -D warnings

.PHONY: rust-fmt
rust-fmt: ## Format the Rust code
	$(CARGO) fmt --all

.PHONY: rust-fmt-check
rust-fmt-check: ## Fail if the Rust code needs formatting
	$(CARGO) fmt --all --check

.PHONY: rust-doc
rust-doc: ## Check the Rust documentation, warnings as errors
	RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc --workspace $(RUST_FEATURES) --no-deps --locked

.PHONY: rust-deny
rust-deny: ## Check the Rust dependencies with cargo-deny (advisories, licences, bans, sources; network)
	$(CARGO) deny $(RUST_DENY_ARGS) check

.PHONY: verify-rust
verify-rust: rust-build rust-fmt-check rust-clippy rust-test rust-doc ## Verify the Rust code (build, fmt, clippy, tests, docs)

endif  # MK_COMMON_RUST_INCLUDED
