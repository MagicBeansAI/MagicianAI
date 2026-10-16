#!/usr/bin/env bash
# Publish the Magican anatomy showcase (showcase/anatomy) to Cloudflare Pages
# at explain.magican.ai. Same credentials and deploy helper as the marketing
# site (~/MagicianNotes/.env, scripts/vibedev-cloudflare-pages-deploy.sh).
#
# Usage:
#   scripts/publish-showcase.sh [--preview] [--no-narration | --no-media] [--dry-run]
#
#   --preview        deploy to the publicly accessible, noindex "preview" branch;
#                    the custom domain is not touched
#   --no-narration   ship the music bed but none of the voice clips (the tour
#                    runs with captions; every clip is optional in the page)
#   --no-media       ship no audio at all
#   --dry-run        build showcase/anatomy/dist/ and report what would be
#                    uploaded, then stop
#
# The pages and audio are in Git LFS: a checkout without `git lfs pull` has
# pointer files instead, and this script refuses to publish those.

set -euo pipefail

log() { printf '%s\n' "$*" >&2; }
fail() { log "ERROR: $*"; exit 1; }

PROJECT_NAME="magican-explain"
DOMAIN="explain.magican.ai"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHOWCASE="$REPO_ROOT/showcase/anatomy"
OUTPUT_DIR="$SHOWCASE/dist"

BRANCH="main"
PREVIEW=false
DRY_RUN=false
SITE_ARGS=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --preview) PREVIEW=true; BRANCH="preview" ;;
    --no-narration) SITE_ARGS+=(--no-narration) ;;
    --no-media) SITE_ARGS+=(--no-media) ;;
    --dry-run) DRY_RUN=true ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
  shift
done

command -v node >/dev/null 2>&1 || fail "node is required (it builds dist/)"

# LFS pointers are a few lines of text, not pages or audio
for page in explorer.html; do
  [ -f "$SHOWCASE/$page" ] || fail "missing showcase/anatomy/$page"
  if head -c 40 "$SHOWCASE/$page" | grep -q "git-lfs.github.com"; then
    fail "showcase/anatomy/$page is a Git LFS pointer. Run: git lfs pull --include 'showcase/anatomy/**'"
  fi
done

log "🧱 Building ${OUTPUT_DIR#$REPO_ROOT/} ..."
if [ "$PREVIEW" = true ]; then SITE_ARGS+=(--preview); fi
node "$SHOWCASE/explorer/site.mjs" ${SITE_ARGS[@]+"${SITE_ARGS[@]}"}
VERIFY_ARGS=()
[ "$PREVIEW" = false ] || VERIFY_ARGS+=(--preview)
python3 "$REPO_ROOT/scripts/verify_public_site.py" "$OUTPUT_DIR" explain ${VERIFY_ARGS[@]+"${VERIFY_ARGS[@]}"}

files=$(find "$OUTPUT_DIR" -type f | wc -l | tr -d ' ')
size=$(du -sh "$OUTPUT_DIR" | cut -f1)
largest=$(find "$OUTPUT_DIR" -type f -exec ls -l {} + | sort -k5 -n | tail -1 | awk '{printf "%.1f MB %s", $5/1048576, $NF}')
log "   ${files} files, ${size// /}; largest: ${largest/$OUTPUT_DIR\//}"
# Cloudflare Pages limits: 20,000 files per deployment, 25 MiB per file
[ "$files" -le 20000 ] || fail "too many files for Cloudflare Pages (${files} > 20000)"
if find "$OUTPUT_DIR" -type f -size +25M | grep -q .; then
  fail "a file is over Cloudflare Pages' 25 MiB limit: $(find "$OUTPUT_DIR" -type f -size +25M | head -1)"
fi

if [ "$DRY_RUN" = true ]; then
  target="publish-showcase"
  [ "$PREVIEW" = true ] && target="publish-showcase-preview"
  log "Dry run: nothing uploaded. Deploy with: make ${target}"
  exit 0
fi

# Load local environment (same as the marketing site and vibedev)
for env_file in ~/MagicianNotes/.env.development ~/MagicianNotes/.env; do
  if [ -f "$env_file" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$env_file"
    set +a
  fi
done
[ -n "${CLOUDFLARE_ACCOUNT_ID:-}" ] || fail "CLOUDFLARE_ACCOUNT_ID is missing from ~/MagicianNotes/.env"
if [ -n "${CLOUDFLARE_PAGES_API_TOKEN:-}" ]; then
  export CLOUDFLARE_API_TOKEN="$CLOUDFLARE_PAGES_API_TOKEN"
elif [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  fail "CLOUDFLARE_PAGES_API_TOKEN or CLOUDFLARE_API_TOKEN is missing from ~/MagicianNotes/.env"
fi

log "🚀 Publishing to Cloudflare Pages project ${PROJECT_NAME} (${BRANCH})..."
deploy_out=$(bash "$REPO_ROOT/scripts/vibedev-cloudflare-pages-deploy.sh" \
  --output-dir "$OUTPUT_DIR" \
  --project-name "$PROJECT_NAME" \
  --branch "$BRANCH")
url=$(printf '%s\n' "$deploy_out" | sed -n 's/^VIBEDEV_PUBLIC_URL=//p' | tail -1)

if [ "$PREVIEW" = true ]; then
  log "✅ Preview deployed: ${url:-https://${BRANCH}.${PROJECT_NAME}.pages.dev}"
  log "   (also at https://${BRANCH}.${PROJECT_NAME}.pages.dev; ${DOMAIN} is unchanged)"
  exit 0
fi
log "✅ Deployed: ${url:-https://${PROJECT_NAME}.pages.dev}"

log "🔗 Attaching custom domain ${DOMAIN}..."
api="https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects/${PROJECT_NAME}/domains"
response=$(curl -s -w "\n%{http_code}" -X POST "$api" \
  -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"name\": \"${DOMAIN}\"}")
status=$(printf '%s\n' "$response" | tail -n 1)
body=$(printf '%s\n' "$response" | sed '$d')
if [ "$status" = 200 ] || [ "$status" = 201 ]; then
  log "✅ ${DOMAIN} attached."
elif printf '%s' "$body" | grep -qE '8000007|8000018'; then
  log "✅ ${DOMAIN} was already attached."
else
  log "⚠️  Could not attach ${DOMAIN} (HTTP ${status}): ${body}"
  log "   Dashboard: Workers & Pages → ${PROJECT_NAME} → Custom domains → Set up a custom domain → ${DOMAIN}"
fi

# Pages validates the domain (and its DNS record) in the background; report where it stands
domain_status=$(curl -s "$api/${DOMAIN}" -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
  | grep -o '"status":"[a-z_]*"' | head -1 | cut -d'"' -f4 || true)
case "$domain_status" in
  active) log "🌐 Live at https://${DOMAIN}" ;;
  "") log "   Check https://${DOMAIN} in a minute; Pages is validating the domain." ;;
  *) log "   Domain status: ${domain_status}. It turns active once the CNAME ${DOMAIN} → ${PROJECT_NAME}.pages.dev resolves (Pages adds it for zones in this account)." ;;
esac
