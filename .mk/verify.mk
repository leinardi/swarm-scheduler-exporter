ifndef MK_LOCAL_VERIFY_INCLUDED
MK_LOCAL_VERIFY_INCLUDED := 1

# Local snippet (NOT part of make-common): one command that verifies the Go code the way CI does.
#
# go-vet sees only untagged files. The integration-tagged harness and suite are vetted on their
# own, so a Go change that breaks them fails here and not first on a machine with Docker; running
# them needs Docker with privileged containers, so go-test-integration stays a separate target.
# go-lint is make-common's: golangci-lint through the pre-commit hook, at the hook's pinned version.

.PHONY: verify-go
verify-go: go-build go-test go-vet go-vet-integration go-lint ## Verify the Go code (build, race tests, vet untagged and integration-tagged, lint)

.PHONY: verify
verify: verify-go ## Verify everything (the Go code)

endif  # MK_LOCAL_VERIFY_INCLUDED
