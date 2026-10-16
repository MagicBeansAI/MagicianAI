#!/usr/bin/env bash
set -euo pipefail

ARTIFACTS=""
VERSION=""
TAG=""
REPO=""
OUTPUT="latest.json"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifacts) ARTIFACTS="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    *) printf 'ERROR: unknown argument %s\n' "$1" >&2; exit 2 ;;
  esac
done

for required in ARTIFACTS VERSION TAG REPO; do
  if [[ -z "${!required}" ]]; then
    printf 'ERROR: --%s is required\n' "$(printf '%s' "$required" | tr '[:upper:]_' '[:lower:]-')" >&2
    exit 2
  fi
done
command -v jq >/dev/null 2>&1 || { echo 'ERROR: jq is required' >&2; exit 2; }

resolve_platform() {
  local prefix="$1" artifact_name="$2" pattern="$3"
  local dir="$ARTIFACTS/$artifact_name" matches count file sig signature
  [[ -d "$dir" ]] || { printf 'ERROR: updater artifact directory is missing: %s\n' "$dir" >&2; exit 1; }

  matches="$(find "$dir" -type f -name "$pattern" ! -name '*.sig' -print)"
  count="$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')"
  [[ "$count" == "1" ]] || {
    printf 'ERROR: expected exactly one %s updater artifact in %s; found %s\n' "$pattern" "$dir" "$count" >&2
    exit 1
  }
  file="$matches"
  sig="$file.sig"
  [[ -s "$sig" ]] || { printf 'ERROR: updater signature is missing or empty: %s\n' "$sig" >&2; exit 1; }
  signature="$(tr -d '\r\n' < "$sig")"
  [[ -n "$signature" ]] || { printf 'ERROR: updater signature is empty: %s\n' "$sig" >&2; exit 1; }

  printf -v "${prefix}_FILE" '%s' "$(basename "$file")"
  printf -v "${prefix}_SIG" '%s' "$signature"
}

resolve_platform DARWIN_AARCH64 magician-desktop-macos-arm64-updater '*.app.tar.gz'
resolve_platform DARWIN_X86_64 magician-desktop-macos-x64-updater '*.app.tar.gz'
resolve_platform LINUX_X86_64 magician-desktop-linux-x64-updater '*.AppImage.tar.gz'
resolve_platform WINDOWS_X86_64 magician-desktop-windows-x64-updater '*.nsis.zip'

PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
BASE_URL="https://github.com/$REPO/releases/download/$TAG"

jq -n \
  --arg version "$VERSION" \
  --arg notes "Magician $VERSION release" \
  --arg pub_date "$PUB_DATE" \
  --arg darwin_aarch64_sig "$DARWIN_AARCH64_SIG" \
  --arg darwin_aarch64_url "$BASE_URL/$DARWIN_AARCH64_FILE" \
  --arg darwin_x86_64_sig "$DARWIN_X86_64_SIG" \
  --arg darwin_x86_64_url "$BASE_URL/$DARWIN_X86_64_FILE" \
  --arg linux_x86_64_sig "$LINUX_X86_64_SIG" \
  --arg linux_x86_64_url "$BASE_URL/$LINUX_X86_64_FILE" \
  --arg windows_x86_64_sig "$WINDOWS_X86_64_SIG" \
  --arg windows_x86_64_url "$BASE_URL/$WINDOWS_X86_64_FILE" \
  '{
    version: $version,
    notes: $notes,
    pub_date: $pub_date,
    platforms: {
      "darwin-aarch64": { signature: $darwin_aarch64_sig, url: $darwin_aarch64_url },
      "darwin-x86_64": { signature: $darwin_x86_64_sig, url: $darwin_x86_64_url },
      "linux-x86_64": { signature: $linux_x86_64_sig, url: $linux_x86_64_url },
      "windows-x86_64": { signature: $windows_x86_64_sig, url: $windows_x86_64_url }
    }
  }' > "$OUTPUT"

jq -e '
  (.platforms | keys | sort) == ["darwin-aarch64", "darwin-x86_64", "linux-x86_64", "windows-x86_64"] and
  ([.platforms[].signature | length > 0] | all) and
  ([.platforms[].url | startswith("https://")] | all)
' "$OUTPUT" >/dev/null
printf 'Wrote validated updater manifest: %s\n' "$OUTPUT"
