#!/usr/bin/env bash
# Locate and validate the native backend inside a finished Tauri bundle.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE=""
PLATFORM=""
VERIFY_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle) BUNDLE="${2:?--bundle needs a path}"; shift 2 ;;
    --platform) PLATFORM="${2:?--platform needs macos, linux, or windows}"; shift 2 ;;
    --target|--version|--signing|--commit)
      VERIFY_ARGS+=("$1" "${2:?$1 needs a value}"); shift 2 ;;
    --gatekeeper) VERIFY_ARGS+=("$1"); shift ;;
    --help|-h)
      echo "Usage: $0 --bundle PATH --platform macos|linux|windows [native verifier options]"; exit 0 ;;
    *) echo "ERROR: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ -n "$BUNDLE" && -e "$BUNDLE" ]] || { echo "ERROR: finished Desktop bundle is missing: $BUNDLE" >&2; exit 1; }
[[ "$PLATFORM" == macos || "$PLATFORM" == linux || "$PLATFORM" == windows ]] || { echo "ERROR: --platform must be macos, linux, or windows" >&2; exit 2; }

TMP_ROOT=""
cleanup() { [[ -z "$TMP_ROOT" ]] || rm -rf "$TMP_ROOT"; }
trap cleanup EXIT

if [[ "$PLATFORM" == macos ]]; then
  SEARCH_ROOT="$BUNDLE/Contents/Resources"
elif [[ -d "$BUNDLE" ]]; then
  # Test/diagnostic path for an already-extracted AppImage or NSIS root.
  SEARCH_ROOT="$BUNDLE"
elif [[ "$PLATFORM" == windows ]]; then
  command -v 7z >/dev/null 2>&1 || { echo "ERROR: 7z is required to inspect a Windows NSIS installer" >&2; exit 1; }
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/magician-nsis-verify.XXXXXX")"
  BUNDLE="$(cd "$(dirname "$BUNDLE")" && pwd)/$(basename "$BUNDLE")"
  (cd "$TMP_ROOT" && 7z x -y "$BUNDLE" >/dev/null)
  SEARCH_ROOT="$TMP_ROOT"
else
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/magician-appimage-verify.XXXXXX")"
  BUNDLE="$(cd "$(dirname "$BUNDLE")" && pwd)/$(basename "$BUNDLE")"
  (cd "$TMP_ROOT" && "$BUNDLE" --appimage-extract >/dev/null)
  SEARCH_ROOT="$TMP_ROOT/squashfs-root"
fi

[[ -d "$SEARCH_ROOT" ]] || { echo "ERROR: bundle resource root is missing: $SEARCH_ROOT" >&2; exit 1; }
resource_dirs=()
while IFS= read -r directory; do
  [[ -n "$directory" ]] && resource_dirs+=("$directory")
done < <(find "$SEARCH_ROOT" -type d -name native-backend -print | sort)
[[ ${#resource_dirs[@]} -eq 1 ]] || { echo "ERROR: expected one bundled native-backend directory, found ${#resource_dirs[@]}" >&2; exit 1; }

bash "$SCRIPT_DIR/verify-desktop-native-backend.sh" \
  --resource-dir "${resource_dirs[0]}" "${VERIFY_ARGS[@]}"
printf 'Finished Desktop bundle contains the verified native backend.\n'
