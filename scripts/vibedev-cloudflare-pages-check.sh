#!/usr/bin/env bash
# Validate the Cloudflare Pages credentials used by VibeDev static publishing.
#
# Default mode is non-mutating: load local runtime env, confirm credentials are
# present, and ask Wrangler to list Pages projects for the configured account.
# Pass --publish-smoke to create/deploy a tiny Pages project as an end-to-end
# smoke test.
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/vibedev-cloudflare-pages-check.sh [options]

Options:
  --project-name NAME     Also report whether this Pages project exists.
  --publish-smoke         Publish a tiny smoke artifact through the deploy wrapper.
  --smoke-project NAME    Pages project name for --publish-smoke.
                          Defaults to magician-vibedev-smoke.
  --branch BRANCH         Branch for smoke deploy. Defaults to CLOUDFLARE_PAGES_BRANCH or main.
  --env-file FILE         Source an env file before checking. Can be repeated.
  --no-default-env        Do not source ~/MagicianNotes/.env.development and .env.
  --keep-smoke-dir        Keep the temporary smoke artifact directory.
  --help, -h              Show this help.

Required environment, after env files are loaded:
  CLOUDFLARE_ACCOUNT_ID        Cloudflare account id.
  CLOUDFLARE_PAGES_API_TOKEN   Preferred Pages token.
  CLOUDFLARE_API_TOKEN         Fallback token used by Wrangler.

Optional environment:
  WRANGLER_BIN                 Path/name for wrangler. Defaults to wrangler, then npx --yes wrangler.

Notes:
  Default mode does not create Cloudflare resources. --publish-smoke intentionally creates
  or updates the smoke Pages project and deploys a small index.html.
USAGE
}

log() { printf '%s\n' "$*" >&2; }
fail() { log "ERROR: $*"; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNTIME_ROOT="${MAGICIAN_ROOT_DIR:-${MAGICIAN_RUNTIME_ROOT:-${HOME}/MagicianNotes}}"

PROJECT_NAME=""
PUBLISH_SMOKE=false
SMOKE_PROJECT="${CLOUDFLARE_PAGES_SMOKE_PROJECT:-magician-vibedev-smoke}"
BRANCH="${CLOUDFLARE_PAGES_BRANCH:-main}"
LOAD_DEFAULT_ENV=true
KEEP_SMOKE_DIR=false
ENV_FILES=()

while [ "$#" -gt 0 ]; do
  case "$1" in
    --project-name)
      [ "$#" -ge 2 ] || fail "--project-name requires a value"
      PROJECT_NAME="$2"
      shift 2
      ;;
    --publish-smoke)
      PUBLISH_SMOKE=true
      shift
      ;;
    --smoke-project)
      [ "$#" -ge 2 ] || fail "--smoke-project requires a value"
      SMOKE_PROJECT="$2"
      shift 2
      ;;
    --branch)
      [ "$#" -ge 2 ] || fail "--branch requires a value"
      BRANCH="$2"
      shift 2
      ;;
    --env-file)
      [ "$#" -ge 2 ] || fail "--env-file requires a value"
      ENV_FILES+=("$2")
      shift 2
      ;;
    --no-default-env)
      LOAD_DEFAULT_ENV=false
      shift
      ;;
    --keep-smoke-dir)
      KEEP_SMOKE_DIR=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

source_env_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  # Trusted local operator env files; sourced so quoted values and exports work.
  set -a
  # shellcheck disable=SC1090
  . "$file"
  set +a
  log "Loaded env: ${file}"
}

if [ "$LOAD_DEFAULT_ENV" = true ]; then
  source_env_file "${RUNTIME_ROOT}/.env.development"
  source_env_file "${RUNTIME_ROOT}/.env"
fi

if [ "${#ENV_FILES[@]}" -gt 0 ]; then
  for env_file in "${ENV_FILES[@]}"; do
    source_env_file "$env_file"
  done
fi

[ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ] || fail "CLOUDFLARE_ACCOUNT_ID is required"
if [ -n "${CLOUDFLARE_PAGES_API_TOKEN:-}" ]; then
  export CLOUDFLARE_API_TOKEN="$CLOUDFLARE_PAGES_API_TOKEN"
elif [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  fail "CLOUDFLARE_PAGES_API_TOKEN or CLOUDFLARE_API_TOKEN is required"
fi

case "$PROJECT_NAME" in
  *[!a-z0-9-]*)
    fail "--project-name must contain only lowercase letters, digits, and hyphens"
    ;;
esac
case "$SMOKE_PROJECT" in
  *[!a-z0-9-]*|"")
    fail "--smoke-project must contain only lowercase letters, digits, and hyphens"
    ;;
esac
case "$BRANCH" in
  *[!A-Za-z0-9._/-]*|"")
    fail "--branch must contain only letters, digits, dot, underscore, slash, and hyphen"
    ;;
esac

if [ -n "${WRANGLER_BIN:-}" ]; then
  WRANGLER=("$WRANGLER_BIN")
elif command -v wrangler >/dev/null 2>&1; then
  WRANGLER=(wrangler)
elif command -v npx >/dev/null 2>&1; then
  WRANGLER=(npx --yes wrangler)
else
  fail "wrangler is not installed and npx is unavailable. Install with: npm install -g wrangler"
fi

export CI=true
export NO_COLOR=1
export FORCE_COLOR=0

mask_account_id() {
  local value="$1"
  local len="${#value}"
  if [ "$len" -le 10 ]; then
    printf '%s\n' "$value"
  else
    printf '%s...%s\n' "${value:0:6}" "${value: -4}"
  fi
}

log "Checking Cloudflare Pages access for account $(mask_account_id "$CLOUDFLARE_ACCOUNT_ID")"
if ! list_json="$("${WRANGLER[@]}" pages project list --json 2>&1)"; then
  printf '%s\n' "$list_json" >&2
  fail "Cloudflare Pages project list failed; check account id, token scopes, and network access"
fi
log "Cloudflare Pages project list succeeded."

if [ -n "$PROJECT_NAME" ]; then
  if printf '%s\n' "$list_json" | grep -q "\"name\"[[:space:]]*:[[:space:]]*\"${PROJECT_NAME}\""; then
    log "Pages project exists: ${PROJECT_NAME}"
  else
    log "Pages project not found yet: ${PROJECT_NAME}"
  fi
fi

if [ "$PUBLISH_SMOKE" != true ]; then
  printf 'CLOUDFLARE_PAGES_CHECK=ok\n'
  exit 0
fi

SMOKE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/magician-vibedev-cf-pages-smoke.XXXXXX")"
cleanup() {
  if [ "${KEEP_SMOKE_DIR}" != true ]; then
    rm -rf "$SMOKE_DIR"
  else
    log "Kept smoke artifact directory: ${SMOKE_DIR}"
  fi
}
trap cleanup EXIT

cat >"${SMOKE_DIR}/index.html" <<HTML
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Magician VibeDev Cloudflare Pages Smoke</title>
    <meta name="viewport" content="width=device-width, initial-scale=1">
  </head>
  <body>
    <main>
      <h1>Magician VibeDev Cloudflare Pages smoke check</h1>
      <p>Generated at $(date -u '+%Y-%m-%dT%H:%M:%SZ').</p>
    </main>
  </body>
</html>
HTML

log "Publishing smoke artifact to Cloudflare Pages project ${SMOKE_PROJECT}"
"${SCRIPT_DIR}/vibedev-cloudflare-pages-deploy.sh" \
  --output-dir "$SMOKE_DIR" \
  --project-name "$SMOKE_PROJECT" \
  --branch "$BRANCH"
