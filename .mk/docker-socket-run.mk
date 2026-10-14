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

# The exporter writes nothing to its filesystem and needs no Linux capability (it only reads the
# Docker API through the socket its group may open), so the local run gets a read-only root, every
# capability dropped and no privilege gain through setuid binaries.
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
		--read-only \
		--cap-drop ALL \
		--security-opt no-new-privileges \
		--group-add "$(SOCK_GID)" \
		-v "$(DOCKER_SOCK):/var/run/docker.sock:ro" \
		"$(IMAGE_REPO):$(IMAGE_TAG)" \
		$(DOCKER_RUN_ARGS)
