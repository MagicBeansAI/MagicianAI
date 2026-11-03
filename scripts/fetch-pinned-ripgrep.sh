#!/usr/bin/env bash
# Fetch the ripgrep binary skillshub/package-lock.json pins for one
# @vscode/ripgrep platform, verified against the lockfile's sha512 integrity.
#
#   scripts/fetch-pinned-ripgrep.sh <npm-platform> <out-file>
#   e.g. scripts/fetch-pinned-ripgrep.sh linux-x64 /tmp/rg
#
# package-release.sh uses it when skillshub/node_modules lacks the target's
# copy: a clean CI checkout (node_modules is gitignored) or a cross-target
# package (npm installs only the host's optional platform package).
set -euo pipefail

PLATFORM="${1:?npm platform, e.g. darwin-arm64}"
OUT="${2:?output file}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCK="$ROOT_DIR/skillshub/package-lock.json"

read -r URL INTEGRITY < <(python3 -I - "$LOCK" "$PLATFORM" <<'EOF'
import json, sys
lock, platform = sys.argv[1], sys.argv[2]
entry = json.load(open(lock)).get("packages", {}).get(f"node_modules/@vscode/ripgrep-{platform}")
if not entry or not entry.get("resolved") or not entry.get("integrity", "").startswith("sha512-"):
    sys.exit(f"skillshub/package-lock.json pins no @vscode/ripgrep-{platform} with a sha512 integrity")
print(entry["resolved"], entry["integrity"])
EOF
)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
curl -fsSL --retry 3 -o "$WORK/pkg.tgz" "$URL"
ACTUAL="sha512-$(openssl dgst -sha512 -binary "$WORK/pkg.tgz" | openssl base64 -A)"
if [[ "$ACTUAL" != "$INTEGRITY" ]]; then
  echo "integrity mismatch for $URL" >&2
  exit 1
fi
tar -xzf "$WORK/pkg.tgz" -C "$WORK"
BIN="$WORK/package/bin/rg"
[[ -f "$BIN" ]] || BIN="$WORK/package/bin/rg.exe"
[[ -f "$BIN" ]] || { echo "no bin/rg in $URL" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")"
cp "$BIN" "$OUT"
chmod 755 "$OUT"
