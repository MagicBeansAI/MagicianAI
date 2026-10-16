# Multi-stage Debian/glibc image. See docs/components/scripts/container-runtime.md.

# ---------------------------------------------------------------------------
# Stage 1: UI builder — preserve the monorepo's shared SDK imports
# ---------------------------------------------------------------------------
FROM node:20-alpine AS ui

# Vite's render phase exceeds Node's default heap in a 4 GiB build VM.
# The backend depends on this completed stage, keeping the large heaps apart.
ENV NODE_OPTIONS=--max-old-space-size=3072
WORKDIR /build/ui/unified-ui
COPY ui/unified-ui/package.json ui/unified-ui/package-lock.json* ./
RUN npm ci --ignore-scripts
COPY ui/unified-ui/ ./
COPY sdk/typescript/ /build/sdk/typescript/
RUN npm --prefix /build/sdk/typescript ci --ignore-scripts --no-audit --no-fund
RUN npx svelte-kit sync && npm run build

# ---------------------------------------------------------------------------
# Stage 2: Rust builder — native Linux binaries for the selected image platform
# ---------------------------------------------------------------------------
FROM rust:1.92.0-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential clang cmake libclang-dev libssl-dev pkg-config \
      protobuf-compiler libprotobuf-dev perl git ca-certificates && rm -rf /var/lib/apt/lists/*

# Host ~/.cargo job-gating does not reach an image build. Bound Cargo and
# native build-system concurrency here as well. This is not an RSS limit.
ARG CARGO_BUILD_JOBS=1
ARG CARGO_PROFILE_RELEASE_LTO=false
ARG TARGETARCH
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    CMAKE_BUILD_PARALLEL_LEVEL=${CARGO_BUILD_JOBS} \
    CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO} \
    CARGO_TARGET_DIR=/build/target

WORKDIR /build

# Use the complete workspace, including magician-bin and the extracted crates.
# The former hand-maintained stub layer silently ignored resolution failures.
COPY . .
# Assemble the runtime payload here; this also serializes UI and Rust builds.
COPY --from=ui /build/ui/unified-ui/build/ /out/ui/
# Keep completed dependencies/artifacts when a first Linux build needs repair.
RUN --mount=type=cache,id=magician-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=magician-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=magician-cargo-target-${TARGETARCH},target=/build/target,sharing=locked \
    make build-container-runtime && \
    strip "$CARGO_TARGET_DIR/release/magician" \
          "$CARGO_TARGET_DIR/release/magicutor" \
          "$CARGO_TARGET_DIR/release/magic-supervisor" && \
    mkdir -p /out && \
    cp "$CARGO_TARGET_DIR/release/magician" \
       "$CARGO_TARGET_DIR/release/magicutor" \
       "$CARGO_TARGET_DIR/release/magic-supervisor" /out/
# MagicRun's in-jail helper (`magicrun-jail-egress-forwarder`): every Linux app
# jail execs through it (task ceiling, exec status) and brokered egress relays
# through it. Built from the exact MagicRun revision Magician pins (a
# dependency's binary, so `cargo install --git`).
RUN --mount=type=cache,id=magician-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=magician-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    MAGICRUN_REV="$(sed -n 's/^tool-runtime-core = { git = "https:\/\/github.com\/MagicBeansAI\/MagicRun.git", rev = "\([0-9a-f]*\)" }$/\1/p' Cargo.toml)"; \
    test -n "$MAGICRUN_REV"; \
    cargo install --root /out/jail --git https://github.com/MagicBeansAI/MagicRun.git \
      --rev "$MAGICRUN_REV" tool-runtime-core --bin magicrun-jail-egress-forwarder && \
    strip /out/jail/bin/magicrun-jail-egress-forwarder

# The same independent tool build is also available as containers/tools/Dockerfile
# for repairing a cached runtime without rebuilding the services or UI.
FROM golang:1.23.0-bookworm AS go
FROM rust:1.92.0-bookworm AS skilltools
COPY --from=go /usr/local/go/ /usr/local/go/
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev python3 perl ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ARG CARGO_BUILD_JOBS=1
ARG CARGO_PROFILE_RELEASE_LTO=false
ARG TARGETARCH
ENV PATH="/usr/local/go/bin:${PATH}" \
    CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO} \
    GOMAXPROCS=1 GOFLAGS=-p=1
WORKDIR /build
COPY . .
RUN --mount=type=cache,id=magician-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=magician-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=magician-cargo-target-${TARGETARCH},target=/build/target,sharing=locked \
    --mount=type=cache,id=magician-tools-go-${TARGETARCH},target=/root/go,sharing=locked \
    --mount=type=cache,id=magician-tools-gocache-${TARGETARCH},target=/root/.cache/go-build,sharing=locked \
    make build-container-skill-tools

# ---------------------------------------------------------------------------
# Stage 2b: agent-browser (patched, glibc)
# ---------------------------------------------------------------------------
# The runtime's `browser` skill ships the VANILLA agent-browser via npm (which
# carries linux glibc binaries). We overlay the magician-patched build on top.
# The patch (download fixes + --keep-browser + startup exit diagnostics)
# is maintained against upstream v0.38.1. Its dependency graph remains
# pure-Rust/rustls and builds on this stage's Rust 1.92 Bookworm image.
# See docs/plans/2026-06-23-composed-installer-design.md.
# Depend on builder so BuildKit cannot compile the two Rust graphs at once.
# Inherit the explicit job cap and matching glibc/toolchain.
FROM skilltools AS agentbrowser
ARG AB_TAG=v0.38.1
RUN git clone --depth 1 --branch ${AB_TAG} https://github.com/vercel-labs/agent-browser.git /ab
# The patch path is re-included in .dockerignore (the _vendor/ tree is otherwise excluded).
COPY skillshub/browser/_vendor/v0.38.1-Magician.0/patches/0001-magician-patches.patch /tmp/ab.patch
# cli/ is its own package root (no workspace), so cargo would otherwise write to
# /ab/cli/target/release/. Pin CARGO_TARGET_DIR=/ab/target so the output lands at
# /ab/target/release/agent-browser — matching the COPY sources + comment below.
RUN cd /ab && git apply --3way /tmp/ab.patch && CARGO_TARGET_DIR=/ab/target cargo build --locked --release --manifest-path cli/Cargo.toml
# -> /ab/target/release/agent-browser  (glibc, patched 0.38.1-Magician.0)

# ---------------------------------------------------------------------------
# Stage 3: Runtime — debian-slim (glibc) with binaries, UI, and seed data
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime-base

# Managed interpreters must be traversable by the final non-root user too.
# uv's default /root/.local/share/uv/python breaks the skill venv at first boot.
ENV UV_PYTHON_INSTALL_DIR=/opt/uv/python

# Runtime utilities needed by tool execution and health checks
COPY scripts/install-container-tools.sh /tmp/
RUN sh /tmp/install-container-tools.sh && rm /tmp/install-container-tools.sh

# OCI image labels
LABEL org.opencontainers.image.source="https://github.com/magicbeanbs100x/magician"
LABEL org.opencontainers.image.description="Magician — intelligent agent supervisor (magician + magicutor + magic-supervisor)"
LABEL org.opencontainers.image.version="0.1.0"

WORKDIR /app

# Copy seed / prompt data
COPY data/ ./data/

# Runtime configs read relative to CWD /app:
#   - magician is launched with `--config tool-runtime-config.yaml`
#     (magic-supervisor passes this; magician/src/bin/magician.rs default).
#   - magicutor reads magicutor/config/magicutor-config.yaml (its search path
#     #2; magic-supervisor also sets MAGICUTOR_CONFIG_PATH to this when unset).
# Without these the children fail to start, so they must be baked into /app.
COPY tool-runtime-config.yaml ./tool-runtime-config.yaml
COPY magician-config.yaml ./magician-config.yaml
COPY llm-router.yaml ./llm-router.yaml
COPY magicutor/config/ ./magicutor/config/

# Host CDP uses the desktop extension; in-container automation uses baked browsers.

# Read-only seed; live runtime state remains in the mounted runtime root.
COPY magician_data_v3/ ./magician_data_v3/

# Operator / runtime helper scripts (host SilverBullet launch, seed-runtime-root,
# Ollama host setup, etc.).
COPY scripts/ ./scripts/

# Skill source; Linux dependency trees are rebuilt below.
COPY skillshub/ ./skillshub/
ENV MAGICIAN_SKILLSHUB_ROOT=/app/skillshub
COPY --from=skilltools /out/skill-tools/document-to-markdown /app/skillshub/document-to-markdown/bin/document-to-markdown
COPY --from=skilltools /out/skill-tools/metabase-pp-cli /app/skillshub/metabase/bin/metabase-pp-cli
COPY containers/tools/higgsfield-release.json /app/containers/tools/higgsfield-release.json
# These install pinned Linux artifacts only, never operator account state.
RUN python3 /app/skillshub/scripts/setup_officecli.py && \
    python3 /app/scripts/install-container-higgsfield.py

# uv and the managed Python tools use a world-traversable prefix.
ENV PATH="/opt/uv/bin:${PATH}"

# Optional Linux skill dependencies are fail-open; required bins are checked later.
RUN set -eux; \
    make -C /app/skillshub setup-node setup-deps setup-skill-bins setup-pdftotext setup-ocr \
      || echo "WARN: some skillshub node/bin deps failed (continuing)"; \
    ( cd /app/skillshub/browser && npm install --include=optional ) \
      || echo "WARN: browser npm install failed (continuing)"; \
    make -C /app/skillshub setup-python \
      || echo "WARN: skillshub python venv partial (continuing)"

# Pi coding agent CLI (MIT) — REQUIRED, not fail-open like the skill deps above:
# the runtime lists a harness engine only when its binary is on PATH. Debian's
# nodejs is too old for Pi (>= 22.19), so Pi installs and runs under the pinned
# Skillshub Node; the /usr/local/bin/pi wrapper names that node explicitly
# instead of trusting the bundle's `#!/usr/bin/env node`. The version comes from
# setup-pi-coding-agent.sh, the same pin the host installer and verify use.
RUN set -eux; \
    NODE_DIR=/app/skillshub/.node/bin; \
    test -x "$NODE_DIR/node" || { echo "ERROR: Skillshub Node missing; Pi requires it"; exit 1; }; \
    PI_PACKAGE="$(sed -n 's/^PI_PACKAGE="\${MAGICIAN_PI_PACKAGE:-\([^}]*\)}"$/\1/p' /app/scripts/setup-pi-coding-agent.sh)"; \
    PI_VERSION="$(sed -n 's/^PI_VERSION="\${MAGICIAN_PI_VERSION:-\([^}]*\)}"$/\1/p' /app/scripts/setup-pi-coding-agent.sh)"; \
    test -n "$PI_PACKAGE" && test -n "$PI_VERSION"; \
    PATH="$NODE_DIR:$PATH" npm install -g --prefix /opt/pi --ignore-scripts --no-audit --no-fund "$PI_PACKAGE@$PI_VERSION"; \
    PI_CLI="/opt/pi/lib/node_modules/$PI_PACKAGE/$("$NODE_DIR/node" -p "require('/opt/pi/lib/node_modules/$PI_PACKAGE/package.json').bin.pi")"; \
    test -f "$PI_CLI"; \
    printf '#!/bin/sh\nexec %s %s "$@"\n' "$NODE_DIR/node" "$PI_CLI" > /usr/local/bin/pi; \
    chmod 755 /usr/local/bin/pi; \
    chmod -R a+rX /opt/pi; \
    pi --version | grep -Fq "$PI_VERSION"

# Overlay the patched glibc agent-browser for either image architecture.
COPY --from=agentbrowser /ab/target/release/agent-browser /app/skillshub/browser/node_modules/agent-browser/bin/agent-browser-linux-x64
COPY --from=agentbrowser /ab/target/release/agent-browser /app/skillshub/browser/node_modules/agent-browser/bin/agent-browser-linux-arm64
COPY --from=agentbrowser /ab/target/release/agent-browser /app/skillshub/browser/bin/agent-browser
RUN chmod +x /app/skillshub/browser/node_modules/agent-browser/bin/agent-browser-linux-x64 \
             /app/skillshub/browser/node_modules/agent-browser/bin/agent-browser-linux-arm64 \
             /app/skillshub/browser/bin/agent-browser && \
    bash /app/skillshub/browser/scripts/verify-skill-discovery.sh \
      /app/skillshub/browser/bin/agent-browser \
      /app/skillshub/browser/node_modules/agent-browser

# Bake Obscura and Chrome-for-Testing into a stable, world-readable cache.
ENV BROWSER_CACHE_HOME=/opt/browser-cache
RUN mkdir -p "$BROWSER_CACHE_HOME"
RUN HOME="$BROWSER_CACHE_HOME" make -C /app/skillshub setup-cloak-browser \
      || echo "WARN: cloak/obscura browser fetch failed (continuing; agent-browser falls back to CfT)"
# The npm wrapper selects the matching glibc architecture.
RUN HOME="$BROWSER_CACHE_HOME" node /app/skillshub/browser/node_modules/agent-browser/bin/agent-browser.js install \
      || echo "WARN: Chrome-for-Testing unavailable (Linux ARM64 requires CloakBrowser, system Chromium, or host CDP)"
# Make the baked caches readable/writable by the runtime user.
RUN chmod -R a+rX "$BROWSER_CACHE_HOME" || true

# Run as non-root for security (debian: groupadd/useradd, not alpine addgroup/adduser)
# /opt/uv holds uv + the marimo tool venv/bin (installed world-traversable by
# install-container-tools.sh); re-assert a+rX so the non-root `magician` user can
# traverse it and resolve the marimo bin on PATH (/opt/uv/bin).
RUN groupadd -r magician && useradd -r -g magician magician && \
    mkdir -p /data && \
    chown -R magician:magician /app && \
    chown -R magician:magician /data && \
    chmod -R a+rX /opt/uv
USER magician
# Browser materialization needs PyYAML before the supervisor can start.
RUN /app/skillshub/.venv/bin/python -c 'import yaml'
# Presence/architecture only; live and governed canaries remain separate gates.
# Optional setup above must not silently leave advertised Linux tools missing.
RUN /app/skillshub/.venv/bin/python /app/scripts/verify-container-skill-bins.py

# Runtime root: live data / config / secrets live here, mounted from the host
# (e.g. `-v ~/MagicianNotes:/data`). The seed (magician_data_v3) is baked above.
# SilverBullet + Ollama run on the HOST; the container reaches them over the host
# network (see the `run-container` make target).
ENV MAGICIAN_ROOT_DIR=/data \
    MAGICIAN_HTTP_HOST=0.0.0.0 \
    MAGICIAN_FRONTEND_DIR=/app/ui
VOLUME /data

# magician (3002), magicutor (3003)
EXPOSE 3002 3003

# Health check against magician's /health endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -sf http://localhost:3002/health || exit 1

# The entrypoint seeds /data/magician-config.yaml when missing, then execs
# magic-supervisor to start magician + magicutor.
CMD ["bash", "./scripts/container-entrypoint.sh"]

# The ordinary Dockerfile target remains a complete standalone image. Local OCI
# preparation targets runtime-base, then overlays host-built Zig artifacts.
FROM runtime-base AS runtime
USER root
# App OS jail on Linux: bubblewrap is setuid-root so it works in a default
# container (no user-namespace or seccomp flags needed; it drops privileges
# itself). The jail helper must be root-owned at its fixed path; MagicRun
# refuses any other location or owner, and without it every jail is refused.
RUN chmod u+s /usr/bin/bwrap
COPY --from=builder /out/jail/bin/magicrun-jail-egress-forwarder /usr/libexec/magicrun/magicrun-jail-egress-forwarder
RUN chown -R root:root /usr/libexec/magicrun && chmod 0755 /usr/libexec/magicrun \
      /usr/libexec/magicrun/magicrun-jail-egress-forwarder
COPY --from=builder --chown=magician:magician /out/magician /app/magician.bin
COPY --from=builder --chown=magician:magician /out/magicutor /app/magicutor.bin
COPY --from=builder --chown=magician:magician /out/magic-supervisor /app/magic-supervisor
COPY --from=builder --chown=magician:magician /out/ui/ /app/ui/
USER magician
