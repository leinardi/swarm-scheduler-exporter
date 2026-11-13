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
# Merged from bitgauge/.mk/rust-release.mk at 9be3b0d20765 (GPL-3.0, its rust-zigbuild-install)
# and swarm-device-access/.mk/rust-release.mk at d603e59e23b8 (Apache-2.0), themselves adapted from
# swarm-scheduler-exporter (MIT); relicensed here by their author, Roberto Leinardi.
#

ifndef MK_COMMON_RUST_RELEASE_INCLUDED
MK_COMMON_RUST_RELEASE_INCLUDED := 1

ifndef MK_COMMON_RUST_INCLUDED
$(error rust-release.mk needs rust.mk (CARGO, BIN_NAME, DIST_DIR, RUST_VERSION_ENV): list rust.mk before it)
endif

# The static musl builds of the release binaries: one cargo-zigbuild run per platform builds every
# binary in RUST_RELEASE_BINS, and each lands in RUST_RELEASE_DIR under its release asset's name,
# <bin>-<os>-<arch>. CI and the release build with the same target, rust-release-binaries, so CI
# tests the packaging path the release ships; image.mk's prebuilt images COPY exactly these files.

# The binaries to build and ship, each a `--bin` of the workspace: BIN_NAME alone by default.
RUST_RELEASE_BINS ?= $(BIN_NAME)
ifeq ($(strip $(RUST_RELEASE_BINS)),)
$(error RUST_RELEASE_BINS is empty; name at least one binary, or leave it unset for BIN_NAME)
endif

# Targets run once before the builds, such as a web build every binary embeds (web-build); none
# by default.
RUST_RELEASE_PREREQS ?=

# The release toolchain beyond rust-toolchain.toml, each archive by its sha256: zig from
# https://ziglang.org/download/index.json, cargo-zigbuild from the release asset's .sha256, rust-std
# from the .sha256 static.rust-lang.org publishes beside each archive. A consumer whose meta-tests
# or Dockerfile pin other values sets them before the include. CI installs the x86_64 zig and
# cargo-zigbuild with gh-composite-actions' setup-zigbuild at these versions; an image build stage
# downloads the same archives (and the rust-std ones; on a host, rustup installs the musl targets
# rust-toolchain.toml lists). cargo-zigbuild publishes no aarch64 musl asset, so an arm64 build host
# takes the gnu one.
RUST_RELEASE_ZIG_VERSION ?= 0.17.0
RUST_RELEASE_ZIG_SHA256_x86_64 ?= 1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
RUST_RELEASE_ZIG_SHA256_aarch64 ?= 9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8
RUST_RELEASE_CARGO_ZIGBUILD_VERSION ?= 0.24.0
RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl ?= 52be2cb85ad3875682564a29bac1832b2ab3db68b1c504765c92d5249cc9b96f
RUST_RELEASE_CARGO_ZIGBUILD_SHA256_aarch64-unknown-linux-gnu ?= de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c
RUST_RELEASE_RUST_STD_VERSION ?= 1.98.1
RUST_RELEASE_RUST_STD_SHA256_x86_64-unknown-linux-musl ?= bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f
RUST_RELEASE_RUST_STD_SHA256_aarch64-unknown-linux-musl ?= 7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a

# The platforms to build, as os/arch: both release platforms by default, and only the ones a
# recovery has to rebuild when the release passes a subset.
RUST_RELEASE_PLATFORMS ?= linux/amd64 linux/arm64
RUST_RELEASE_DIR       ?= $(REPO_ROOT)/$(DIST_DIR)

# The Rust target of a platform; an unknown platform stops make.
rust_release_triple = $(or $(RUST_RELEASE_TRIPLE_$(subst /,_,$(1))),$(error no Rust target for the platform $(1)))
RUST_RELEASE_TRIPLE_linux_amd64 ?= x86_64-unknown-linux-musl
RUST_RELEASE_TRIPLE_linux_arm64 ?= aarch64-unknown-linux-musl

# The release asset of a platform: $(call rust_release_asset,<os/arch>[,<bin>]), BIN_NAME's when
# no binary is given.
rust_release_asset = $(RUST_RELEASE_DIR)/$(or $(2),$(BIN_NAME))-$(subst /,-,$(1))

