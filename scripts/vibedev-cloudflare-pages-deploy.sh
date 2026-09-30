#!/usr/bin/env bash
# Deploy a VibeDev static artifact to Cloudflare Pages Direct Upload.
#
# This is intentionally a thin, non-interactive wrapper around Wrangler. The
# Magician backend supplies an already-built output directory and a safe project
# slug; this script handles first-time Pages project creation and repeat deploys.
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/vibedev-cloudflare-pages-deploy.sh --output-dir DIR --project-name NAME [--branch BRANCH]

Required environment:
  CLOUDFLARE_ACCOUNT_ID        Cloudflare account id, to keep Wrangler non-interactive.
  CLOUDFLARE_PAGES_API_TOKEN   Preferred Cloudflare Pages API token.
  CLOUDFLARE_API_TOKEN         Fallback token name used by Wrangler and older configs.

Optional environment:
  WRANGLER_BIN           Path/name for wrangler. Defaults to wrangler, then npx --yes wrangler.
USAGE
}

log() { printf '%s\n' "$*" >&2; }
fail() { log "ERROR: $*"; exit 1; }

OUTPUT_DIR=""
PROJECT_NAME=""
BRANCH="${CLOUDFLARE_PAGES_BRANCH:-main}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-dir)
      [ "$#" -ge 2 ] || fail "--output-dir requires a value"
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --project-name|--site-slug)
      [ "$#" -ge 2 ] || fail "$1 requires a value"
      PROJECT_NAME="$2"
      shift 2
      ;;
    --branch)
      [ "$#" -ge 2 ] || fail "--branch requires a value"
      BRANCH="$2"
      shift 2
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

[ -n "$OUTPUT_DIR" ] || { usage; fail "--output-dir is required"; }
[ -n "$PROJECT_NAME" ] || { usage; fail "--project-name is required"; }
[ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ] || fail "CLOUDFLARE_ACCOUNT_ID is required"
if [ -n "${CLOUDFLARE_PAGES_API_TOKEN:-}" ]; then
  export CLOUDFLARE_API_TOKEN="$CLOUDFLARE_PAGES_API_TOKEN"
elif [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  fail "CLOUDFLARE_PAGES_API_TOKEN or CLOUDFLARE_API_TOKEN is required"
fi
[ -d "$OUTPUT_DIR" ] || fail "output directory does not exist: $OUTPUT_DIR"
[ -f "$OUTPUT_DIR/index.html" ] || fail "output directory must contain index.html: $OUTPUT_DIR"

case "$PROJECT_NAME" in
  *[!a-z0-9-]*|"")
    fail "project name must contain only lowercase letters, digits, and hyphens"
    ;;
esac
case "$BRANCH" in
  *[!A-Za-z0-9._/-]*|"")
    fail "branch must contain only letters, digits, dot, underscore, slash, and hyphen"
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

project_exists=false
if list_json="$("${WRANGLER[@]}" pages project list --json 2>/dev/null)"; then
  if printf '%s\n' "$list_json" | grep -q "\"name\"[[:space:]]*:[[:space:]]*\"${PROJECT_NAME}\""; then
    project_exists=true
  fi
fi

if [ "$project_exists" != true ]; then
  log "Ensuring Cloudflare Pages project exists: ${PROJECT_NAME}"
  create_output="$("${WRANGLER[@]}" pages project create "$PROJECT_NAME" --production-branch "$BRANCH" 2>&1)" || {
    if printf '%s\n' "$create_output" | grep -Eiq 'already exists|duplicate|taken'; then
      log "Cloudflare Pages project already exists: ${PROJECT_NAME}"
    else
      printf '%s\n' "$create_output" >&2
      exit 1
    fi
  }
fi

log "Deploying ${OUTPUT_DIR} to Cloudflare Pages project ${PROJECT_NAME} (${BRANCH})"
deploy_output="$("${WRANGLER[@]}" pages deploy "$OUTPUT_DIR" --project-name "$PROJECT_NAME" --branch "$BRANCH" --commit-dirty=true 2>&1)" || {
  printf '%s\n' "$deploy_output" >&2
  exit 1
}

public_url="$(printf '%s\n' "$deploy_output" \
  | grep -Eo "https://[^[:space:]\"'<>]+\\.pages\\.dev[^[:space:]\"'<>]*" \
  | tail -1 \
  | sed -E 's/[),.;]+$//')"

if [ -z "$public_url" ]; then
  public_url="https://${PROJECT_NAME}.pages.dev"
fi

printf '%s\n' "$deploy_output" >&2
printf 'VIBEDEV_PUBLIC_URL=%s\n' "$public_url"
