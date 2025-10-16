# Default goal
.DEFAULT_GOAL := help

# ==== Project vars ====
GO        ?= go
PKG       := ./...                          # all packages
CMD_DIR   := ./cmd/swarm-scheduler-exporter     # main package
BIN_NAME  := swarm-scheduler-exporter
DIST_DIR  := dist

# Derive version info from git (fallbacks provided)
GIT_COMMIT := $(shell git rev-parse --short HEAD 2>/dev/null || echo "none")
GIT_TAG    := $(shell git describe --tags --abbrev=0 2>/dev/null || echo "v0.0.0")
BUILD_DATE := $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")

LDFLAGS := -s -w \
  -X 'main.version=$(GIT_TAG)' \
  -X 'main.commit=$(GIT_COMMIT)' \
  -X 'main.date=$(BUILD_DATE)'

# Helpful env for reproducible builds
export CGO_ENABLED ?= 0

# ==== Helpers ====
.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nTargets:\n"} /^[a-zA-Z0-9_\-\/]+:.*##/ { printf "  \033[36m%-25s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# ==== Go tasks ====
.PHONY: build
build: ## Build the binary into ./dist
	@mkdir -p $(DIST_DIR)
	$(GO) build -ldflags "$(LDFLAGS)" -o $(DIST_DIR)/$(BIN_NAME) $(CMD_DIR)

.PHONY: run
run: ## Run the app (like go run)
	$(GO) run -ldflags "$(LDFLAGS)" $(CMD_DIR) -containers -log-level debug

.PHONY: clean
clean: ## Remove build artifacts
	@rm -rf $(DIST_DIR) coverage.out coverage.html

.PHONY: tidy
tidy: ## Ensure go.mod/go.sum are tidy
	$(GO) mod tidy
	$(GO) mod verify

.PHONY: fmt
fmt: ## Format code (go fmt & goimports if available)
	$(GO) fmt $(PKG)
	@if command -v goimports >/dev/null 2>&1; then goimports -w .; fi

.PHONY: fmt-check
fmt-check: ## Fail if formatting is needed
	@diff <(echo -n) <($(GO) fmt -n $(PKG) 2>&1 | sed 's/^/FMT: /') || (echo "Run 'make fmt' to format"; exit 1)

.PHONY: vet
vet: ## Static checks (go vet)
	$(GO) vet $(PKG)

.PHONY: check
check: check-installed-pre-commit ## Run all pre-commit hooks on every file in the repository
	pre-commit run --all-files

.PHONY: check-stage
check-stage: check-installed-pre-commit ## Run pre-commit hooks only on files staged in git
	@echo "Running pre-commit on current staging area..."
	pre-commit run

.PHONY: check-master-changes
check-master-changes: check-installed-pre-commit ## Run pre-commit hooks only on commits made since master/origin/master
	@REF=; \
	if git rev-parse --verify --quiet origin/master >/dev/null && \
	   git merge-base --is-ancestor origin/master HEAD; then \
	  REF=origin/master; \
	elif git rev-parse --verify --quiet master >/dev/null && \
	     git merge-base --is-ancestor master HEAD; then \
	  REF=master; \
	else \
	  echo "Error: neither origin/master nor master is an ancestor of HEAD." >&2; \
	  exit 1; \
	fi; \
	echo "Running pre-commit on new commits since $$REF..."; \
	echo "pre-commit run --from-ref $$REF --to-ref HEAD"; \
	pre-commit run --from-ref $$REF --to-ref HEAD

.PHONY: shellcheck
shellcheck: check-installed-pre-commit check-installed-shellcheck ## Lint all shell scripts using shellcheck via pre-commit
	@echo "Running shellcheck..."
	pre-commit run shellcheck --all-files

.PHONY: prettier
prettier: check-installed-pre-commit ## Format all YAML files with Prettier via pre-commit
	pre-commit run prettier-yaml --all-files

.PHONY: actionlint
actionlint: check-installed-pre-commit ## Lint all GitHub Actions workflows using actionlint via pre-commit
	@echo "Running actionlint..."
	pre-commit run actionlint --all-files

.PHONY: yamllint
yamllint: check-installed-pre-commit ## Validate YAML syntax and style using yamllint via pre-commit
	@echo "Running yamllint..."
	pre-commit run yamllint --all-files

.PHONY: check-installed-shellcheck
check-installed-shellcheck: ## Verify that shellcheck is installed (prints install instructions if missing)
	@if ! command -v shellcheck >/dev/null 2>&1; then \
		echo "Error: shellcheck is not installed."; \
		echo "Install it with:"; \
		echo "  macOS: brew install shellcheck"; \
		echo "  Ubuntu: sudo apt-get install -y shellcheck"; \
		exit 1; \
	fi

.PHONY: check-installed-prettier
check-installed-prettier: ## Verify that prettier is installed (prints install instructions if missing)
	@if ! command -v prettier >/dev/null 2>&1; then \
		echo "Error: prettier is not installed."; \
		echo "Install it with:"; \
		echo "  macOS: brew install prettier"; \
		echo "  Ubuntu: npm install --global prettier"; \
		exit 1; \
	fi

.PHONY: check-installed-pre-commit
check-installed-pre-commit: ## Verify that pre-commit is installed (prints install instructions if missing)
	@if ! command -v pre-commit >/dev/null 2>&1; then \
		echo "Error: pre-commit is not installed."; \
		echo "Install it with:"; \
		echo "  macOS: brew install pre-commit"; \
		echo "  Ubuntu: pip install pre-commit"; \
		exit 1; \
	fi

.PHONY: install-pre-commit
install-pre-commit: check-installed-pre-commit ## Install the git pre-commit hook into .git/hooks
	@echo "Setting up pre-commit..."
	pre-commit install

.PHONY: password
password: ## Generate a random 99-character password (PostgreSQL-compatible charset)
	@echo "Generating a 99-character password (PostgreSQL compatible)..."
	LC_ALL=C tr -dc '[[:alnum:]_.+=,-]' </dev/urandom | head -c 99; echo

# ==== Docker image vars ====
IMAGE_NAME ?= swarm-scheduler-exporter
IMAGE_TAG  ?= $(GIT_TAG)   # e.g. v0.4.0
IMAGE_REPO ?= $(IMAGE_NAME)

# Enable BuildKit for caching and smaller layers
export DOCKER_BUILDKIT ?= 1

.PHONY: docker-build
docker-build: ## Build the Docker image locally (same versioning as Go build)
	@echo "Building image $(IMAGE_REPO):$(IMAGE_TAG)"
	docker build \
		--file deployments/docker/Dockerfile \
		--tag $(IMAGE_REPO):$(IMAGE_TAG) \
		--build-arg VERSION=$(GIT_TAG) \
		--build-arg COMMIT=$(GIT_COMMIT) \
		--build-arg DATE=$(BUILD_DATE) \
		.

# ==== Docker runtime detection ====
# Override DOCKER_SOCK to force a path: `make docker-run DOCKER_SOCK=/run/user/1000/docker.sock`
DOCKER_SOCK ?= $(shell \
	if [ -S "/var/run/docker.sock" ]; then \
		echo "/var/run/docker.sock"; \
	elif [ -S "$$HOME/.colima/default/docker.sock" ]; then \
		echo "$$HOME/.colima/default/docker.sock"; \
	elif [ -S "$$HOME/.docker/run/docker.sock" ]; then \
		echo "$$HOME/.docker/run/docker.sock"; \
	elif [ -n "$$XDG_RUNTIME_DIR" ] && [ -S "$$XDG_RUNTIME_DIR/docker.sock" ]; then \
		echo "$$XDG_RUNTIME_DIR/docker.sock"; \
	else \
		echo "/var/run/docker.sock"; \
	fi)

# Cross-platform "stat" to get the socket group id:
# - Linux: stat -c %g
# - macOS/BSD: stat -f %g
SOCK_GID := $(shell \
	if [ -S "$(DOCKER_SOCK)" ]; then \
		if stat --version >/dev/null 2>&1; then \
			stat -c %g "$(DOCKER_SOCK)"; \
		else \
			stat -f %g "$(DOCKER_SOCK)"; \
		fi; \
	fi)

.PHONY: docker-run
docker-run: ## Run the image locally binding the Docker socket and metrics port
	@if [ ! -S "$(DOCKER_SOCK)" ]; then \
		echo "ERROR: Docker socket not found at '$(DOCKER_SOCK)'."; \
		echo "       Override with: make docker-run DOCKER_SOCK=/path/to/docker.sock"; \
		exit 1; \
	fi
	@if [ -z "$(SOCK_GID)" ]; then \
		echo "ERROR: Could not determine group id for '$(DOCKER_SOCK)'."; \
		exit 1; \
	fi
	@echo "Using Docker socket: $(DOCKER_SOCK) (gid=$(SOCK_GID))"
	docker run --rm \
		--name $(IMAGE_NAME) \
		--network host \
		--group-add $(SOCK_GID) \
		-v $(DOCKER_SOCK):/var/run/docker.sock:ro \
		$(IMAGE_REPO):$(IMAGE_TAG) \
		-listen-addr 0.0.0.0:8888

.PHONY: docker-build-latest
docker-build-latest: ## Build and tag as :latest (in addition to version tag)
	$(MAKE) docker-build
	docker tag $(IMAGE_REPO):$(IMAGE_TAG) $(IMAGE_REPO):latest
