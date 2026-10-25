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

# Local snippet (NOT part of make-common): check every Rust dependency against the RustSec
# advisory database, with cargo-deny as configured in deny.toml. It is not reachability-aware: any
# advisory against a crate in the graph counts, which is why every ignore in deny.toml carries a
# reason. It needs the network for the database, so it runs when Cargo.toml, Cargo.lock or
# deny.toml change (the cargo-deny hook, with every check), on every pull request (CI's rust job)
# and every week (scheduled-scan), and deliberately here. --workspace: the root is a package as
# well as the workspace, and cargo-deny would otherwise check its graph alone. CARGO comes from
# rust.mk.

.PHONY: audit-deps
audit-deps: ## Check every Rust dependency against the RustSec advisories (cargo-deny; network required)
	$(CARGO) deny --workspace check advisories

endif  # MK_LOCAL_AUDIT_DEPS_INCLUDED
