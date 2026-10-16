#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
canary_root="$(mktemp -d "${TMPDIR:-/tmp}/magician-app-real-server-consumer.XXXXXX")"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$canary_root"
}
trap cleanup EXIT

mkdir -p \
  "$canary_root/repo/sdk" \
  "$canary_root/repo/examples/reference-apps" \
  "$canary_root/package"

cargo build -p magician-api --example p4_supported_public_fixture
target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
server_binary="$target_root/debug/examples/p4_supported_public_fixture"
test -x "$server_binary"

ready_file="$canary_root/server-origin"
server_log="$canary_root/server.log"
"$server_binary" "$ready_file" >"$server_log" 2>&1 &
server_pid="$!"

for ((attempt = 0; attempt < 600; attempt += 1)); do
  if [[ -s "$ready_file" ]]; then
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    sed -n '1,240p' "$server_log" >&2
    echo "Real Apps fixture server exited before becoming ready." >&2
    exit 1
  fi
  sleep 0.1
done
if [[ ! -s "$ready_file" ]]; then
  sed -n '1,240p' "$server_log" >&2
  echo "Real Apps fixture server did not become ready within 60 seconds." >&2
  exit 1
fi
server_origin="$(tr -d '\r\n' <"$ready_file")"

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
MAGICIAN_P4_REAL_SERVER_ORIGIN="$server_origin" \
  node --test "$consumer_root/dist/tests/supported-public-real-server-canary.test.js"

echo "Packed TypeScript consumer passed against configure_app_routes and real Apps registry/entity owners."
echo "Action launch/run polling remains gated on a provider-free ArtifactV2/AppWorkflow fixture owner."
