#!/bin/bash
# Ensures foundational CLI tools are available.
# Called by desktop setup AND by `make setup-prerequisites`.
# Idempotent — skips anything already installed.
# macOS only (uses Homebrew).
set -euo pipefail

# SLIM mode: install only the run-time essentials (brew + git + cloudflared +
# container-runtime deps), and SKIP the native-skill build deps
# (node/python3/uv/imagemagick/poppler/ffmpeg/protobuf). A run-only / container user gets those
# baked into the image, so installing them on the host is redundant.
# Toggle via MAGICIAN_PREREQS_SLIM=1 or the --slim flag.
SLIM="${MAGICIAN_PREREQS_SLIM:-0}"

# The build toolchain (make + rust) is only needed for the MODE=dev + FLOW=local
# combo (the one that compiles binaries on the host). install.sh passes these
# through; every other combo fetches/runs an image and skips ensure_make/ensure_rust.
INSTALL_MODE="${MAGICIAN_INSTALL_MODE:-}"
INSTALL_FLOW="${MAGICIAN_INSTALL_FLOW:-}"

while [ $# -gt 0 ]; do case "$1" in
  --slim) SLIM=1; shift;;
  *) echo "unknown arg: $1" >&2; exit 2;;
esac; done

# Resolve the repo root so ensure_make can delegate to the sibling helper.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ensure_brew() {
  if command -v brew &>/dev/null; then echo "  ✓ brew"; return 0; fi
  echo "  Installing Homebrew..."
  NONINTERACTIVE=1 /bin/bash -c \
    "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
}

ensure_git() {
  if command -v git &>/dev/null; then echo "  ✓ git $(git --version)"; return 0; fi
  echo "  Installing git..."
  brew install git
}

ensure_node() {
  if command -v node &>/dev/null && command -v npm &>/dev/null; then
    echo "  ✓ node $(node --version), npm $(npm --version)"; return 0
  fi
  echo "  Installing Node.js..."
  brew install node
}

ensure_python3() {
  if command -v python3 &>/dev/null; then
    echo "  ✓ python3 $(python3 --version 2>&1)"; return 0
  fi
  echo "  Installing Python 3..."
  brew install python@3
}

ensure_uv() {
  if command -v uv &>/dev/null; then echo "  ✓ uv $(uv --version)"; return 0; fi
  echo "  Installing uv..."
  curl -LsSf https://astral.sh/uv/install.sh | sh
}

# Baseline CLI tools advertised by the embedded `shell` pack
# (see magician/src/magician_v2/execution/embedded_pack_defs/shell.yaml).
# macOS dev parity with the Linux container — keeps agents from
# scripting against tools that exist in prod but not on the operator's
# laptop while iterating.
ensure_jq() {
  if command -v jq &>/dev/null; then echo "  ✓ jq $(jq --version)"; return 0; fi
  echo "  Installing jq..."
  brew install jq
}

ensure_ripgrep() {
  if command -v rg &>/dev/null; then echo "  ✓ rg $(rg --version | head -1)"; return 0; fi
  echo "  Installing ripgrep..."
  brew install ripgrep
}

ensure_imagemagick() {
  if command -v magick &>/dev/null; then
    echo "  ✓ imagemagick $(magick -version | head -1)"; return 0
  fi
  echo "  Installing imagemagick..."
  brew install imagemagick
}

ensure_poppler() {
  if command -v pdftotext &>/dev/null; then
    echo "  ✓ poppler $(pdftotext -v 2>&1 | head -1)"; return 0
  fi
  echo "  Installing poppler (pdftotext, pdfinfo)..."
  brew install poppler
}

# Media editing and its real subprocess regressions use the same host ffmpeg
# and ffprobe pair that the production media-edit runtime resolves.
ensure_ffmpeg() {
  if command -v ffmpeg &>/dev/null && ffmpeg -version &>/dev/null && \
     command -v ffprobe &>/dev/null && ffprobe -version &>/dev/null; then
    echo "  ✓ ffmpeg $(ffmpeg -version 2>/dev/null | head -1)"; return 0
  fi
  echo "  Installing ffmpeg..."
  brew install ffmpeg
  if ! command -v ffmpeg &>/dev/null || ! ffmpeg -version &>/dev/null || \
     ! command -v ffprobe &>/dev/null || ! ffprobe -version &>/dev/null; then
    echo "  ✗ Homebrew completed, but ffmpeg and ffprobe are not both runnable" >&2
    return 1
  fi
  echo "  ✓ ffmpeg $(ffmpeg -version 2>/dev/null | head -1)"
}

