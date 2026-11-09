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


ifndef MK_LOCAL_DIST_INCLUDED
MK_LOCAL_DIST_INCLUDED := 1

# Local snippet (NOT part of make-common): the release binaries. CI and the release build them with
# this one target, so CI tests the packaging path the release ships: static musl binaries,
# cross-compiled by cargo-zigbuild with zig as the C compiler and linker, named as the release
# assets (dist/swarm-scheduler-exporter-linux-<arch>). The image's prebuilt target COPYs exactly these
# files (`make image-build-prebuilt`).
#
# Reproducible: the pinned toolchain (rust-toolchain.toml), zig and cargo-zigbuild, Cargo.lock
# (--locked), the checkout and the cargo home remapped out of the binary, and stamps that come from
# the commit. A rebuild of the same commit with the same stamps produces the same bytes, which is
# what lets a release recovery check a published asset by sha256.

# The platforms to build, as os/arch.
DIST_PLATFORMS ?= linux/amd64 linux/arm64

# The stamps compiled in (src/version.rs): the release passes the version, the commit's first seven
# characters and its commit date; the defaults are rust.mk's.
DIST_VERSION ?= $(GIT_VERSION)
DIST_COMMIT  ?= $(GIT_COMMIT)
DIST_DATE    ?= $(BUILD_DATE)

DIST_CARGO_HOME ?= $(or $(CARGO_HOME),$(HOME)/.cargo)

# The Rust target of a platform; an unknown platform stops make.
dist_triple = $(or $(DIST_TRIPLE_$(subst /,_,$(1))),$(error no Rust target for the platform $(1)))
DIST_TRIPLE_linux_amd64 := x86_64-unknown-linux-musl
DIST_TRIPLE_linux_arm64 := aarch64-unknown-linux-musl

# The release asset of a platform.
dist_asset = $(DIST_DIR)/$(BIN_NAME)-$(subst /,-,$(1))

define _DIST_STATIC
	$(RUST_STAMP_PREFIX)_VERSION="$(DIST_VERSION)" \
	  $(RUST_STAMP_PREFIX)_COMMIT="$(DIST_COMMIT)" \
	  $(RUST_STAMP_PREFIX)_DATE="$(DIST_DATE)" \
	  RUSTFLAGS="--remap-path-prefix=$(REPO_ROOT)=/src --remap-path-prefix=$(DIST_CARGO_HOME)=/cargo" \
	  $(CARGO) zigbuild --release --locked --target "$(call dist_triple,$(1))" --bin "$(BIN_NAME)"
	cp "$(RUST_TARGET_DIR)/$(call dist_triple,$(1))/release/$(BIN_NAME)" "$(call dist_asset,$(1))"

endef

.PHONY: dist-static
dist-static: ## Build the static release binaries for DIST_PLATFORMS into DIST_DIR (cargo-zigbuild and zig)
	@mkdir -p "$(DIST_DIR)"
	$(foreach platform,$(DIST_PLATFORMS),$(call _DIST_STATIC,$(platform)))

# Over every shipped platform's asset, whichever run built it: the release regenerates it after
# adding the assets a recovery took from what is already published.
DIST_CHECKSUM_PLATFORMS ?= linux/amd64 linux/arm64

.PHONY: dist-checksums
dist-checksums: ## Write DIST_DIR/checksums.txt over the release binaries
	cd "$(DIST_DIR)" && sha256sum $(foreach platform,$(DIST_CHECKSUM_PLATFORMS),"$(notdir $(call dist_asset,$(platform)))") > checksums.txt

endif  # MK_LOCAL_DIST_INCLUDED
