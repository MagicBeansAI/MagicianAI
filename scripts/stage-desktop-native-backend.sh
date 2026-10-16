#!/usr/bin/env bash
# Stage the existing native package artifact for inclusion in Magican Desktop.
# This never builds: callers decide when a reviewed release build is ready.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RESOURCE_DIR="${MAGICIAN_DESKTOP_NATIVE_RESOURCE_DIR:-$ROOT_DIR/desktop/src-tauri/native-backend}"
BUILD_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
TMP_ROOT="${MAGICIAN_PACKAGE_STAGE_TMP:-$BUILD_DIR/desktop-native-package-stage}"
PATH_FILE="$TMP_ROOT/package-path"
RESOURCE_FORMAT="${MAGICIAN_DESKTOP_NATIVE_RESOURCE_FORMAT:-directory}"

rm -rf "$TMP_ROOT"
mkdir -p "$TMP_ROOT" "$RESOURCE_DIR"

MAGICIAN_PACKAGE_DIR="$TMP_ROOT" \
MAGICIAN_PACKAGE_PATH_FILE="$PATH_FILE" \
  bash "$SCRIPT_DIR/package-release.sh"

archive="$(cat "$PATH_FILE")"
[[ -f "$archive" ]] || { echo "Native package staging did not produce an archive" >&2; exit 1; }
package_root="${archive%.tar.gz}"
[[ -d "$package_root" ]] || { echo "Native package staging did not preserve its package directory" >&2; exit 1; }

find "$RESOURCE_DIR" -mindepth 1 -maxdepth 1 ! -name README.md -exec rm -rf {} +
case "$RESOURCE_FORMAT" in
  directory)
    cp -R "$package_root"/. "$RESOURCE_DIR"/
    staged="$RESOURCE_DIR"
    ;;
  archive)
    cp "$archive" "$RESOURCE_DIR/"
    cp "$archive.sha256" "$RESOURCE_DIR/"
    staged="$RESOURCE_DIR/$(basename "$archive")"
    ;;
  *)
    echo "MAGICIAN_DESKTOP_NATIVE_RESOURCE_FORMAT must be directory or archive" >&2
    exit 2
    ;;
esac

verify_args=(--resource-dir "$RESOURCE_DIR")
[[ -z "${MAGICIAN_PACKAGE_TARGET:-}" ]] || verify_args+=(--target "$MAGICIAN_PACKAGE_TARGET")
[[ -z "${RELEASE_VERSION:-}" ]] || verify_args+=(--version "$RELEASE_VERSION")
[[ -z "${MAGICIAN_PACKAGE_EXPECTED_SIGNING:-}" ]] || verify_args+=(--signing "$MAGICIAN_PACKAGE_EXPECTED_SIGNING")
[[ -z "${MAGICIAN_PACKAGE_EXPECTED_COMMIT:-}" ]] || verify_args+=(--commit "$MAGICIAN_PACKAGE_EXPECTED_COMMIT")
bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" "${verify_args[@]}"

printf 'Staged Desktop native backend: %s\n' "$staged"