# checksums.txt covers every binary's asset on every shipped platform, whichever run built it: the
# release writes it after adding the assets a recovery took from what is already published.
RUST_RELEASE_SHIPPED_PLATFORMS ?= linux/amd64 linux/arm64

# Where cargo writes: its effective target directory (the binaries) and build directory (a build
# script's OUT_DIR, which is the target directory unless build.build-dir moves it), as cargo
# metadata reports them, so CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR, build.target-dir and
# build.build-dir in a .cargo/config.toml all count (rust.mk's RUST_TARGET_DIR reads the first
# only). Those variables and CARGO_HOME (its config.toml counts too) are passed to cargo by name:
# GNU make before 4.4 runs $(shell) without the command line's and the exported ones, which the
# build's recipe has. Recursive and read once per make run, so cargo is asked only by a recipe
# that copies; a consumer may set either instead. The keys are matched where cargo prints them,
# before version and workspace_root, so a [workspace.metadata] or [package.metadata] key of the
# same name is not read. A path holding a backslash or a double quote (escaped in the JSON) is
# not read: set it then. A path holding a space stops make: RUSTFLAGS cannot remap it.
_RUST_RELEASE_CARGO_ENV = $(foreach var,CARGO_HOME CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_BUILD_DIR,$(if $($(var)),$(var)="$($(var))"))
_RUST_RELEASE_METADATA = $(shell cd "$(REPO_ROOT)" && $(_RUST_RELEASE_CARGO_ENV) $(CARGO) metadata --format-version 1 --no-deps 2>/dev/null | sed -n -e 's/.*"target_directory":"\([^"\\ ]*\)","build_directory":"\([^"\\ ]*\)","version":1,"workspace_root":".*/\1 \2/p' -e t -e 's/.*"target_directory":"\([^"\\ ]*\)","version":1,"workspace_root":".*/\1/p')
_rust_release_metadata = $(or $(_RUST_RELEASE_DIRS),$(eval _RUST_RELEASE_DIRS := $$(_RUST_RELEASE_METADATA))$(_RUST_RELEASE_DIRS))
RUST_RELEASE_TARGET_DIR ?= $(word 1,$(_rust_release_metadata))
RUST_RELEASE_BUILD_DIR  ?= $(or $(word 2,$(_rust_release_metadata)),$(RUST_RELEASE_TARGET_DIR))
rust_release_target_dir = $(if $(word 3,$(RUST_RELEASE_TARGET_DIR) $(RUST_RELEASE_BUILD_DIR)),$(error a target or build directory with a space cannot be remapped))$(or $(RUST_RELEASE_TARGET_DIR),$(error cannot read cargo's target directory; set RUST_RELEASE_TARGET_DIR))
# The directories cargo writes to other than the checkout's target/ (outside it, or another one
# inside it), remapped to /src/target, where a default build has them, so a build with another
# directory gives the same bytes; of two nested ones the inner comes last. A target directory
# that is the checkout itself, or nesting three deep with the cargo home, is not supported.
_rust_release_outside_one = $(if $(filter $(REPO_ROOT)/target,$(patsubst %/,%,$(1))),,$(patsubst %/,%,$(1)))
_rust_release_nested_last = $(if $(filter $(2)/%,$(1)),$(2) $(1),$(1) $(2))
_rust_release_outside = $(call _rust_release_nested_last,$(call _rust_release_outside_one,$(rust_release_target_dir)),$(call _rust_release_outside_one,$(filter-out $(rust_release_target_dir),$(RUST_RELEASE_BUILD_DIR))))

# The static build of every binary for one Rust target: cargo-zigbuild with zig as the C compiler
# and linker, the version stamps of rust-dist, and the checkout and the cargo home remapped, so a
# binary carries no path of the machine that built it and a rebuild of the same commit gives the
# same bytes. A target or build directory other than the checkout's target/ is remapped too;
# rustc applies the last prefix that matches, so the checkout, the cargo home or rust-src inside
# one is remapped again after it. The toolchain's rust-src, when it is installed, is mapped back
# to the /rustc/<commit> the standard library was built with: rustc otherwise writes the local
# copy's path, under the rustup home, in place of that prefix, so a machine with rust-src and one
# without (CI's minimal profile) would build different bytes. RUSTFLAGS is set, not inherited: a
# flag from the environment would change the shipped bytes. With --target, cargo applies it to
# the binaries and not to build scripts. Recursive (=), so rustc is asked only by a recipe that
# builds.
RUST_RELEASE_CARGO_HOME = $(or $(CARGO_HOME),$(HOME)/.cargo)
RUST_RELEASE_RUST_SRC   = $(shell cd "$(REPO_ROOT)" && rustc --print sysroot)/lib/rustlib/src/rust
RUST_RELEASE_RUSTC_HASH = $(shell cd "$(REPO_ROOT)" && rustc -vV | sed -n 's/^commit-hash: //p')
# The remaps, as prefix=replacement: the fixed ones, then the outside directories, then again
# each fixed one inside an outside directory, so for every path the longest prefix comes last.
_RUST_RELEASE_REMAPS    = $(REPO_ROOT)=/src $(RUST_RELEASE_CARGO_HOME)=/cargo \
  $(RUST_RELEASE_RUST_SRC)=/rustc/$(RUST_RELEASE_RUSTC_HASH)
_rust_release_remaps    = $(_RUST_RELEASE_REMAPS) $(addsuffix =/src/target,$(_rust_release_outside)) \
  $(foreach remap,$(_RUST_RELEASE_REMAPS),$(if $(filter $(addsuffix /%,$(_rust_release_outside)),$(firstword $(subst =, ,$(remap)))),$(remap)))
RUST_RELEASE_RUSTFLAGS  = $(strip $(addprefix --remap-path-prefix=,$(_rust_release_remaps)))
# $(call rust_static_build,<Rust target>)
rust_static_build = RUSTFLAGS="$(RUST_RELEASE_RUSTFLAGS)" $(RUST_VERSION_ENV) \
  $(CARGO) zigbuild --release --locked --target $(1) $(addprefix --bin ,$(RUST_RELEASE_BINS))

# The static builds take only the pinned zig and cargo-zigbuild: another release links other bytes.
.PHONY: rust-zigbuild-check
rust-zigbuild-check: ## Fail unless the zig and cargo-zigbuild on PATH are the pinned releases
	@set -eu; \
	for tool in zig cargo-zigbuild; do \
	  if ! command -v "$$tool" >/dev/null 2>&1; then \
	    echo "$$tool is not on PATH; install the release rust-release.mk pins (make rust-zigbuild-install; CI: setup-zigbuild)" >&2; \
	    exit 1; \
	  fi; \
	done; \
	zig_version="$$(zig version)"; \
	if [ "$$zig_version" != "$(RUST_RELEASE_ZIG_VERSION)" ]; then \
	  echo "zig $$zig_version is on PATH; the static builds need zig $(RUST_RELEASE_ZIG_VERSION)" >&2; \
	  exit 1; \
	fi; \
	zigbuild_version="$$(cargo-zigbuild --version)"; \
	if [ "$$zigbuild_version" != "cargo-zigbuild $(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)" ]; then \
	  echo "$$zigbuild_version is on PATH; the static builds need cargo-zigbuild $(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)" >&2; \
	  exit 1; \
	fi

# One binary's copy into its asset: $(call _RUST_RELEASE_COPY,<os/arch>,<bin>). The blank line
# ends the recipe line.
define _RUST_RELEASE_COPY
	cp "$(rust_release_target_dir)/$(call rust_release_triple,$(1))/release/$(2)" "$(call rust_release_asset,$(1),$(2))"

endef

# One platform: the build, then a copy per binary.
define _RUST_RELEASE_BINARY
	$(call rust_static_build,"$(call rust_release_triple,$(1))")
$(foreach bin,$(RUST_RELEASE_BINS),$(call _RUST_RELEASE_COPY,$(1),$(bin)))
endef

.PHONY: rust-release-binaries
rust-release-binaries: rust-zigbuild-check $(RUST_RELEASE_PREREQS) ## Build the static musl release binaries (RUST_RELEASE_BINS) for RUST_RELEASE_PLATFORMS into dist/
	mkdir -p "$(RUST_RELEASE_DIR)"
	$(foreach platform,$(RUST_RELEASE_PLATFORMS),$(call _RUST_RELEASE_BINARY,$(platform)))

.PHONY: rust-release-checksums
rust-release-checksums: ## Write dist/checksums.txt over every binary's asset on every shipped platform
	cd "$(RUST_RELEASE_DIR)" && sha256sum $(foreach bin,$(RUST_RELEASE_BINS),$(foreach platform,$(RUST_RELEASE_SHIPPED_PLATFORMS),"$(notdir $(call rust_release_asset,$(platform),$(bin)))")) > checksums.txt

# The two above in order. Goals on one command line are not ordered under make -j, so
# `make rust-release-binaries rust-release-checksums` could checksum binaries a parallel build has
# not written yet; each step here is its own make run, the second started only after the first
# succeeded. rust-release-checksums stays on its own for a recovery, which checksums published
# binaries without rebuilding them.
.PHONY: rust-release-dist
rust-release-dist: ## Build the release binaries, then write dist/checksums.txt (ordered, safe under make -j)
	$(MAKE) --no-print-directory rust-release-binaries
	$(MAKE) --no-print-directory rust-release-checksums

# Installs the pinned zig and cargo-zigbuild for the build host into RUST_RELEASE_TOOLS_DIR, each
# archive checked against its sha256 above before it is unpacked, for a local build (CI uses
# setup-zigbuild); put $(RUST_RELEASE_TOOLS_DIR)/bin on PATH afterwards. The directory must not
# exist yet, so nothing from an earlier install mixes in. cargo-zigbuild publishes no aarch64 musl
# asset, so an arm64 host takes the gnu one.
RUST_RELEASE_TOOLS_DIR ?= $(REPO_ROOT)/target/zigbuild-tools

.PHONY: rust-zigbuild-install
rust-zigbuild-install: ## Install the pinned zig and cargo-zigbuild into RUST_RELEASE_TOOLS_DIR (network)
	@set -eu; \
	case "$$(uname -m)" in \
	  x86_64) zig_arch=x86_64; zig_sha256=$(RUST_RELEASE_ZIG_SHA256_x86_64); \
	    zigbuild_asset=x86_64-unknown-linux-musl; \
	    zigbuild_sha256=$(RUST_RELEASE_CARGO_ZIGBUILD_SHA256_x86_64-unknown-linux-musl) ;; \
	  aarch64) zig_arch=aarch64; zig_sha256=$(RUST_RELEASE_ZIG_SHA256_aarch64); \
	    zigbuild_asset=aarch64-unknown-linux-gnu; \
	    zigbuild_sha256=$(RUST_RELEASE_CARGO_ZIGBUILD_SHA256_aarch64-unknown-linux-gnu) ;; \
	  *) echo "no pinned zig for $$(uname -m)" >&2; exit 1 ;; \
	esac; \
	dir="$(RUST_RELEASE_TOOLS_DIR)"; \
	if [ -e "$$dir" ]; then echo "$$dir already exists; install into a new directory" >&2; exit 1; fi; \
	mkdir -p "$$dir/bin" "$$dir/zig" "$$dir/downloads"; \
	curl -fsSL --proto '=https' --tlsv1.2 -o "$$dir/downloads/zig.tar.xz" \
	  "https://ziglang.org/download/$(RUST_RELEASE_ZIG_VERSION)/zig-$$zig_arch-linux-$(RUST_RELEASE_ZIG_VERSION).tar.xz"; \
	echo "$$zig_sha256  $$dir/downloads/zig.tar.xz" | sha256sum -c -; \
	tar -xJf "$$dir/downloads/zig.tar.xz" -C "$$dir/zig" --strip-components=1; \
	ln -s ../zig/zig "$$dir/bin/zig"; \
	curl -fsSL --proto '=https' --tlsv1.2 -o "$$dir/downloads/cargo-zigbuild.tar.xz" \
	  "https://github.com/rust-cross/cargo-zigbuild/releases/download/v$(RUST_RELEASE_CARGO_ZIGBUILD_VERSION)/cargo-zigbuild-$$zigbuild_asset.tar.xz"; \
	echo "$$zigbuild_sha256  $$dir/downloads/cargo-zigbuild.tar.xz" | sha256sum -c -; \
	tar -xJf "$$dir/downloads/cargo-zigbuild.tar.xz" -C "$$dir/bin" --strip-components=1; \
	"$$dir/bin/zig" version; "$$dir/bin/cargo-zigbuild" --version

endif  # MK_COMMON_RUST_RELEASE_INCLUDED
