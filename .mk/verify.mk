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

ifndef MK_LOCAL_VERIFY_INCLUDED
MK_LOCAL_VERIFY_INCLUDED := 1

# Local snippet (NOT part of make-common): one command that verifies the Go and the Rust code the
# way CI does. verify-rust lives in rust.mk.
#
# go-vet sees only untagged files. The integration-tagged harness and suite are vetted on their
# own, so a Go change that breaks them fails here and not first on a machine with Docker; running
# them needs Docker with privileged containers, so go-test-integration stays a separate target.
# go-lint is make-common's: golangci-lint through the pre-commit hook, at the hook's pinned version.

.PHONY: verify-go
verify-go: go-build go-test go-vet go-vet-integration go-lint ## Verify the Go code (build, race tests, vet untagged and integration-tagged, lint)

.PHONY: verify
verify: verify-go verify-rust ## Verify everything (the Go and the Rust code)

endif  # MK_LOCAL_VERIFY_INCLUDED
