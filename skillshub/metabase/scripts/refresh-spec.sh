#!/usr/bin/env bash
# Fetch the live Metabase OpenAPI spec and re-curate it for the data-analyst
# CLI. Run when Metabase upgrades or when the curated path/operation list
# changes.
#
#   make regen-metabase-cli   # invokes this then rebuilds the binary
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
CACHE_DIR="$DIR/.cache"
mkdir -p "$CACHE_DIR"

: "${METABASE_BASE_URL:?Set METABASE_BASE_URL to the Metabase instance URL (https://...)}"

echo "Fetching $METABASE_BASE_URL/api/docs/openapi.json -> $CACHE_DIR/metabase-full.json"
curl -fsSL "$METABASE_BASE_URL/api/docs/openapi.json" -o "$CACHE_DIR/metabase-full.json"

python3 "$DIR/postprocess-spec.py" \
  --input "$CACHE_DIR/metabase-full.json" \
  --output "$DIR/spec.json" \
  --server "$METABASE_BASE_URL"

echo "Curated spec ready at $DIR/spec.json"
