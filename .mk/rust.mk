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


ifndef MK_LOCAL_RUST_INCLUDED
MK_LOCAL_RUST_INCLUDED := 1

# Local snippet (NOT part of make-common): the Rust workspace. Every lint and test command runs
# workspace-wide, so the meta and testenv crates are held to the same lints and tests as the
# exporter: the lint set lives once in the root [workspace.lints], and a command scoped to one
# package would leave the others unchecked. --locked everywhere: Cargo.lock is the pin, and a
# command that rewrote it would test a dependency graph nobody reviewed. cargo deny takes
# --workspace for the same reason: the root is a package too, and cargo-deny would otherwise check
# its graph alone. Build and clippy take --all-features, so the harness code behind testenv's
# integration feature is compiled and linted like the rest; the tests do not, since that
# feature's suite needs Docker (rust-test-integration).

CARGO ?= cargo

# The version stamps (src/version.rs reads them at compile time), from the same git and clock
# values as Go's -ldflags -X; the defaults below apply only where go.mk has not set them. Only
# rust-build stamps, so the tests see the defaults. Cargo tracks the variables: going from a
# stamped build to an unstamped one recompiles the exporter crate, nothing else.
GIT_COMMIT  ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo "none")
GIT_VERSION ?= $(shell git describe --tags --dirty --always 2>/dev/null || echo "v0.0.0")
BUILD_DATE  ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")
RUST_VERSION_ENV = \
  SWARM_SCHEDULER_EXPORTER_VERSION="$(GIT_VERSION)" \
  SWARM_SCHEDULER_EXPORTER_COMMIT="$(GIT_COMMIT)" \
  SWARM_SCHEDULER_EXPORTER_DATE="$(BUILD_DATE)"

.PHONY: rust-build
rust-build: ## Build the Rust workspace, with the version stamps
	$(RUST_VERSION_ENV) $(CARGO) build --workspace --all-targets --all-features --locked

.PHONY: rust-test
rust-test: ## Run the Rust tests, meta-tests included
	$(CARGO) test --workspace --locked

# Flags for rust-run, e.g. RUST_RUN_ARGS="--containers --log-level debug".
RUST_RUN_ARGS ?=

.PHONY: rust-run
rust-run: ## Run the Rust exporter against the local Docker daemon, with the version stamps (flags in RUST_RUN_ARGS)
	$(RUST_VERSION_ENV) $(CARGO) run --locked --bin swarm-scheduler-exporter -- $(RUST_RUN_ARGS)

.PHONY: rust-clippy
rust-clippy: ## Lint the Rust workspace with clippy, warnings as errors
	$(CARGO) clippy --workspace --all-targets --all-features --locked -- -D warnings

.PHONY: rust-fmt
rust-fmt: ## Format the Rust code
	$(CARGO) fmt --all

.PHONY: rust-fmt-check
rust-fmt-check: ## Fail if the Rust code needs formatting
	$(CARGO) fmt --all --check

.PHONY: rust-deny
rust-deny: ## Check Rust dependencies with cargo-deny (advisories, licences, bans, sources; network required)
	$(CARGO) deny --workspace check

.PHONY: verify-rust
verify-rust: rust-build rust-fmt-check rust-clippy rust-test ## Verify the Rust code (build, fmt, clippy, tests)

endif  # MK_LOCAL_RUST_INCLUDED
