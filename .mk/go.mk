#
# MIT License
#
# Copyright (c) 2025 Roberto Leinardi
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

ifndef MK_COMMON_GO_INCLUDED
MK_COMMON_GO_INCLUDED := 1

GO       ?= go
GO_PKG   ?= ./...
GO_CMD   ?= ./cmd/app
BIN_NAME ?= app
DIST_DIR ?= dist

# GO_BINARIES: space-separated list of binary names to build.
# Default is a single binary named $(BIN_NAME) built from $(GO_CMD).
# Multi-binary projects set GO_BINARIES and per-binary GO_CMD_<name>:
#   GO_BINARIES := controller agent
#   GO_CMD_controller := ./cmd/controller
#   GO_CMD_agent      := ./cmd/agent
GO_BINARIES        ?= $(BIN_NAME)
GO_CMD_$(BIN_NAME) ?= $(GO_CMD)

# Version metadata (generic, derived from git when available)
GIT_COMMIT  ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo "none")
GIT_VERSION ?= $(shell git describe --tags --dirty --always 2>/dev/null || echo "v0.0.0")
BUILD_DATE  ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")

# Reproducible-ish builds by default
export CGO_ENABLED ?= 0

# These are generic defaults, override in Makefile if your main package differs.
GO_LDFLAGS ?= -s -w \
  -X main.version=$(GIT_VERSION) \
  -X main.commit=$(GIT_COMMIT) \
  -X main.date=$(BUILD_DATE)

define _GO_BUILD_TEMPLATE
.PHONY: go-build-$(1)
go-build-$(1): ## Build $(1) into $$(DIST_DIR)/
	@mkdir -p "$$(DIST_DIR)"
	CGO_ENABLED=$$(CGO_ENABLED) $$(GO) build \
	  -ldflags "$$(GO_LDFLAGS)" \
	  -o "$$(DIST_DIR)/$(1)" "$$(GO_CMD_$(1))"
endef

$(foreach b,$(GO_BINARIES),$(eval $(call _GO_BUILD_TEMPLATE,$(b))))

.PHONY: go-build
go-build: $(addprefix go-build-,$(GO_BINARIES)) ## Build all configured binaries into $(DIST_DIR)/

.PHONY: go-run
go-run: ## Run the app (ARGS="...")
	$(GO) run -ldflags "$(GO_LDFLAGS)" "$(GO_CMD)" $(ARGS)

.PHONY: go-clean
go-clean: ## Remove build/test artifacts
	@rm -rf "$(DIST_DIR)" coverage.out coverage.html

.PHONY: go-tidy
go-tidy: ## Ensure go.mod/go.sum are tidy and verified
	$(GO) mod tidy
	$(GO) mod verify

.PHONY: go-fmt
go-fmt: ## Format code (gofmt + goimports if available)
	@$(GO) fmt $(GO_PKG)
	@dirs="$$( $(GO) list -f '{{.Dir}}' $(GO_PKG) 2>/dev/null )"; \
	gofmt -w $$dirs; \
	if command -v goimports >/dev/null 2>&1; then goimports -w $$dirs; fi

.PHONY: go-fmt-check
go-fmt-check: ## Fail if formatting is needed
	@dirs="$$( $(GO) list -f '{{.Dir}}' $(GO_PKG) 2>/dev/null )"; \
	out="$$(gofmt -l $$dirs)"; \
	if [ -n "$$out" ]; then \
	  echo "Formatting needed:"; echo "$$out"; \
	  echo "Run: make go-fmt"; \
	  exit 1; \
	fi; \
	if command -v goimports >/dev/null 2>&1; then \
	  out="$$(goimports -l $$dirs)"; \
	  if [ -n "$$out" ]; then \
	    echo "goimports needed:"; echo "$$out"; \
	    echo "Run: make go-fmt"; \
	    exit 1; \
	  fi; \
	fi

.PHONY: go-vet
go-vet: ## Static checks (go vet)
	$(GO) vet $(GO_PKG)

# Lint through the same pinned golangci-lint the pre-commit hook runs, over the whole module, so
# `make go-lint`, the commit hook and CI cannot disagree on the linter version or its
# configuration. GO_LINT_HOOK is the hook id in the consumer's .pre-commit-config.yaml (the
# golangci/golangci-lint repository's whole-module hook). Self-contained rather than depending on
# pre-commit.mk's check-installed-pre-commit, so go.mk still works on its own.
GO_LINT_HOOK ?= golangci-lint-full

.PHONY: go-lint
go-lint: ## Lint all Go code with golangci-lint (pre-commit pin)
	@if ! command -v pre-commit >/dev/null 2>&1; then \
	  echo "Error: pre-commit is not installed (go-lint runs golangci-lint through it)."; \
	  echo "Install it with: pipx install pre-commit (or brew/apt install pre-commit)"; \
	  exit 1; \
	fi
	@pre-commit run "$(GO_LINT_HOOK)" --all-files

.PHONY: go-test
go-test: ## Run tests with race detector
	CGO_ENABLED=1 $(GO) test -race $(GO_PKG)

.PHONY: go-test-cover
go-test-cover: ## Run tests with coverage (coverage.out)
	$(GO) test -coverprofile=coverage.out $(GO_PKG)

endif  # MK_COMMON_GO_INCLUDED
