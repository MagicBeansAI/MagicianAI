#!/usr/bin/env bash
# Build and publish the Magician marketing site to Cloudflare Pages.
# It uses the same credentials as vibedev (from ~/MagicianNotes/.env).

set -euo pipefail

log() { printf '%s\n' "$*" >&2; }
fail() { log "ERROR: $*"; exit 1; }

PROJECT_NAME="magician-marketing"
DOMAIN="next.magican.ai"
OUTPUT_DIR="marketing-site"

# Load local environment (same as vibedev)
for env_file in ~/MagicianNotes/.env.development ~/MagicianNotes/.env; do
  if [ -f "$env_file" ]; then
    set -a
    source "$env_file"
    set +a
  fi
done

if [ -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
  fail "CLOUDFLARE_ACCOUNT_ID is missing from ~/MagicianNotes/.env"
fi

if [ -n "${CLOUDFLARE_PAGES_API_TOKEN:-}" ]; then
  export CLOUDFLARE_API_TOKEN="$CLOUDFLARE_PAGES_API_TOKEN"
elif [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  fail "CLOUDFLARE_PAGES_API_TOKEN or CLOUDFLARE_API_TOKEN is missing from ~/MagicianNotes/.env"
fi

if [ -n "${WRANGLER_BIN:-}" ]; then
  WRANGLER=("$WRANGLER_BIN")
elif command -v wrangler >/dev/null 2>&1; then
  WRANGLER=(wrangler)
elif command -v npx >/dev/null 2>&1; then
  WRANGLER=(npx --yes wrangler)
else
  fail "wrangler is not installed and npx is unavailable."
fi

log "🎨 Building marketing site..."
make build-marketing-site

log "🚀 Publishing to Cloudflare Pages project: ${PROJECT_NAME}..."
bash scripts/vibedev-cloudflare-pages-deploy.sh \
  --output-dir "$OUTPUT_DIR" \
  --project-name "$PROJECT_NAME" \
  --branch main

log "✅ Done! Marketing site is live at https://${PROJECT_NAME}.pages.dev"

log "🔗 Attaching custom domain: ${DOMAIN} using Cloudflare API..."
RESPONSE=$(curl -s -w "\n%{http_code}" -X POST \
  "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/pages/projects/${PROJECT_NAME}/domains" \
  -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"name\": \"${DOMAIN}\"}")

HTTP_STATUS=$(echo "$RESPONSE" | tail -n 1)
BODY=$(echo "$RESPONSE" | sed '$d')

if [ "$HTTP_STATUS" -eq 201 ] || [ "$HTTP_STATUS" -eq 200 ]; then
  log "✅ Custom domain ${DOMAIN} successfully attached!"
else
  # 8000007 or 8000018 means the domain is already attached
  if echo "$BODY" | grep -qE '8000007|8000018'; then
    log "✅ Custom domain ${DOMAIN} is already attached to this project."
  else
    log "⚠️ Failed to attach custom domain (HTTP ${HTTP_STATUS})."
    log "Response: $BODY"
    log "To attach the custom domain, visit the Cloudflare Dashboard: Pages -> magician-marketing -> Custom Domains -> Add Domain (${DOMAIN})"
  fi
fi