# LanceDB's lance-encoding and lance-file crates compile protobuf schemas in
# their build scripts, so a fresh local-development checkout needs protoc before
# the first Cargo check/build.
ensure_protoc() {
  if command -v protoc &>/dev/null && protoc --version &>/dev/null; then
    echo "  ✓ protoc $(protoc --version)"; return 0
  fi
  echo "  Installing protobuf (protoc)..."
  brew install protobuf
}

# cloudflared powers the public Cloudflare Tunnel for Kapso webhook ingress
# (scripts/ensure-magician-tunnel.sh). It is a Homebrew *formula* (not a cask),
# CLI-only — no GUI app to open. First use is a one-time interactive login +
# tunnel create, which ensure-magician-tunnel.sh prints when needed.
ensure_cloudflared() {
  if command -v cloudflared &>/dev/null; then
    echo "  ✓ cloudflared $(cloudflared --version 2>/dev/null | head -1)"; return 0
  fi
  echo "  Installing cloudflared..."
  brew install cloudflared
  echo "  ⚠ First public tunnel needs a one-time login: cloudflared tunnel login (see scripts/ensure-magician-tunnel.sh)"
}

# --- build toolchain (MODE=dev + FLOW=local only) ---------------------------
# ensure_make: GNU make is required for the host build (make build-all-debug).
# Delegate to the sibling scripts/ensure_make.sh when present (it handles the
# Xcode-CLT install on macOS + apt/dnf/pacman/zypper on Linux); else fall back to
# the package manager directly (brew on macOS, apt on Linux).
ensure_make() {
  if command -v make &>/dev/null; then echo "  ✓ make $(make --version 2>/dev/null | head -1)"; return 0; fi
  if [ -f "$SCRIPT_DIR/ensure_make.sh" ]; then
    echo "  Ensuring make (scripts/ensure_make.sh)..."
    bash "$SCRIPT_DIR/ensure_make.sh"
    return $?
  fi
  echo "  Installing make..."
  if [ "$(uname -s)" = Darwin ]; then
    brew install make
  elif command -v apt-get &>/dev/null; then
    sudo apt-get update && sudo apt-get install -y make
  else
    echo "  ⚠ could not auto-install make — install it manually, then re-run." >&2
    return 1
  fi
}

# ensure_rust: the host build needs the Rust toolchain (cargo via rustup). We
# only CHECK for it and print a clear install hint — we deliberately do NOT pipe
# rustup-init unprompted (it mutates the user's shell profile + PATH).
ensure_rust() {
  if command -v rustup &>/dev/null && command -v cargo &>/dev/null; then
    echo "  ✓ rust $(rustc --version 2>/dev/null || echo 'toolchain present'), cargo $(cargo --version 2>/dev/null | head -1)"
    return 0
  fi
  if command -v cargo &>/dev/null; then
    echo "  ✓ cargo $(cargo --version 2>/dev/null | head -1) (rustup not found — toolchain managed externally)"
    return 0
  fi
  echo "  ⚠ Rust toolchain (rustup + cargo) not found — required for the MODE=dev FLOW=local build." >&2
  echo "    Install it with:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  echo "    then re-run this installer (open a new shell or 'source \$HOME/.cargo/env' first)." >&2
  return 1
}

echo "Checking prerequisites..."
# Always install: brew + git + cloudflared + the shell-pack CLI essentials
# (jq/ripgrep) + the container runtime (bootstrapped by the caller, but brew is
# its prerequisite). These are needed regardless of how skills run.
ensure_brew
ensure_git
ensure_jq
ensure_ripgrep
if [ "$SLIM" = 1 ]; then
  # Run-only / container user: node/python3/uv/imagemagick/poppler/ffmpeg/protobuf are
  # native-skill BUILD deps — redundant on the host because the image bakes them.
  echo "  (slim mode) skipping native build deps: node, python3, uv, imagemagick, poppler, ffmpeg, protobuf"
else
  ensure_node
  ensure_python3
  ensure_uv
  ensure_imagemagick
  ensure_poppler
  ensure_ffmpeg
  ensure_protoc
fi
ensure_cloudflared

# Build toolchain — ONLY for the host-build combo (MODE=dev + FLOW=local). Every
# other combo (user mode, or any container flow) fetches/runs a prebuilt image and
# never compiles on the host, so make/rust are not required there. Non-fatal: a
# missing toolchain is surfaced loudly but does not abort the slim prereq pass.
if [ "$INSTALL_MODE" = dev ] && [ "$INSTALL_FLOW" = local ]; then
  echo "  (dev + local) ensuring host build toolchain: make, rust"
  ensure_make || echo "  ⚠ ensure_make reported a problem (the host build needs make)" >&2
  ensure_rust || echo "  ⚠ ensure_rust reported a problem (the host build needs cargo/rustup)" >&2
fi

echo "All prerequisites ready."
