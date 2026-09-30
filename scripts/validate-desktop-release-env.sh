#!/usr/bin/env bash
set -euo pipefail

platform="${1:-${RUNNER_OS:-}}"

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    printf 'ERROR: required release setting %s is missing\n' "$name" >&2
    return 1
  fi
}

require_env TAURI_UPDATER_PUBLIC_KEY
require_env TAURI_SIGNING_PRIVATE_KEY

if [[ "$platform" == "macOS" || "$platform" == "Darwin" ]]; then
  require_env APPLE_CERTIFICATE
  require_env APPLE_CERTIFICATE_PASSWORD
  require_env APPLE_SIGNING_IDENTITY
  require_env APPLE_API_ISSUER
  require_env APPLE_API_KEY
  require_env APPLE_API_KEY_CONTENT
fi

printf 'Release signing inputs are configured for %s.\n' "${platform:-this platform}"
