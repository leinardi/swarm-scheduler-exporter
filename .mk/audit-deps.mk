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
# The version is pinned so two runs mean the same thing. Bump it deliberately.

GOVULNCHECK_VERSION ?= v1.8.0
GOVULNCHECK        ?= golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION)

.PHONY: audit-deps
audit-deps: audit-deps-go ## Scan Go dependencies for known vulnerabilities (network required)

.PHONY: audit-deps-go
audit-deps-go: ## Run the pinned govulncheck over every Go package
	$(GO) run $(GOVULNCHECK) ./...

endif  # MK_LOCAL_AUDIT_DEPS_INCLUDED
