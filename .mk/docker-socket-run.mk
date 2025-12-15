IMAGE_NAME ?= $(BIN_NAME)

# Override DOCKER_SOCK to force a path:
#   make docker-run DOCKER_SOCK=/run/user/1000/docker.sock
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
# - Linux (GNU): stat -c %g
# - macOS/BSD:   stat -f %g
SOCK_GID := $(shell \
	if [ -S "$(DOCKER_SOCK)" ]; then \
		if stat --version >/dev/null 2>&1; then \
			stat -c %g "$(DOCKER_SOCK)"; \
		else \
			stat -f %g "$(DOCKER_SOCK)"; \
		fi; \
	fi)

# App runtime args (drop-in defaults)
DOCKER_RUN_ARGS ?= -listen-addr 0.0.0.0:8888
DOCKER_RUN_PORT ?= 8888

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
		--name "$(IMAGE_NAME)" \
		--network host \
		--group-add "$(SOCK_GID)" \
		-v "$(DOCKER_SOCK):/var/run/docker.sock:ro" \
		"$(IMAGE_REPO):$(IMAGE_TAG)" \
		$(DOCKER_RUN_ARGS)
