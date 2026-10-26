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

# Local snippet (NOT part of make-common): the integration suite (the testenv crate, behind its
# integration feature) against a throwaway multi-node Swarm made of privileged docker:dind
# containers on the host daemon, running the release binary.
#
# Prerequisites: a Docker daemon that allows privileged containers. The host may itself be a Swarm
# node: the test Swarm lives entirely inside the DinD containers.
#
# RUN goes to the runner's --run, which reads it with the -run semantics of Go's test runner (a
# regular expression per /-separated level, unanchored) and fails when it selects nothing; the
# runner prints the selected count. The suite deadline is SSE_IT_SUITE_TIMEOUT (default 12m).
#
# Select tests:             make test-integration RUN='^TestNode_'
# Rewrite the golden file:  make test-integration UPDATE=1 RUN=TestSnapshot_

# The binary under test: the release build below, unless SSE_IT_BINARY is set.
export SSE_IT_BINARY ?= $(REPO_ROOT)/target/release/$(BIN_NAME)

.PHONY: test-integration
test-integration: ## Run the DinD Swarm integration suite against the release binary (requires Docker with privileged containers)
	$(RUST_VERSION_ENV) $(CARGO) build --locked --release --bin $(BIN_NAME)
	$(if $(filter 1,$(UPDATE)),SSE_IT_UPDATE=1,) \
	  $(CARGO) test --locked -p testenv --features integration --test integration -- --run '$(RUN)'

# Everything the suite creates carries the envid label, so this removes every leak, whatever its
# age, without touching anything else on the host.
.PHONY: sweep-test-leaks
sweep-test-leaks: ## Remove every labelled integration-test container and network, whatever its age
	docker ps -aq --filter "label=swarm-scheduler-exporter.it.envid" | xargs -r docker rm -fv
	docker network ls -q --filter "label=swarm-scheduler-exporter.it.envid" | xargs -r docker network rm
