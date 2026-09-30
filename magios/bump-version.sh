#!/usr/bin/env bash
# Bump the Magios (iOS) app version — the SAFE way.
#
# `magios/project.yml` is the version SOURCE OF TRUTH: xcodegen regenerates
# `Magios.xcodeproj/project.pbxproj` from it, so bumping the pbxproj directly is
# silently clobbered on the next regen (this happened for builds 60-63). This
# script updates project.yml AND regenerates the pbxproj so the two never drift.
#
# Usage:
#   magios/bump-version.sh                 # auto: patch +1 and build +1
#   magios/bump-version.sh 0.1.70          # explicit marketing version; build +1
#   magios/bump-version.sh 0.2.0 70        # explicit marketing version + build
#   DRY=1 magios/bump-version.sh 0.1.70    # show what would change, don't write
set -euo pipefail
cd "$(dirname "$0")"   # -> magios/
YML=project.yml

cur_mv=$(grep -m1 'MARKETING_VERSION:' "$YML" | sed -E 's/.*"([^"]+)".*/\1/')
cur_bn=$(grep -m1 'CURRENT_PROJECT_VERSION:' "$YML" | sed -E 's/.*"([^"]+)".*/\1/')

new_mv="${1:-}"
new_bn="${2:-}"

if [[ -z "$new_mv" ]]; then
  IFS='.' read -r a b c <<< "$cur_mv"
  new_mv="${a}.${b}.$((c + 1))"     # auto-bump the patch
fi
if [[ -z "$new_bn" ]]; then
  new_bn=$((cur_bn + 1))
fi

echo "MARKETING_VERSION:      $cur_mv -> $new_mv"
echo "CURRENT_PROJECT_VERSION: $cur_bn -> $new_bn"

if [[ "${DRY:-0}" == "1" ]]; then
  echo "(DRY run — no files changed)"
  exit 0
fi

perl -i -pe \
  "s/MARKETING_VERSION: \"[^\"]*\"/MARKETING_VERSION: \"$new_mv\"/;
   s/CURRENT_PROJECT_VERSION: \"[^\"]*\"/CURRENT_PROJECT_VERSION: \"$new_bn\"/" "$YML"
echo "updated $YML"

if command -v xcodegen >/dev/null 2>&1; then
  xcodegen generate >/dev/null
  echo "regenerated Magios.xcodeproj (version applied to the pbxproj)"
else
  echo "WARNING: xcodegen not on PATH — run 'xcodegen generate' in magios/ to apply to the pbxproj" >&2
  exit 1
fi

echo "Done. Remember to add a magios/CHANGELOG.md entry for $new_mv."
