#!/usr/bin/env bash
# Measure per-platform OCI compressed and expanded image size.

set -euo pipefail

IMAGE=""
OUTPUT=""
SKIP_PULL=false
MAX_COMPRESSED_BYTES=""
MAX_EXPANDED_BYTES=""
PLATFORMS=()

usage() {
  cat <<'EOF'
Usage: scripts/measure-container-image.sh --image IMAGE --output REPORT [options]

Options:
  --platform OS/ARCH              Platform to measure; repeatable (defaults to amd64 + arm64).
  --skip-pull                     Record registry-compressed sizes only.
  --max-compressed-bytes BYTES    Fail when any platform exceeds this budget.
  --max-expanded-bytes BYTES      Fail when any pulled platform exceeds this budget.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image) IMAGE="${2:?missing image}"; shift 2 ;;
    --output) OUTPUT="${2:?missing output}"; shift 2 ;;
    --platform) PLATFORMS+=("${2:?missing platform}"); shift 2 ;;
    --skip-pull) SKIP_PULL=true; shift ;;
    --max-compressed-bytes) MAX_COMPRESSED_BYTES="${2:?missing budget}"; shift 2 ;;
    --max-expanded-bytes) MAX_EXPANDED_BYTES="${2:?missing budget}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "ERROR: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "$IMAGE" && -n "$OUTPUT" ]] || { usage >&2; exit 2; }
[[ ${#PLATFORMS[@]} -gt 0 ]] || PLATFORMS=(linux/amd64 linux/arm64)
command -v docker >/dev/null 2>&1 || { echo "ERROR: docker is required" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq is required" >&2; exit 2; }
for budget in "$MAX_COMPRESSED_BYTES" "$MAX_EXPANDED_BYTES"; do
  [[ -z "$budget" || "$budget" =~ ^[0-9]+$ ]] || { echo "ERROR: budgets must be integer bytes" >&2; exit 2; }
done
if [[ "$SKIP_PULL" == "true" && -n "$MAX_EXPANDED_BYTES" ]]; then
  echo "ERROR: --max-expanded-bytes cannot be used with --skip-pull" >&2
  exit 2
fi

started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
index_json="$(docker buildx imagetools inspect --raw "$IMAGE")"
printf '%s' "$index_json" | jq -e 'type == "object"' >/dev/null \
  || { echo "ERROR: registry returned an invalid OCI manifest" >&2; exit 1; }

rows='[]'
budget_failed=false
for platform in "${PLATFORMS[@]}"; do
  os="${platform%%/*}"
  arch="${platform##*/}"
  digest="$(printf '%s' "$index_json" | jq -r --arg os "$os" --arg arch "$arch" '
    first(.manifests[]? | select(.platform.os == $os and .platform.architecture == $arch) | .digest) // empty
  ')"
  if [[ -n "$digest" ]]; then
    manifest_ref="${IMAGE%@*}@${digest}"
    manifest_json="$(docker buildx imagetools inspect --raw "$manifest_ref")"
  elif printf '%s' "$index_json" | jq -e '.layers and .config' >/dev/null 2>&1 && [[ ${#PLATFORMS[@]} -eq 1 ]]; then
    manifest_ref="$IMAGE"
    manifest_json="$index_json"
    digest="$(printf '%s' "$manifest_json" | jq -r '.config.digest // "single-manifest"')"
  else
    echo "ERROR: image '$IMAGE' has no manifest for $platform" >&2
    exit 1
  fi

  compressed_bytes="$(printf '%s' "$manifest_json" | jq '[.config.size // 0, .layers[]?.size // 0] | add')"
  layers="$(printf '%s' "$manifest_json" | jq '[.layers[]? | {digest,media_type:.mediaType,size_bytes:.size}] | sort_by(.size_bytes) | reverse')"
  expanded_bytes=null
  if [[ "$SKIP_PULL" == "false" ]]; then
    docker pull --platform "$platform" "$manifest_ref" >/dev/null
    expanded_bytes="$(docker image inspect --format '{{.Size}}' "$manifest_ref")"
    [[ "$expanded_bytes" =~ ^[0-9]+$ ]] \
      || { echo "ERROR: Docker returned a non-numeric expanded size for $platform" >&2; exit 1; }
  fi

  compressed_ok=true
  expanded_ok=true
  if [[ -n "$MAX_COMPRESSED_BYTES" && "$compressed_bytes" -gt "$MAX_COMPRESSED_BYTES" ]]; then
    compressed_ok=false
    budget_failed=true
  fi
  if [[ -n "$MAX_EXPANDED_BYTES" && "$expanded_bytes" != null && "$expanded_bytes" -gt "$MAX_EXPANDED_BYTES" ]]; then
    expanded_ok=false
    budget_failed=true
  fi

  row="$(jq -n \
    --arg platform "$platform" \
    --arg digest "$digest" \
    --argjson compressed_bytes "$compressed_bytes" \
    --argjson expanded_bytes "$expanded_bytes" \
    --argjson compressed_within_budget "$compressed_ok" \
    --argjson expanded_within_budget "$expanded_ok" \
    --argjson layers "$layers" \
    '{platform:$platform,digest:$digest,compressed_bytes:$compressed_bytes,expanded_bytes:$expanded_bytes,compressed_within_budget:$compressed_within_budget,expanded_within_budget:$expanded_within_budget,layers:$layers}')"
  rows="$(jq -cn --argjson rows "$rows" --argjson row "$row" '$rows + [$row]')"
  printf '%-14s compressed=%12s expanded=%12s\n' "$platform" "$compressed_bytes" "$expanded_bytes"
done

mkdir -p "$(dirname "$OUTPUT")"
jq -n \
  --arg schema_version "1" \
  --arg image "$IMAGE" \
  --arg started_at "$started_at" \
  --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --argjson max_compressed_bytes "${MAX_COMPRESSED_BYTES:-null}" \
  --argjson max_expanded_bytes "${MAX_EXPANDED_BYTES:-null}" \
  --argjson platforms "$rows" \
  --argjson within_budget "$([[ "$budget_failed" == "false" ]] && echo true || echo false)" \
  '{schema_version:($schema_version|tonumber),image:$image,started_at:$started_at,completed_at:$completed_at,budgets:{max_compressed_bytes:$max_compressed_bytes,max_expanded_bytes:$max_expanded_bytes},within_budget:$within_budget,platforms:$platforms}' \
  > "$OUTPUT"
printf 'Image measurement report: %s\n' "$OUTPUT"

[[ "$budget_failed" == "false" ]]
