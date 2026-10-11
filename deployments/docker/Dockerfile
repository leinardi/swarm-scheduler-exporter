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
# build: compile static binary
########################
FROM --platform=$BUILDPLATFORM dhi.io/golang:1.26.9-alpine3.23-dev AS build
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

# Buildx provides these automatically for multi-arch builds
ARG TARGETOS
ARG TARGETARCH

ENV CGO_ENABLED=0 \
    GOOS=${TARGETOS} \
    GOARCH=${TARGETARCH}

WORKDIR /src

# Bring in source
COPY . .

# Module and build caches (BuildKit)
RUN --mount=type=cache,target=/go/pkg/mod,id=gomodcache \
	--mount=type=cache,target=/root/.cache/go-build \
	go build -trimpath -buildvcs=false \
		-ldflags="-s -w -X 'main.version=${VERSION}' -X 'main.commit=${COMMIT}' -X 'main.date=${DATE}'" \
		-o /out/swarm-scheduler-exporter ./cmd/swarm-scheduler-exporter

########################
# runtime: dhi.io hardened static base (distroless-style, no shell).
# The exporter binary is CGO_ENABLED=0 so no libc is required.
########################
FROM dhi.io/static:20250419

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
# the first resync, including its retry backoff. The probe uses the default -listen-addr: a
# deployment that changes -listen-addr must override this check and pass the same address (see
# the README Health section). An exporter that stays unhealthy is stuck, and Swarm replacing the
# unhealthy task is the intended remedy.
HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
  CMD ["/usr/local/bin/swarm-scheduler-exporter", "-healthcheck"]

ENTRYPOINT ["/usr/local/bin/swarm-scheduler-exporter"]
