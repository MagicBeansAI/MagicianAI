#!/usr/bin/env bash
# Shared macOS source installation for shell, TUI and Desktop setup.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-$HOME/MagicianNotes}"
PREFIX="${MAGICIAN_PREFIX:-$HOME/.magician}"
PHASE=all
DRY_RUN=0
DESKTOP_RUNNING=0

usage() {
  cat <<'USAGE'
Usage: install-source-checkout.sh [--data-dir PATH] [--prefix PATH]
       [--phase prepare|start|verify|all]
       [--desktop-running] [--dry-run]

Prepare installs software, builds once, verifies and installs the native package.
Start restarts Desktop (unless this is its installer) and starts the backend service.
Verify checks required services and the local memory model. Dry run changes nothing.
USAGE
}
while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir|--prefix|--phase)
      [ $# -ge 2 ] || { echo "Missing value for $1" >&2; exit 2; }
      case "$1" in
        --data-dir) DATA_DIR="$2";;
        --prefix) PREFIX="$2";;
        --phase) PHASE="$2";;
      esac
      shift 2;;
    --dry-run) DRY_RUN=1; shift;;
    --desktop-running) DESKTOP_RUNNING=1; shift;;
    --help|-h) usage; exit 0;;
    *) echo "Unknown source-install argument: $1" >&2; exit 2;;
  esac
done
case "$PHASE" in prepare|start|verify|all) ;; *) echo "Unknown source-install phase: $PHASE" >&2; exit 2;; esac
case "$DATA_DIR" in /*) ;; *) echo "Runtime data directory must be absolute" >&2; exit 2;; esac
case "$PREFIX" in /*) ;; *) echo "Native install prefix must be absolute" >&2; exit 2;; esac
if [ "$DATA_DIR" = "$PREFIX" ]; then
  echo "Runtime data and installed software need separate directories" >&2; exit 2
fi
if [ ! -f "$REPO/magician-bin/Cargo.toml" ] || [ ! -f "$REPO/Makefile" ]; then
  echo "Source installation needs a Magician checkout" >&2; exit 1
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "Source installation supports Apple Silicon Macs on macOS 14 or newer" >&2; exit 1
fi
major="$(sw_vers -productVersion | cut -d. -f1)"
case "$major" in ''|*[!0-9]*) echo "Could not determine macOS version" >&2; exit 1;; esac
[ "$major" -ge 14 ] || { echo "Source installation needs macOS 14 or newer" >&2; exit 1; }
python3 "$REPO/scripts/native-runtime-service.py" validate --prefix "$PREFIX" --data-dir "$DATA_DIR"
export MAGICIAN_ROOT_DIR="$DATA_DIR" MAGICIAN_PREFIX="$PREFIX"
export MAGICIAN_INSTALL_MODE=dev MAGICIAN_INSTALL_FLOW=local MAGICIAN_PREREQS_SLIM=0

run() {
  if [ "$DRY_RUN" = 1 ]; then
    printf 'would run:'; printf ' %q' "$@"; printf '\n'
  else
    "$@"
  fi
}
prepare() {
  echo "Preparing macOS source installation (debug): $PREFIX; data: $DATA_DIR"
  run make -C "$REPO" setup-prerequisites
  # MLX needs Rust 1.95+. Keep its compiler for every source-install build;
  # switching compilers in one managed target invalidates cached dependencies.
  if [ "${DECISION_ENGINE_MLX:-1}" = 1 ]; then
    source_toolchain="${MLX_RUST_TOOLCHAIN:-1.95.0}"
    run make -C "$REPO" setup-decision-engine-mlx-build "MLX_RUST_TOOLCHAIN=$source_toolchain"
    export MLX_RUST_TOOLCHAIN="$source_toolchain" RUSTUP_TOOLCHAIN="$source_toolchain"
  fi
  run python3 "$REPO/scripts/native-runtime-service.py" seed --checkout "$REPO" --data-dir "$DATA_DIR"
  run python3 "$REPO/scripts/configure-cloud-services.py" --from-environment
  run make -C "$REPO" setup-pi-coding-agent
  run make -C "$REPO" setup-skill-runtime setup-bot-runtime setup-media-tools setup-bundled-browser
  run make -C "$REPO" setup-ollama-embedding
  run make -C "$REPO" build-source-debug
  if [ "$DRY_RUN" = 1 ]; then
    run make -C "$REPO" package-source PROFILE=debug
    echo "would verify and install that package into $PREFIX without replacing runtime data"
    return
  fi
  path_file="$(mktemp -t magician-source-package)"
  trap 'rm -f "$path_file"' EXIT
  make -C "$REPO" package-source PROFILE=debug "MAGICIAN_PACKAGE_PATH_FILE=$path_file"
  archive="$(cat "$path_file")"
  case "$archive" in *.tar.gz) ;; *) echo "Packager did not return a native archive" >&2; exit 1;; esac
  package="${archive%.tar.gz}"
  [ -f "$package/install.sh" ] && [ -f "$package/SHA256SUMS" ] \
    || { echo "Packager did not produce a complete package" >&2; exit 1; }
  bash "$package/install.sh" --yes --prefix "$PREFIX"
}
start() {
  if [ "$DESKTOP_RUNNING" != 1 ]; then
    run python3 "$REPO/scripts/native-runtime-service.py" check-desktop --data-dir "$DATA_DIR"
  fi
  run python3 "$REPO/scripts/native-runtime-service.py" start --prefix "$PREFIX" --data-dir "$DATA_DIR"
  if [ "$DESKTOP_RUNNING" != 1 ]; then
    run make -C "$REPO" restart-desktop-tray-debug
  fi
}
verify() {
  run bash "$REPO/scripts/install-verify.sh" --core --wait 120 --native-prefix "$PREFIX"
}
case "$PHASE" in
  prepare) prepare;;
  start) start;;
  verify) verify;;
  all) prepare; start; verify; run python3 "$REPO/scripts/configure-cloud-services.py";;
esac
if [ "$DRY_RUN" = 1 ]; then
  echo "Dry run complete. No runtime files, packages or services were changed."
fi
