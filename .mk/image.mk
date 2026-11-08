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


ifndef MK_LOCAL_IMAGE_INCLUDED
MK_LOCAL_IMAGE_INCLUDED := 1

# Local snippet (NOT part of make-common): the image's prebuilt target, the one CI tests and the
# release ships. It is built from the release binaries `make dist-static` wrote, named as the
# release assets, through the `dist` build context, so the image binary is the asset byte for byte
# and the Dockerfile's build stage (dhi.io/rust) is never used. The runtime base (dhi.io/static)
# still needs a dhi.io login. `make docker-build` builds the default target, with its own Rust build.

# One platform per build, loaded into the local image store: the host's by default; another one
# needs QEMU (binfmt) to run its tests.
IMAGE_PLATFORM ?= linux/$(patsubst x86_64,amd64,$(patsubst aarch64,arm64,$(shell uname -m)))
PREBUILT_IMAGE ?= $(IMAGE_REPO):prebuilt-$(subst /,-,$(IMAGE_PLATFORM))

.PHONY: image-build-prebuilt
image-build-prebuilt: ## Build the image's prebuilt target for IMAGE_PLATFORM from DIST_DIR (make dist-static first)
	docker buildx build --load --platform "$(IMAGE_PLATFORM)" --target prebuilt \
	  --build-context dist="$(DIST_DIR)" -f "$(DOCKERFILE)" -t "$(PREBUILT_IMAGE)" \
	  --build-arg VERSION="$(DIST_VERSION)" --build-arg COMMIT="$(DIST_COMMIT)" --build-arg DATE="$(DIST_DATE)" \
	  "$(DOCKER_CONTEXT)"

# smoke (the release's), reduced (CI's arm64, under QEMU) or full (CI's amd64): scripts/image-test.sh.
IMAGE_TEST_TIER ?= full

.PHONY: image-test
image-test: ## Test the prebuilt image against its release binary (IMAGE_TEST_TIER smoke, reduced or full)
	"$(REPO_ROOT)/scripts/image-test.sh" --tier "$(IMAGE_TEST_TIER)" --image "$(PREBUILT_IMAGE)" \
	  --platform "$(IMAGE_PLATFORM)" --binary "$(call dist_asset,$(IMAGE_PLATFORM))" \
	  --expect-version "$(DIST_VERSION) $(DIST_COMMIT) $(DIST_DATE)"

endif  # MK_LOCAL_IMAGE_INCLUDED
