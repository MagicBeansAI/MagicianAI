# Add durable Linux pairing support to an existing runtime image. No service,
# desktop, UI or skill compilation; no runtime state or credentials are copied.
ARG RUNTIME_BASE=magician:runtime-base
FROM ${RUNTIME_BASE}
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
      dbus gnome-keyring libsecret-tools && \
    rm -rf /var/lib/apt/lists/*
COPY scripts/container-entrypoint.sh scripts/run-linux-keyring.py /app/scripts/
USER magician
RUN python3 -c "import shutil; assert all(shutil.which(p) for p in ('dbus-daemon', 'dbus-send', 'gnome-keyring-daemon', 'secret-tool'))"
