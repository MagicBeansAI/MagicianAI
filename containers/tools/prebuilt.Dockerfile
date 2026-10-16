# Package already qualified Linux tools without compilers or downloads.
# Context: five executables + SHA256SUMS in prebuilt-skill-tools/, and the
# source files copied below. Never use a runtime-data directory as context.
ARG RUNTIME_BASE=magician:runtime-base
FROM ${RUNTIME_BASE}
USER root
RUN --mount=type=bind,source=prebuilt-skill-tools,target=/prebuilt,readonly \
    cd /prebuilt && sha256sum --strict --check SHA256SUMS && \
    install -D -m 0755 agent-browser /app/skillshub/browser/bin/agent-browser && \
    install -D -m 0755 document-to-markdown /app/skillshub/document-to-markdown/bin/document-to-markdown && \
    install -D -m 0755 metabase-pp-cli /app/skillshub/metabase/bin/metabase-pp-cli && \
    install -D -m 0755 higgsfield /app/skillshub/higgsfield/bin/higgsfield && \
    install -D -m 0755 officecli /app/skillshub/officecli-runtime/bin/officecli && \
    for skill in office-excel office-powerpoint office-word; do \
      mkdir -p /app/skillshub/$skill/bin && \
      ln -sfn ../../officecli-runtime/bin/officecli /app/skillshub/$skill/bin/officecli; \
    done
COPY scripts/verify-container-skill-bins.py /app/scripts/
COPY skillshub/macos-ui-automation/SKILL.md /app/skillshub/macos-ui-automation/SKILL.md
COPY skillshub/macos-ui-automation/bin/macos-ui-controller /app/skillshub/macos-ui-automation/bin/macos-ui-controller
USER magician
RUN /app/skillshub/.venv/bin/python /app/scripts/verify-container-skill-bins.py
