#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
canary_root="$(mktemp -d "${TMPDIR:-/tmp}/magician-app-ts-consumer.XXXXXX")"
trap 'rm -rf "$canary_root"' EXIT

mkdir -p \
  "$canary_root/repo/sdk" \
  "$canary_root/repo/examples/reference-apps" \
  "$canary_root/package"
rsync -a --exclude node_modules --exclude dist \
  "$repo_root/sdk/typescript" "$canary_root/repo/sdk/"
rsync -a --exclude node_modules --exclude dist \
  "$repo_root/examples/reference-apps/research-planner" \
  "$canary_root/repo/examples/reference-apps/"

sdk_root="$canary_root/repo/sdk/typescript"
consumer_root="$canary_root/repo/examples/reference-apps/research-planner"
npm --prefix "$sdk_root" ci --ignore-scripts --no-audit --no-fund
npm --prefix "$sdk_root" run build

pack_json="$(npm pack "$sdk_root" --ignore-scripts --json --pack-destination "$canary_root/package")"
packed_name="$(node -e 'const rows=JSON.parse(process.argv[1]); if(rows.length!==1) process.exit(2); process.stdout.write(rows[0].filename)' "$pack_json")"
packed_sdk="$canary_root/package/$packed_name"
test -f "$packed_sdk"

node -e '
  const fs = require("node:fs");
  const path = process.argv[1];
  const packed = process.argv[2];
  const manifest = JSON.parse(fs.readFileSync(path, "utf8"));
  manifest.dependencies["@magician/apps"] = `file:${packed}`;
  fs.writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
' "$consumer_root/package.json" "$packed_sdk"

npm --prefix "$consumer_root" install --ignore-scripts --no-audit --no-fund
npm --prefix "$consumer_root" run build --ignore-scripts
node --test "$consumer_root/dist/tests/supported-public-consumer-canary.test.js"

echo "TypeScript supported-public consumer canary passed from a fresh packed-SDK tree."
