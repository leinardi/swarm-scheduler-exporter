#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
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

# Local snippet (NOT part of make-common): integration tests against a throwaway multi-node Swarm
# made of privileged docker:dind containers on the host daemon. The tests carry
# //go:build integration and run the exporter binary built by go-build against that Swarm.
#
# Prerequisites: a Docker daemon that allows privileged containers. The host may itself be a Swarm
# node: the test Swarm lives entirely inside the DinD containers.
#
# Select tests:              make go-test-integration RUN='^TestNode_'
# Rewrite the golden files:  make go-test-integration UPDATE=1 RUN=TestSnapshot_
# Change the go test timeout: make go-test-integration INTEGRATION_TIMEOUT=20m

INTEGRATION_TIMEOUT ?= 15m
INTEGRATION_PKG     ?= ./test/integration/...

# go test changes the working directory to the package dir, so the binary path must be absolute.
export SSE_IT_BINARY ?= $(REPO_ROOT)/$(DIST_DIR)/$(BIN_NAME)

.PHONY: go-test-integration
go-test-integration: go-build ## Run the DinD Swarm integration tests (requires Docker with privileged containers)
	$(GO) test \
	  -tags=integration \
	  -timeout=$(INTEGRATION_TIMEOUT) \
	  -v \
	  $(if $(RUN),-run '$(RUN)',) \
	  $(INTEGRATION_PKG) \
	  $(if $(filter 1,$(UPDATE)),-args -update,)

.PHONY: go-vet-integration
go-vet-integration: ## Static checks including the integration-tagged packages
	$(GO) vet -tags=integration $(GO_PKG)

.PHONY: sweep-test-leaks
sweep-test-leaks: ## Remove every labelled integration-test container and network, whatever its age
	docker ps -aq --filter "label=swarm-scheduler-exporter.it.envid" | xargs -r docker rm -fv
	docker network ls -q --filter "label=swarm-scheduler-exporter.it.envid" | xargs -r docker network rm
