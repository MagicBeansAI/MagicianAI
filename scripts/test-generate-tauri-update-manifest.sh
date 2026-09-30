#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

make_artifact() {
  local dir="$1" file="$2" sig="$3"
  mkdir -p "$TMP_ROOT/$dir"
  printf 'artifact' > "$TMP_ROOT/$dir/$file"
  printf '%s\n' "$sig" > "$TMP_ROOT/$dir/$file.sig"
}

make_artifact magician-desktop-macos-arm64-updater Magician_aarch64.app.tar.gz sig-arm64
make_artifact magician-desktop-macos-x64-updater Magician_x86_64.app.tar.gz sig-x64
make_artifact magician-desktop-linux-x64-updater Magician_amd64.AppImage.tar.gz sig-linux
make_artifact magician-desktop-windows-x64-updater Magician_1.2.3_x64-setup.nsis.zip sig-windows

bash "$REPO_ROOT/scripts/generate-tauri-update-manifest.sh" \
  --artifacts "$TMP_ROOT" --version 1.2.3 --tag v1.2.3 \
  --repo magicbeanbs100x/magician --output "$TMP_ROOT/latest.json"

jq -e '.version == "1.2.3"' "$TMP_ROOT/latest.json" >/dev/null
jq -e '.platforms["windows-x86_64"].signature == "sig-windows"' "$TMP_ROOT/latest.json" >/dev/null
jq -e '.platforms["darwin-aarch64"].signature == "sig-arm64"' "$TMP_ROOT/latest.json" >/dev/null

: > "$TMP_ROOT/magician-desktop-linux-x64-updater/Magician_amd64.AppImage.tar.gz.sig"
if bash "$REPO_ROOT/scripts/generate-tauri-update-manifest.sh" \
  --artifacts "$TMP_ROOT" --version 1.2.3 --tag v1.2.3 \
  --repo magicbeanbs100x/magician --output "$TMP_ROOT/invalid.json" >/dev/null 2>&1; then
  echo 'ERROR: empty updater signature was accepted' >&2
  exit 1
fi

echo 'release manifest regression checks passed'
