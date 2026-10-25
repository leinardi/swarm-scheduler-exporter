# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e
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

# The syntax line at the top pins the BuildKit frontend by index digest, so a moved
# 1.7 tag cannot change how this file is built. Dependabot's docker ecosystem
# does not update it: bump it by hand (docs/release.md).
#
# Base images are pinned by tag, not by digest: dhi.io republishes its tags
# often to ship security fixes, and a rebuild should pick those up. Every
# release build is scanned, so a newer base never ships unscanned.

########################
# build: cross-compile a static musl binary for the target platform on the build platform, with
# cargo-zigbuild and zig as the C compiler and linker (ring's assembly included).
#
# The debian variant, because it carries the upstream toolchain, for which the upstream musl
# standard libraries below fit; the alpine variant's rustc is Alpine's own build for its
# x86_64-alpine-linux-musl host and cannot take another target's standard library.
########################
FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

# The image's own toolchain version: the standard libraries added below must be the same release.
ARG RUST_VERSION=1.98.1
ARG ZIG_VERSION=0.17.0
ARG CARGO_ZIGBUILD_VERSION=0.24.0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# curl and xz fetch and unpack zig and the standard libraries; nothing from this stage but the
# binary ships. Unpinned: dhi.io republishes this tag with a refreshed package index, which would
# break any pinned version, and every download below is checked by its own sha256 (.hadolint.yaml
# ignores DL3008 for this reason).
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
	&& rm -rf /var/lib/apt/lists/*

# zig for the build platform, checked against the sha256 ziglang.org publishes.
ARG BUILDARCH
RUN case "${BUILDARCH}" in \
		amd64) zig_arch=x86_64; zig_sha256=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;; \
		arm64) zig_arch=aarch64; zig_sha256=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;; \
		*) echo "no zig for build platform ${BUILDARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz" \
	&& echo "${zig_sha256}  /tmp/zig.tar.xz" | sha256sum -c - \
	&& mkdir /opt/zig \
	&& tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
	&& rm /tmp/zig.tar.xz
ENV PATH="/opt/zig:${PATH}"

# cargo-zigbuild's release binary for the build platform, checked against the sha256 its release
# publishes. The release has no aarch64 musl asset, and this stage is glibc-based, so arm64 takes
# the gnu one.
RUN case "${BUILDARCH}" in \
		amd64) zigbuild_asset=x86_64-unknown-linux-musl; zigbuild_sha256=52be2cb85ad3875682564a29bac1832b2ab3db68b1c504765c92d5249cc9b96f ;; \
		arm64) zigbuild_asset=aarch64-unknown-linux-gnu; zigbuild_sha256=de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c ;; \
		*) echo "no cargo-zigbuild for build platform ${BUILDARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/cargo-zigbuild.tar.xz "https://github.com/rust-cross/cargo-zigbuild/releases/download/v${CARGO_ZIGBUILD_VERSION}/cargo-zigbuild-${zigbuild_asset}.tar.xz" \
	&& echo "${zigbuild_sha256}  /tmp/cargo-zigbuild.tar.xz" | sha256sum -c - \
	&& mkdir /tmp/cargo-zigbuild \
	&& tar -xJf /tmp/cargo-zigbuild.tar.xz -C /tmp/cargo-zigbuild --strip-components=1 \
	&& install -m 0755 /tmp/cargo-zigbuild/cargo-zigbuild /usr/local/bin/cargo-zigbuild \
	&& rm -rf /tmp/cargo-zigbuild /tmp/cargo-zigbuild.tar.xz \
	&& cargo-zigbuild --version

# The musl standard library of the target platform, checked against static.rust-lang.org's sha256.
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
		amd64) triple=x86_64-unknown-linux-musl; std_sha256=bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f ;; \
		arm64) triple=aarch64-unknown-linux-musl; std_sha256=7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a ;; \
		*) echo "no Rust target for platform ${TARGETARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/rust-std.tar.xz "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz" \
	&& echo "${std_sha256}  /tmp/rust-std.tar.xz" | sha256sum -c - \
	&& mkdir /tmp/rust-std \
	&& tar -xJf /tmp/rust-std.tar.xz -C /tmp/rust-std --strip-components=1 \
	&& /tmp/rust-std/install.sh --prefix=/usr/local --disable-ldconfig \
	&& rm -rf /tmp/rust-std /tmp/rust-std.tar.xz \
	&& echo "${triple}" > /tmp/triple

WORKDIR /src
COPY . .

# The registry and the target directory are BuildKit caches, so the binary is copied out of them.
# The registry cache is shared by both platforms' builds, which run in parallel: sharing=locked
# serialises them, since two cargo processes unpacking the same crate into it fail with
# "File exists". The target cache is per platform and needs no lock.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry,sharing=locked \
	--mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH} \
	triple="$(cat /tmp/triple)" \
	&& SWARM_SCHEDULER_EXPORTER_VERSION="${VERSION}" \
		SWARM_SCHEDULER_EXPORTER_COMMIT="${COMMIT}" \
		SWARM_SCHEDULER_EXPORTER_DATE="${DATE}" \
		cargo zigbuild --release --locked --target "${triple}" --bin swarm-scheduler-exporter \
	&& mkdir -p /out \
	&& cp "target/${triple}/release/swarm-scheduler-exporter" /out/swarm-scheduler-exporter

########################
# runtime: dhi.io hardened static base (distroless-style, no shell).
# The binary is statically linked against musl, so no libc is required.
########################
FROM dhi.io/static:20250419 AS runtime

# The same build arguments as the build stage: an ARG is scoped to the stage that declares it.
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

# The seven OCI labels .hadolint.yaml requires (label-schema, DL3049 as an error).
LABEL org.opencontainers.image.title="swarm-scheduler-exporter" \
	org.opencontainers.image.description="Prometheus exporter for Docker Swarm services/tasks" \
	org.opencontainers.image.source="https://github.com/leinardi/swarm-scheduler-exporter" \
	org.opencontainers.image.version="${VERSION}" \
	org.opencontainers.image.revision="${COMMIT}" \
	org.opencontainers.image.created="${DATE}" \
	org.opencontainers.image.licenses="MIT"

# Copy the binary
COPY --from=build /out/swarm-scheduler-exporter /usr/local/bin/swarm-scheduler-exporter

# Run as a fixed non-root user; the exporter does not need root. The uid is pinned here rather
# than inherited from the base image's default (65532 today), so a base update cannot change it.
# A deployment that reaches the Docker socket through a bind mount adds the socket's group with
# --group-add (docker run) or user: "65532:<gid>" (compose, stack) instead of running as root.
USER 65532:65532

# Document the metrics port
EXPOSE 8888

# The image has no shell or curl, so the binary probes its own /healthz. The start period covers
# the first resync, including its retry backoff. The probe uses the default --listen-addr: a
# deployment that changes --listen-addr must override this check and pass the same address (see
# the README Health section). An exporter that stays unhealthy is stuck, and Swarm replacing the
# unhealthy task is the intended remedy.
HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
  CMD ["/usr/local/bin/swarm-scheduler-exporter", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/swarm-scheduler-exporter"]
