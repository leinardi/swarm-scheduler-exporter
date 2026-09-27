# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

# The syntax line above pins the BuildKit frontend by index digest, so a moved
# 1.7 tag cannot change how this file is built. Dependabot's docker ecosystem
# does not update it: bump it by hand (docs/release.md).
#
# Base images are pinned as tag@sha256:<index digest>, so a rebuild of the same
# commit (a release recovery, for one) uses the same bases. Dependabot's docker
# ecosystem updates the tag and the digest together, through reviewed PRs.

########################
# build: compile static binary
########################
FROM --platform=$BUILDPLATFORM dhi.io/golang:1.26.8-alpine3.23-dev@sha256:c2f8109463456b1c6bf1c9e53e702c89508f1b3b4b5e72779407d1fbf03a76be AS build
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
FROM dhi.io/static:20250419@sha256:98ef7a853608577e8d66dad1d25ada75d745d782f28d84e9ecfb85dfeb1f9c98

# hadolint ignore=DL3050
LABEL org.opencontainers.image.title="swarm-scheduler-exporter" \
	org.opencontainers.image.description="Prometheus exporter for Docker Swarm services/tasks" \
	org.opencontainers.image.source="https://github.com/leinardi/swarm-scheduler-exporter" \
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
