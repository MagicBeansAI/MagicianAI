# Repair/cache tool executables on top of an existing qualified runtime image.
# Builds only the document adapter. No Magician,
# Magicutor, desktop or UI build; no runtime data, accounts or credentials.
ARG RUNTIME_BASE=magician:runtime-base
FROM rust:1.95.0-bookworm AS tools
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev python3 perl ca-certificates && \
    rm -rf /var/lib/apt/lists/*
ENV CARGO_TARGET_DIR=/build/target CARGO_BUILD_JOBS=1 \
    CARGO_PROFILE_RELEASE_LTO=false
WORKDIR /build
COPY . .
ARG TARGETARCH
RUN --mount=type=cache,id=magician-tools-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=magician-tools-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=magician-tools-target-${TARGETARCH},target=/build/target,sharing=locked \
    make build-container-skill-tools

FROM ${RUNTIME_BASE}
USER root
# Repair cached bases too: never inherit the retired generated adapter.
RUN rm -rf /app/skillshub/metabase /app/skillshub/scripts/generated_tools.py
COPY skillshub/metabase-mcp/ /app/skillshub/metabase-mcp/
COPY skillshub/Makefile /app/skillshub/Makefile
COPY skillshub/scripts/migrate_metabase_mcp.py skillshub/scripts/stage_release.py skillshub/scripts/verify_runtime.py /app/skillshub/scripts/
COPY scripts/setup-skill-runtime.sh /app/scripts/setup-skill-runtime.sh
COPY magician_data_v3/system/agent_templates/agents/simple-data-analyst/definition.agent.yaml /app/magician_data_v3/system/agent_templates/agents/simple-data-analyst/definition.agent.yaml
COPY --from=tools /out/skill-tools/document-to-markdown /app/skillshub/document-to-markdown/bin/document-to-markdown
COPY skillshub/scripts/setup_officecli.py /app/skillshub/scripts/setup_officecli.py
COPY skillshub/officecli-runtime/release.json /app/skillshub/officecli-runtime/release.json
COPY scripts/install-container-higgsfield.py scripts/verify-container-skill-bins.py /app/scripts/
COPY containers/tools/higgsfield-release.json /app/containers/tools/higgsfield-release.json
RUN python3 /app/skillshub/scripts/setup_officecli.py && \
    python3 /app/scripts/install-container-higgsfield.py
USER magician
RUN /app/skillshub/.venv/bin/python /app/scripts/verify-container-skill-bins.py
