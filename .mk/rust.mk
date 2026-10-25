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
# its graph alone.

CARGO ?= cargo

.PHONY: rust-build
rust-build: ## Build the Rust workspace
	$(CARGO) build --workspace --all-targets --locked

.PHONY: rust-test
rust-test: ## Run the Rust tests, meta-tests included
	$(CARGO) test --workspace --locked

.PHONY: rust-clippy
rust-clippy: ## Lint the Rust workspace with clippy, warnings as errors
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

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
