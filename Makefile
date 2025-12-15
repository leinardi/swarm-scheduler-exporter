# Resolve repository root (Makefile can live anywhere)
REPO_ROOT := $(shell git rev-parse --show-toplevel 2>/dev/null || pwd)

MK_COMMON_REPO        ?= leinardi/make-common
MK_COMMON_VERSION     ?= v1

MK_COMMON_DIR         := $(REPO_ROOT)/.mk

# Shared snippets coming from make-common
MK_COMMON_FILES       := docker.mk help.mk go.mk password.mk pre-commit.mk

# Repo-local snippets that are NOT in make-common
MK_LOCAL_FILES        := docker-socket-run.mk

MK_COMMON_BOOTSTRAP_SCRIPT := $(REPO_ROOT)/scripts/bootstrap-mk-common.sh

# Bootstrap: the script will self-update and fetch the selected .mk snippets
MK_COMMON_BOOTSTRAP := $(shell "$(MK_COMMON_BOOTSTRAP_SCRIPT)" \
  "$(MK_COMMON_REPO)" \
  "$(MK_COMMON_VERSION)" \
  "$(MK_COMMON_DIR)" \
  "$(MK_COMMON_FILES)")

# -----------------------------------------------------------------------------
# Project-specific config
# -----------------------------------------------------------------------------
BIN_NAME     ?= swarm-scheduler-exporter
GO_CMD       ?= ./cmd/swarm-scheduler-exporter
GO_PKG       ?= ./...
DIST_DIR     ?= dist

# Docker build settings
DOCKERFILE     ?= deployments/docker/Dockerfile
DOCKER_CONTEXT ?= .

IMAGE_NAME   ?= $(BIN_NAME)
IMAGE_REPO   ?= $(IMAGE_NAME)
IMAGE_TAG    ?= $(GIT_VERSION)

# Drop-in runtime defaults (matches old `run`/`docker-run`)
ARGS ?= -containers -log-level debug
DOCKER_RUN_ARGS ?= -listen-addr 0.0.0.0:8888

# -----------------------------------------------------------------------------
# Include shared make logic (fetched from make-common)
# -----------------------------------------------------------------------------
include $(addprefix $(MK_COMMON_DIR)/,$(MK_COMMON_FILES))

# -----------------------------------------------------------------------------
# Include repo-local logic (no bootstrap; lives only in this repo)
# -----------------------------------------------------------------------------
-include $(addprefix $(REPO_ROOT)/.mk/,$(MK_LOCAL_FILES))

.PHONY: mk-common-update
mk-common-update: ## Check for remote updates of shared .mk files
	@echo "[mk] Checking for updates from $(MK_COMMON_REPO)@$(MK_COMMON_VERSION)"
	MK_COMMON_UPDATE=1 "$(MK_COMMON_BOOTSTRAP_SCRIPT)" \
	  "$(MK_COMMON_REPO)" \
	  "$(MK_COMMON_VERSION)" \
	  "$(MK_COMMON_DIR)" \
	  "$(MK_COMMON_FILES)"
