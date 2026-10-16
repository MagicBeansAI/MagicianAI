#!/bin/sh
# install-container-tools.sh — single source of truth for container runtime tools
# Runs as root during Docker build (before USER directive).
set -eu

# Baseline shell toolbelt — these are the tools the embedded `shell` pack
# description (magician/src/magician_v2/execution/embedded_pack_defs/shell.yaml)
# advertises as "always available". Keep this list in sync with that YAML.
#
# Runtime base is debian:bookworm-slim (glibc), so packages install via apt.
# Unlike Alpine's busybox, debian ships GNU coreutils + grep/sed/findutils and
# an awk (mawk) as priority:required base packages, so we don't pull GNU text
# tools separately — the CRITICAL_BINS check still asserts grep/sed/awk/find.
#
# Grouped for review:
#   - Core utilities          (curl wget jq ripgrep bash coreutils git
#                              openssh-client ca-certificates)
#   - Language + build         (python3 python3-pip python3-venv nodejs npm
#                              build-essential pkg-config) — agents fall back to
#                              inline `python3 -c` / `node -e`; build tools +
#                              pkg-config are needed by Phase 2 skill dep builds.
#   - Media / PDF / fonts / OCR(imagemagick poppler-utils ffmpeg fonts-liberation
#                              tesseract-ocr) — resize/convert images, extract PDF
#                              text, A/V, a base font set for headless rendering,
#                              and OCR (the `ocr` skill shells out to tesseract;
#                              it defaults to `-l eng`). On bookworm `tesseract-ocr`
#                              hard-Depends on `tesseract-ocr-eng` + `-osd`, so the
#                              English language data is pulled even with
#                              --no-install-recommends — no extra `-eng` package.
#   - Chromium runtime libs    (libnss3 libatk-bridge2.0-0 libatk1.0-0 libgtk-3-0
#                              libgbm1 libasound2 libxshmfence1 libxdamage1
#                              libxcomposite1 libxfixes3 libxrandr2 libxkbcommon0
#                              libcups2 libpango-1.0-0 libpangocairo-1.0-0
#                              libdrm2 libxss1) — shared libs that real
#                              headless Chromium (obscura / Chrome-for-Testing,
#                              installed later) links against at runtime.
PACKAGES="ca-certificates curl wget jq ripgrep bash coreutils git openssh-client bubblewrap \
dbus gnome-keyring libsecret-tools \
python3 python3-pip python3-venv nodejs npm build-essential pkg-config \
imagemagick poppler-utils ffmpeg fonts-liberation tesseract-ocr \
libnss3 libatk-bridge2.0-0 libatk1.0-0 libgtk-3-0 libgbm1 libasound2 \
libxshmfence1 libxdamage1 libxcomposite1 libxfixes3 libxrandr2 libxkbcommon0 \
libcups2 libpango-1.0-0 libpangocairo-1.0-0 libdrm2 libxss1"

echo "==> Installing container tools: $PACKAGES"
export DEBIAN_FRONTEND=noninteractive
apt-get update && \
  apt-get install -y --no-install-recommends $PACKAGES && \
  rm -rf /var/lib/apt/lists/*

# Critical binaries that must be present for runtime operation. Mirrors
# the always-available list in shell.yaml so a missing baseline tool
# fails the image build instead of being discovered by an agent at
# runtime.
#
# `convert` (not `magick`): debian's `imagemagick` package installs the v6
# CLI whose canonical entrypoint is `convert`; the v7-style `magick` wrapper
# is not guaranteed on bookworm, so we assert the portable name.
#
# `tesseract`: assert it loudly here so a missing OCR binary fails the image
# build instead of letting `setup-ocr` fail-open (the `ocr` skill would then
# silently no-op at runtime).
CRITICAL_BINS="curl jq rg bash git python3 node npm grep sed awk find convert pdftotext tesseract dbus-daemon gnome-keyring-daemon secret-tool"

echo "==> Verifying critical binaries..."
fail=0
for bin in $CRITICAL_BINS; do
  if command -v "$bin" >/dev/null 2>&1; then
    echo "  OK: $bin  ($($bin --version 2>&1 | head -1))"
  else
    echo "  MISSING: $bin"
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "ERROR: One or more critical binaries are missing." >&2
  exit 1
fi

# Install uv (Python package manager) and marimo (notebook engine + data science libs).
#
# Install under a world-traversable prefix (/opt/uv) instead of /root/.local:
# the runtime runs as the non-root `magician` user, which cannot traverse /root
# (mode 0700), so uv + the marimo tool bin would be unreachable on PATH there.
#   - UV_INSTALL_DIR    : where the `uv`/`uvx` binaries land (install.sh)
#   - UV_TOOL_DIR       : where `uv tool install` materializes tool venvs
#   - UV_TOOL_BIN_DIR   : where `uv tool install` symlinks tool entrypoints
# The Dockerfile puts /opt/uv/bin on PATH and chmods /opt/uv a+rX afterwards.
echo "==> Installing uv and marimo..."
export UV_INSTALL_DIR="/opt/uv/bin"
export UV_TOOL_DIR="/opt/uv/tools"
export UV_TOOL_BIN_DIR="/opt/uv/bin"
mkdir -p "$UV_INSTALL_DIR" "$UV_TOOL_DIR"
export PATH="/opt/uv/bin:$PATH"
if ! command -v uv >/dev/null 2>&1; then
  curl -LsSf https://astral.sh/uv/install.sh | sh
fi

uv tool install marimo \
  --with pandas --with polars --with plotly --with matplotlib --with seaborn \
  --with numpy --with scipy --with duckdb --with openpyxl --with xlsxwriter \
  --with requests --with beautifulsoup4 --with lxml --with tabulate \
  || echo "  marimo install skipped (may already exist)"

# Make uv + the materialized tool venvs/bins traversable + readable for the
# non-root runtime user (the Dockerfile also repeats this after the build).
chmod -R a+rX /opt/uv || true

if command -v marimo >/dev/null 2>&1; then
  echo "  OK: marimo  ($(marimo --version 2>&1 | head -1))"
else
  echo "  WARN: marimo not in PATH (may need shell restart)"
fi

echo "==> All container tools installed and verified."
