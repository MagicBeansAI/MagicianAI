#!/usr/bin/env bash
# Install the local-generation model already selected in magician-config.yaml.
# The settings API validates and pins the choice before this installer runs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="${MAGICIAN_ROOT_DIR:-${MAGICIAN_STORAGE_PATH:-$HOME/MagicianNotes}}"
CONFIG_PATH="${MAGICIAN_CONFIG_PATH:-}"

if [[ -z "$CONFIG_PATH" ]]; then
  for candidate in \
    "$DATA_DIR/magician-config.yaml" \
    "$ROOT_DIR/magician-config.yaml"; do
    if [[ -f "$candidate" ]]; then
      CONFIG_PATH="$candidate"
      break
    fi
  done
fi
if [[ -z "$CONFIG_PATH" || ! -f "$CONFIG_PATH" ]]; then
  printf '  ERROR magician-config.yaml was not found\n' >&2
  exit 1
fi

SELECTED="$(ruby -e '
  text = File.read(ARGV.fetch(0))
  matches = text.scan(/^\s*selected: &local_generation_model\s+(\S+)\s*$/).flatten
  abort "expected exactly one selected local-generation anchor, found #{matches.length}" unless matches.length == 1
  print matches.fetch(0)
' "$CONFIG_PATH")"

exec bash "$SCRIPT_DIR/setup-local-generation-model.sh" "$SELECTED" --configured
