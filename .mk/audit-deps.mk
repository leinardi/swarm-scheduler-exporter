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

ifndef MK_LOCAL_AUDIT_DEPS_INCLUDED
MK_LOCAL_AUDIT_DEPS_INCLUDED := 1

# Local snippet (NOT part of make-common): check the dependencies this repository ships against
# known vulnerabilities, with the tool the Go ecosystem publishes.
#
# What this is and is not. govulncheck is source-aware: it reports a vulnerable module only when
# the build actually reaches the affected symbol, so it answers "is this repository exposed", not
# "does a vulnerable version appear in the graph".
#
# It needs the network, and govulncheck over every package is too slow to run on every Go change.
# This target is the one that is run deliberately, whenever go.mod or go.sum change.
#
# -test and -tags=integration scan the test files and the integration harness too: they run on
# developer machines and in CI, and a vulnerable dependency they reach is reached there.
#
# The version is pinned so two runs mean the same thing. Bump it deliberately.

GOVULNCHECK_VERSION ?= v1.8.0
GOVULNCHECK        ?= golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION)

.PHONY: audit-deps
audit-deps: audit-deps-go audit-deps-rust ## Scan Go and Rust dependencies for known vulnerabilities (network required)

.PHONY: audit-deps-go
audit-deps-go: ## Run the pinned govulncheck over every Go package, tests included
	$(GO) run $(GOVULNCHECK) -test -tags=integration ./...

# The Rust side is checked against the RustSec advisory database by cargo-deny, configured in
# deny.toml. Unlike govulncheck it is not reachability-aware: any advisory against a crate in the
# graph counts, which is why every ignore in deny.toml carries a reason. CARGO comes from rust.mk.
.PHONY: audit-deps-rust
audit-deps-rust: ## Check every Rust dependency against the RustSec advisories (cargo-deny)
	$(CARGO) deny --workspace check advisories

endif  # MK_LOCAL_AUDIT_DEPS_INCLUDED
