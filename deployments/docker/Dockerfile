# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

# The syntax line above pins the BuildKit frontend by index digest, so a moved
# 1.7 tag cannot change how this file is built. Dependabot's docker ecosystem
# does not update it: bump it by hand (docs/release.md).
#
# Base images are pinned as tag@sha256:<index digest>, so a rebuild of the same
# commit (a release recovery, for one) uses the same bases. Dependabot's docker
# ecosystem updates the tag and the digest together, through reviewed PRs.

########################
# deps: cache modules
########################
FROM --platform=$BUILDPLATFORM dhi.io/golang:1-alpine3.23-dev@sha256:0d95d4f9889abcf60c182228d28fa83fde3e0dc2330bb1365e5960273b43ed57 AS deps
WORKDIR /src
RUN --mount=type=cache,target=/var/cache/apk \
	apk add --no-cache git
COPY go.mod go.sum ./
# Use a named cache so we can reuse it in the build stage (no COPY needed)
RUN --mount=type=cache,target=/go/pkg/mod,id=gomodcache \
	go mod download

########################
# build: compile static binary
########################
FROM --platform=$BUILDPLATFORM dhi.io/golang:1-alpine3.23-dev@sha256:0d95d4f9889abcf60c182228d28fa83fde3e0dc2330bb1365e5960273b43ed57 AS build
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

# Reuse the same module & build caches (BuildKit)
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
	org.opencontainers.image.licenses="Apache-2.0"

# Copy the binary
COPY --from=build /out/swarm-scheduler-exporter /usr/local/bin/swarm-scheduler-exporter

# dhi.io/static defaults to a non-root user; the exporter does not need root.

# Document the metrics port
EXPOSE 8888

ENTRYPOINT ["/usr/local/bin/swarm-scheduler-exporter"]
