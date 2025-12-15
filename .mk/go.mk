GO        ?= go
GO_PKG    ?= ./...
GO_CMD    ?= ./cmd/app
BIN_NAME  ?= app
DIST_DIR  ?= dist

# Version metadata (generic, derived from git when available)
GIT_COMMIT ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo "none")
GIT_VERSION ?= $(shell git describe --tags --dirty --always 2>/dev/null || echo "v0.0.0")
BUILD_DATE ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")

# Reproducible-ish builds by default
export CGO_ENABLED ?= 0

# These are generic defaults, override in Makefile if your main package differs.
GO_LDFLAGS ?= -s -w \
  -X main.version=$(GIT_VERSION) \
  -X main.commit=$(GIT_COMMIT) \
  -X main.date=$(BUILD_DATE)

.PHONY: go-build
go-build: ## Build binary into $(DIST_DIR)/
	@mkdir -p "$(DIST_DIR)"
	$(GO) build -ldflags "$(GO_LDFLAGS)" -o "$(DIST_DIR)/$(BIN_NAME)" "$(GO_CMD)"

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

.PHONY: go-test
go-test: ## Run tests
	$(GO) test $(GO_PKG)

.PHONY: go-test-cover
go-test-cover: ## Run tests with coverage (coverage.out)
	$(GO) test -coverprofile=coverage.out $(GO_PKG)
