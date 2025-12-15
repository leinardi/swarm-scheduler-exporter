# Prefer values from the Makefile; provide safe defaults.
IMAGE_NAME ?= $(BIN_NAME)
IMAGE_REPO ?= $(IMAGE_NAME)
IMAGE_TAG  ?= $(GIT_VERSION)

DOCKERFILE ?= deployments/docker/Dockerfile
DOCKER_CONTEXT ?= .

export DOCKER_BUILDKIT ?= 1

# Optional build args (safe even if Dockerfile ignores them)
DOCKER_BUILD_ARGS ?= \
  --build-arg VERSION=$(GIT_VERSION) \
  --build-arg COMMIT=$(GIT_COMMIT) \
  --build-arg DATE=$(BUILD_DATE)

.PHONY: docker-build
docker-build: ## Build Docker image locally: $(IMAGE_REPO):$(IMAGE_TAG)
	@echo "Building image $(IMAGE_REPO):$(IMAGE_TAG)"
	docker build \
	  -f "$(DOCKERFILE)" \
	  -t "$(IMAGE_REPO):$(IMAGE_TAG)" \
	  $(DOCKER_BUILD_ARGS) \
	  "$(DOCKER_CONTEXT)"

.PHONY: docker-tag-latest
docker-tag-latest: docker-build ## Tag built image also as :latest
	docker tag "$(IMAGE_REPO):$(IMAGE_TAG)" "$(IMAGE_REPO):latest"
