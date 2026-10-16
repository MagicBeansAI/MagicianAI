#!/usr/bin/env bash
# Assemble the whole montage from a directory of plates.
#
# Ingest is per-scene and APPENDS, so a rebuild has to start from nothing or the
# frame numbering marches on from whatever was there before. Deleting first is
# not destructive: every frame here is derived from a master in src/, and the
# authored half — quads and captions — lives in life_scenes.json, not in the
# manifest that gets thrown away.
set -euo pipefail
cd "$(dirname "$0")/.."
REEL=static/prologue/reel-life
SRC=${1:-$REEL/src}

# The order IS the film: a day, morning to evening, because the caption's first
# beat is a time of day.
ORDER=(lake park court kitchen climb hill)

rm -f "$REEL"/f*.webp "$REEL"/manifest.json
for id in "${ORDER[@]}"; do
  f=$(ls "$SRC"/scene*-"$id".mp4 2>/dev/null | head -1 || true)
  [ -z "$f" ] && { echo "  skip $id — no master in $SRC"; continue; }
  echo "── $id"
  python3 scripts/life_ingest.py "$f" --id "$id" --commit
done
python3 scripts/life_apply.py
echo
du -sh "$REEL"/*.webp | tail -1 2>/dev/null || true
ls "$REEL"/*.webp | wc -l | xargs echo "  frames:"
