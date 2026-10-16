#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_svg="$repo_root/scripts/assets/magican-talk-control-source.svg"
symbol_dir="$repo_root/magios/MagiosWidgets/Assets.xcassets/MagicanTalkControl.symbolset"
output_svg="$symbol_dir/magican-talk-control.svg"

if ! command -v swiftdraw >/dev/null 2>&1; then
  echo "swiftdraw is required; install it with: brew install swiftdraw" >&2
  exit 1
fi

mkdir -p "$symbol_dir"
swiftdraw "$source_svg" --format sfsymbol --insets auto --output "$output_svg"
echo "Generated $output_svg"
