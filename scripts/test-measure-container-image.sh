#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
mkdir -p "$TMP_ROOT/bin"

cat > "$TMP_ROOT/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2 $3" == "buildx imagetools inspect" && "$4" == "--raw" ]]; then
  case "$5" in
    example.test/magician:1)
      printf '%s\n' '{"manifests":[{"digest":"sha256:amd","platform":{"os":"linux","architecture":"amd64"}},{"digest":"sha256:arm","platform":{"os":"linux","architecture":"arm64"}}]}' ;;
    example.test/magician:1@sha256:amd)
      printf '%s\n' '{"config":{"digest":"sha256:config-amd","size":10},"layers":[{"digest":"sha256:a","mediaType":"layer","size":100},{"digest":"sha256:b","mediaType":"layer","size":200}]}' ;;
    example.test/magician:1@sha256:arm)
      printf '%s\n' '{"config":{"digest":"sha256:config-arm","size":20},"layers":[{"digest":"sha256:c","mediaType":"layer","size":300},{"digest":"sha256:d","mediaType":"layer","size":400}]}' ;;
    *) exit 1 ;;
  esac
elif [[ "$1" == "pull" ]]; then
  exit 0
elif [[ "$1 $2" == "image inspect" && "$3" == "--format" ]]; then
  case "$5" in
    *@sha256:amd) echo 1000 ;;
    *@sha256:arm) echo 2000 ;;
    *) exit 1 ;;
  esac
else
  echo "unexpected docker args: $*" >&2
  exit 1
fi
EOF
chmod +x "$TMP_ROOT/bin/docker"

PATH="$TMP_ROOT/bin:$PATH" bash "$REPO/scripts/measure-container-image.sh" \
  --image example.test/magician:1 \
  --output "$TMP_ROOT/report.json" \
  --max-compressed-bytes 1000 \
  --max-expanded-bytes 2500

jq -e '.within_budget == true' "$TMP_ROOT/report.json" >/dev/null
jq -e '.platforms[] | select(.platform == "linux/amd64") | .compressed_bytes == 310 and .expanded_bytes == 1000' "$TMP_ROOT/report.json" >/dev/null
jq -e '.platforms[] | select(.platform == "linux/arm64") | .compressed_bytes == 720 and .expanded_bytes == 2000' "$TMP_ROOT/report.json" >/dev/null

if PATH="$TMP_ROOT/bin:$PATH" bash "$REPO/scripts/measure-container-image.sh" \
  --image example.test/magician:1 \
  --output "$TMP_ROOT/over-budget.json" \
  --max-compressed-bytes 500 \
  --skip-pull; then
  echo "ERROR: expected compressed budget failure" >&2
  exit 1
fi
jq -e '.within_budget == false' "$TMP_ROOT/over-budget.json" >/dev/null

echo "container image measurement tests passed"
