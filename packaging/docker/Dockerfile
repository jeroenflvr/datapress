# syntax=docker/dockerfile:1
#
# Lightweight runtime image for the standalone `datapress` CLI.
#
# Instead of compiling the (heavy, bundled DuckDB C++) binary from source,
# this image downloads the prebuilt release binary — both DuckDB and
# DataFusion backends bundled — for the target architecture and runs it on a
# minimal distroless base. The binary links rustls + aws-lc-rs (no OpenSSL),
# so distroless/cc (glibc + libstdc++ + libgcc + CA certs) is all it needs.
#
# Build (multi-arch) via the release workflow, or locally:
#   docker buildx build --build-arg VERSION=0.4.5 \
#     -f packaging/docker/Dockerfile -t datapress:0.4.5 --load .
#
# The VERSION build-arg must match a published GitHub release tag (without
# the leading "v").

# ---- fetch stage: download + verify the prebuilt binary ------------------
# Runs on the build platform (no emulation); picks the binary by TARGETARCH.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS fetch

ARG VERSION
ARG TARGETARCH
ARG REPO=jeroenflvr/datapress

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends curl ca-certificates; \
    rm -rf /var/lib/apt/lists/*; \
    if [ -z "${VERSION:-}" ]; then echo "VERSION build-arg is required" >&2; exit 1; fi; \
    case "$TARGETARCH" in \
      amd64) triple="x86_64-unknown-linux-gnu" ;; \
      arm64) triple="aarch64-unknown-linux-gnu" ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    base="https://github.com/${REPO}/releases/download/v${VERSION}"; \
    archive="datapress-v${VERSION}-${triple}.tar.gz"; \
    curl -fsSL "${base}/${archive}" -o /tmp/dp.tar.gz; \
    if curl -fsSL "${base}/${archive}.sha256" -o /tmp/dp.sha256; then \
      echo "$(awk '{print $1}' /tmp/dp.sha256)  /tmp/dp.tar.gz" | sha256sum -c -; \
    else \
      echo "warning: checksum not published for v${VERSION}; skipping verification" >&2; \
    fi; \
    tar -xzf /tmp/dp.tar.gz -C /usr/local/bin datapress; \
    chmod 0755 /usr/local/bin/datapress
# NOTE: no `datapress --version` smoke test here — this stage runs on
# $BUILDPLATFORM but the binary is built for $TARGETARCH, so executing it
# would fail on cross-arch builds. The SHA-256 check above already verifies
# integrity; the runtime stage runs the real binary on the target platform.

# ---- runtime stage: minimal distroless ----------------------------------
# debian13 (trixie, GLIBC 2.41) — the release binaries are built on Ubuntu
# 24.04 and need GLIBC >= 2.39, so the older debian12 base (GLIBC 2.36) would
# fail at startup with "GLIBC_2.39 not found".
FROM gcr.io/distroless/cc-debian13:nonroot

LABEL org.opencontainers.image.title="datapress" \
      org.opencontainers.image.description="Configuration-driven HTTP query server over Parquet and Delta (DuckDB / DataFusion backends)." \
      org.opencontainers.image.url="https://datap-rs.org" \
      org.opencontainers.image.documentation="https://docs.datap-rs.org" \
      org.opencontainers.image.source="https://github.com/jeroenflvr/datapress" \
      org.opencontainers.image.licenses="MIT"

COPY --from=fetch /usr/local/bin/datapress /usr/local/bin/datapress

# Config discovery order is $DATAPRESS_CONFIG_FILE → ./datasets.toml →
# $HOME/datasets.toml. Point it at a conventional mount path; bind-mount or
# bake your own datasets.toml there. NOTE: the file must set
# `listen = "0.0.0.0"` for the port to be reachable from outside the
# container (datapress defaults to loopback).
ENV DATAPRESS_CONFIG_FILE=/etc/datapress/datasets.toml

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/datapress"]
