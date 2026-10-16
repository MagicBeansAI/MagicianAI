#!/usr/bin/env bash
# Regenerate the bundled iOS tutor-primitive recipe set from the seed source, so
# the app ships an offline fallback that can never drift from the built-ins.
#
#   scripts/magios-bundle-tutor-primitives.sh
#
# Reads magician_data_v3/system/tutor_primitives/*.json and writes a single JSON
# array to magios/Magios/Resources/tutor_primitives.json (bundled into the app).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/magician_data_v3/system/tutor_primitives"
OUT_DIR="$ROOT/magios/Magios/Resources"
OUT="$OUT_DIR/tutor_primitives.json"

mkdir -p "$OUT_DIR"

python3 - "$SRC" "$OUT" <<'PY'
import json, glob, os, sys
src, out = sys.argv[1], sys.argv[2]
recipes = []
for f in sorted(glob.glob(os.path.join(src, "*.json"))):
    with open(f) as fh:
        recipes.append(json.load(fh))
with open(out, "w") as fh:
    json.dump(recipes, fh, indent=2)
    fh.write("\n")
print(f"bundled {len(recipes)} tutor primitives -> {out}")
PY
