#!/usr/bin/env bash
# Preflight for launching the staged debug desktop app: the executable and
# every nested dylib must be loadable together under the hardened runtime,
# which means the same Apple Team ID (or all ad-hoc). Prints the reason and
# fails before launch, instead of letting dyld abort into a log nobody reads.
set -euo pipefail

app="${1:?usage: verify-desktop-debug-app.sh <Debug.app>}"
macos_dir="$app/Contents/MacOS"
frameworks_dir="$app/Contents/Frameworks"

if ! command -v codesign >/dev/null 2>&1; then
  exit 0
fi

team_of() {
  codesign --display --verbose=4 "$1" 2>&1 | awk -F= '$1 == "TeamIdentifier" {print $2}'
}

executable=$(ls "$macos_dir" 2>/dev/null | head -1)
if [[ -z "$executable" ]]; then
  echo "error: no executable under $macos_dir" >&2
  exit 1
fi
exe_team=$(team_of "$macos_dir/$executable")
status=0
shopt -s nullglob
for lib in "$frameworks_dir"/*.dylib; do
  lib_team=$(team_of "$lib")
  if [[ "$lib_team" != "$exe_team" ]]; then
    echo "error: $(basename "$lib") is signed for team '${lib_team:-ad-hoc}' but $executable for team '${exe_team:-ad-hoc}'; dyld will refuse to load it" >&2
    echo "       rebuild with 'make build-desktop-tray-debug' so both are signed together" >&2
    status=1
  fi
done
shopt -u nullglob
if ! codesign --verify --deep --strict "$app" 2>/dev/null; then
  echo "error: $app fails signature verification; rebuild with 'make build-desktop-tray-debug'" >&2
  status=1
fi
exit $status
