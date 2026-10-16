# Repair/cache tool executables on top of an existing qualified runtime image.
# Builds only the document adapter and generated Metabase CLI. No Magician,
# Magicutor, desktop or UI build; no runtime data, accounts or credentials.
ARG RUNTIME_BASE=magician:runtime-base
FROM golang:1.23.0-bookworm AS go
FROM rust:1.92.0-bookworm AS tools
COPY --from=go /usr/local/go/ /usr/local/go/
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev python3 perl ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV PATH="/usr/local/go/bin:${PATH}" \
    CARGO_TARGET_DIR=/build/target CARGO_BUILD_JOBS=1 \
    CARGO_PROFILE_RELEASE_LTO=false GOMAXPROCS=1 GOFLAGS=-p=1
WORKDIR /build
COPY . .
ARG TARGETARCH
RUN --mount=type=cache,id=magician-tools-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=magician-tools-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=magician-tools-target-${TARGETARCH},target=/build/target,sharing=locked \
    --mount=type=cache,id=magician-tools-go-${TARGETARCH},target=/root/go,sharing=locked \
    --mount=type=cache,id=magician-tools-gocache-${TARGETARCH},target=/root/.cache/go-build,sharing=locked \
    make build-container-skill-tools

FROM ${RUNTIME_BASE}
USER root
COPY --from=tools /out/skill-tools/document-to-markdown /app/skillshub/document-to-markdown/bin/document-to-markdown
COPY --from=tools /out/skill-tools/metabase-pp-cli /app/skillshub/metabase/bin/metabase-pp-cli
COPY skillshub/scripts/setup_officecli.py /app/skillshub/scripts/setup_officecli.py
COPY skillshub/officecli-runtime/release.json /app/skillshub/officecli-runtime/release.json
COPY scripts/install-container-higgsfield.py scripts/verify-container-skill-bins.py /app/scripts/
COPY containers/tools/higgsfield-release.json /app/containers/tools/higgsfield-release.json
RUN python3 /app/skillshub/scripts/setup_officecli.py && \
    python3 /app/scripts/install-container-higgsfield.py
USER magician
RUN /app/skillshub/.venv/bin/python /app/scripts/verify-container-skill-bins.py
