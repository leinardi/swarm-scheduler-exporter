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

ifndef MK_COMMON_DOCKER_INCLUDED
MK_COMMON_DOCKER_INCLUDED := 1

# Prefer values from the Makefile; provide safe defaults.
IMAGE_NAME ?= $(BIN_NAME)
IMAGE_REPO ?= $(IMAGE_NAME)
IMAGE_TAG  ?= $(GIT_VERSION)

DOCKERFILE     ?= deployments/docker/Dockerfile
DOCKER_CONTEXT ?= .
DOCKER_BUILD_CMD ?= docker buildx build

export DOCKER_BUILDKIT ?= 1

# DOCKER_TARGETS: space-separated list of target names to build images for.
# Default is a single image using $(IMAGE_REPO) and $(DOCKERFILE).
# Multi-image projects set DOCKER_TARGETS and per-target IMAGE_NAME_<t> / DOCKERFILE_<t>:
#   DOCKER_TARGETS        := controller agent
#   IMAGE_NAME_controller := ghcr.io/org/app-controller
#   IMAGE_NAME_agent      := ghcr.io/org/app-agent
#   DOCKERFILE_controller := deployments/docker/controller.Dockerfile
#   DOCKERFILE_agent      := deployments/docker/agent.Dockerfile
DOCKER_TARGETS         ?= $(BIN_NAME)
IMAGE_NAME_$(BIN_NAME) ?= $(IMAGE_REPO)
DOCKERFILE_$(BIN_NAME) ?= $(DOCKERFILE)

# Optional build args (safe even if Dockerfile ignores them)
DOCKER_BUILD_ARGS ?= \
  --build-arg VERSION=$(GIT_VERSION) \
  --build-arg COMMIT=$(GIT_COMMIT) \
  --build-arg DATE=$(BUILD_DATE)

define _DOCKER_BUILD_TEMPLATE
.PHONY: docker-build-$(1)
docker-build-$(1): ## Build Docker image for $(1)
	@echo "Building $$(IMAGE_NAME_$(1)):$$(IMAGE_TAG)"
	$$(DOCKER_BUILD_CMD) \
	  -f "$$(DOCKERFILE_$(1))" \
	  -t "$$(IMAGE_NAME_$(1)):$$(IMAGE_TAG)" \
	  $$(DOCKER_BUILD_ARGS) \
	  "$$(DOCKER_CONTEXT)"

.PHONY: docker-tag-latest-$(1)
docker-tag-latest-$(1): docker-build-$(1) ## Tag $(1) image also as latest
	docker tag "$$(IMAGE_NAME_$(1)):$$(IMAGE_TAG)" "$$(IMAGE_NAME_$(1)):latest"
endef

$(foreach t,$(DOCKER_TARGETS),$(eval $(call _DOCKER_BUILD_TEMPLATE,$(t))))

.PHONY: docker-build
docker-build: $(addprefix docker-build-,$(DOCKER_TARGETS)) ## Build all configured Docker images

.PHONY: docker-tag-latest
docker-tag-latest: $(addprefix docker-tag-latest-,$(DOCKER_TARGETS)) ## Tag all images also as latest

endif  # MK_COMMON_DOCKER_INCLUDED
