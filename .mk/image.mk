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
# Merged from bitgauge/.mk/image.mk at 9be3b0d20765 (GPL-3.0: the full tier's host-platform guard
# and its extra step) and swarm-device-access/.mk/image.mk at d603e59e23b8 (Apache-2.0);
# relicensed here by their author, Roberto Leinardi.
#

ifndef MK_COMMON_IMAGE_INCLUDED
MK_COMMON_IMAGE_INCLUDED := 1

ifndef MK_COMMON_DOCKER_INCLUDED
$(error image.mk needs docker.mk for DOCKER_TARGETS, IMAGE_NAME_<t> and DOCKERFILE_<t>; list docker.mk before it)
endif
ifndef MK_COMMON_RUST_RELEASE_INCLUDED
$(error image.mk needs rust-release.mk for the release assets it packages; list rust-release.mk before it)
endif

# The prebuilt images, the ones CI tests and the release ships: one per DOCKER_TARGETS entry (its
# DOCKERFILE_<t> and IMAGE_NAME_<t>, from docker.mk), each built from the release asset
# rust-release-binaries wrote, through the `dist` build context, so the image binary is the asset
# byte for byte and the Dockerfile's build stages are never used. A Dockerfile's `prebuilt` stage
# COPYs `<bin>-${TARGETOS}-${TARGETARCH}` from that context.
#
# The stamps are rust.mk's GIT_VERSION, GIT_COMMIT and BUILD_DATE, the ones
# rust-release-binaries compiles in: pass the same values to both.

# The binary an image carries: the target's own name by default (IMAGE_BIN_agent-compose := agent
# for an image that carries another target's binary).
$(foreach t,$(DOCKER_TARGETS),$(eval IMAGE_BIN_$(t) ?= $(t)))

# One platform per build, loaded into the local image store: the host's by default; another one
# needs QEMU (binfmt) to run its tests.
IMAGE_HOST_PLATFORM ?= linux/$(patsubst x86_64,amd64,$(patsubst aarch64,arm64,$(shell uname -m)))
IMAGE_PLATFORM ?= $(IMAGE_HOST_PLATFORM)
$(foreach t,$(DOCKER_TARGETS),$(eval PREBUILT_IMAGE_$(t) ?= $$(IMAGE_NAME_$(t)):prebuilt-$$(subst /,-,$$(IMAGE_PLATFORM))))

# smoke (the release's), reduced (CI's foreign platform, under QEMU) or full (CI's host platform):
# the consumer's scripts/image-test.sh, called with --tier, --image, --platform, --binary (the
# release asset the image must carry) and --expect-version. The full tier runs only on the host's
# platform; after it, IMAGE_TEST_FULL_EXTRA (make arguments: a target, and assignments that may
# use IMAGE_TEST_IMAGE, the image under test) runs on the same image; empty by default.
IMAGE_TEST_TIER ?= full
IMAGE_TEST_SCRIPT ?= $(REPO_ROOT)/scripts/image-test.sh
IMAGE_TEST_FULL_EXTRA ?=

# The full tier runs only on the host's platform: a recipe line of image-test-<t>, and the first of
# image-build-test-<t>, so a foreign full run stops before its QEMU build.
_IMAGE_TIER_GUARD = @if [ "$(IMAGE_TEST_TIER)" = full ] && [ "$(IMAGE_PLATFORM)" != "$(IMAGE_HOST_PLATFORM)" ]; then echo "the full tier runs on the host's platform ($(IMAGE_HOST_PLATFORM)), not $(IMAGE_PLATFORM)" >&2; exit 1; fi

# The stamps a build and its test share: computed once here and handed to both sub-makes, since
# rust.mk's BUILD_DATE reads the clock each time it is expanded.
_IMAGE_STAMPS = GIT_VERSION="$(GIT_VERSION)" GIT_COMMIT="$(GIT_COMMIT)" BUILD_DATE="$(BUILD_DATE)"

define _IMAGE_TEMPLATE
.PHONY: image-build-prebuilt-$(1)
image-build-prebuilt-$(1): ## Build the prebuilt image $(1) for IMAGE_PLATFORM from dist/ (make rust-release-binaries first)
	docker buildx build --load --platform "$$(IMAGE_PLATFORM)" --target prebuilt \
	  --build-context dist="$$(RUST_RELEASE_DIR)" -f "$$(DOCKERFILE_$(1))" -t "$$(PREBUILT_IMAGE_$(1))" \
	  --build-arg VERSION="$$(GIT_VERSION)" --build-arg COMMIT="$$(GIT_COMMIT)" --build-arg DATE="$$(BUILD_DATE)" \
	  "$$(DOCKER_CONTEXT)"

.PHONY: image-test-$(1)
image-test-$(1): ## Test the prebuilt image $(1) against its release asset (IMAGE_TEST_TIER smoke, reduced or full)
	$$(_IMAGE_TIER_GUARD)
	"$$(IMAGE_TEST_SCRIPT)" --tier "$$(IMAGE_TEST_TIER)" --image "$$(PREBUILT_IMAGE_$(1))" \
	  --platform "$$(IMAGE_PLATFORM)" --binary "$$(call rust_release_asset,$$(IMAGE_PLATFORM),$$(IMAGE_BIN_$(1)))" \
	  --expect-version "$$(GIT_VERSION) $$(GIT_COMMIT) $$(BUILD_DATE)"
	$$(if $$(strip $$(IMAGE_TEST_FULL_EXTRA)),@if [ "$$(IMAGE_TEST_TIER)" = full ]; then $$(MAKE) --no-print-directory $$(foreach IMAGE_TEST_IMAGE,$$(PREBUILT_IMAGE_$(1)),$$(IMAGE_TEST_FULL_EXTRA)); fi)

# Build, then test, as two make runs: under make -j, `make image-build-prebuilt-$(1)
# image-test-$(1)` could test a missing or previous image before the build has loaded the new one.
.PHONY: image-build-test-$(1)
image-build-test-$(1): ## Build the prebuilt image $(1) for IMAGE_PLATFORM, then test it (ordered, safe under make -j)
	$$(_IMAGE_TIER_GUARD)
	$$(eval _IMAGE_STAMPS_$(1) := $$(_IMAGE_STAMPS))
	$$(MAKE) --no-print-directory image-build-prebuilt-$(1) $$(_IMAGE_STAMPS_$(1))
	$$(MAKE) --no-print-directory image-test-$(1) $$(_IMAGE_STAMPS_$(1))
endef

$(foreach t,$(DOCKER_TARGETS),$(eval $(call _IMAGE_TEMPLATE,$(t))))

.PHONY: image-build-prebuilt
image-build-prebuilt: $(addprefix image-build-prebuilt-,$(DOCKER_TARGETS)) ## Build every prebuilt image for IMAGE_PLATFORM from dist/

.PHONY: image-test
image-test: $(addprefix image-test-,$(DOCKER_TARGETS)) ## Test every prebuilt image against its release asset

# Each image is built, then tested; under make -j the images run side by side, but none is tested
# before its own build.
.PHONY: image-build-test
image-build-test: $(addprefix image-build-test-,$(DOCKER_TARGETS)) ## Build, then test, every prebuilt image (ordered per image, safe under make -j)

endif  # MK_COMMON_IMAGE_INCLUDED
