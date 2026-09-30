#!/usr/bin/env bash
set -euo pipefail

if command -v lightpanda >/dev/null 2>&1; then
  echo "  ✓ Lightpanda already installed: $(command -v lightpanda)"
  lightpanda version
  exit 0
fi

if [ "$(uname -s)" = "Darwin" ]; then
  if ! command -v brew >/dev/null 2>&1; then
    echo "  ✗ Homebrew is required for the official Lightpanda macOS package" >&2
    echo "    Install it from https://brew.sh, then retry." >&2
    exit 1
  fi
  brew install lightpanda-io/browser/lightpanda
else
  echo "  ✗ Automatic Lightpanda setup is currently limited to macOS/Homebrew." >&2
  echo "    Install the official Linux nightly from:" >&2
  echo "    https://github.com/lightpanda-io/browser/releases/tag/nightly" >&2
  echo "    Then ensure lightpanda is on PATH or set LIGHTPANDA_EXECUTABLE_PATH." >&2
  exit 1
fi

lightpanda version
