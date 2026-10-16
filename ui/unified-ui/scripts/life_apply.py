#!/usr/bin/env python3
"""Fold the authored half of each scene into the manifest.

`life_ingest.py` writes what it can MEASURE — frame ranges, the room's light,
how much the camera moved. It deliberately leaves `screenQuad` null, because
under the inversion there is no device in the plate to detect: the quad is where
we DECIDED to stand one, and the caption is the argument the scene is making.
Both are authored, both live in `life_scenes.json`, and both survive a re-ingest
of the same plate — which is the whole reason they are not typed into the
manifest by hand.
"""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAN = ROOT / "static" / "prologue" / "reel-life" / "manifest.json"
HERE = Path(__file__).resolve().parent
AUTH = json.loads((HERE / "life_scenes.json").read_text())
GEOM = json.loads((ROOT / "src" / "lib" / "landing" / "laptopGeom.json").read_text())


def fit_to_asset(quad: list, geom: dict) -> list:
    """Force a quad to the ASSET's screen aspect, keeping its top-left and width.

    The homography maps the asset's screen rect onto this quad, so a quad of any
    other ratio stretches the entire machine — and nothing downstream can tell,
    because a stretched laptop is still a laptop-shaped thing. It happened
    silently once already (quads at 1.438 against an asset at 1.559) and would
    happen again on every re-roll, since each new photograph has its own ratio.
    Deriving the height here means the authored file only ever has to say where
    the device stands and how wide it is.
    """
    (x0, y0), (x1, _), _, _ = quad
    w = x1 - x0
    h = round(w / (geom["sw"] / geom["sh"]))
    return [[x0, y0], [x0 + w, y0], [x0 + w, y0 + h], [x0, y0 + h]]

man = json.loads(MAN.read_text())
applied, missing = [], []
for sc in man.get("scenes", []):
    a = AUTH.get(sc["id"])
    if not a:
        missing.append(sc["id"])
        continue
    sc.update({k: v for k, v in a.items() if not k.startswith("_")})
    sc.setdefault("device", "laptop")
    if sc.get("screenQuad"):
        sc["screenQuad"] = fit_to_asset(sc["screenQuad"], GEOM)
    applied.append(sc["id"])
man["version"] = int(man.get("version", 0)) + 1
MAN.write_text(json.dumps(man, indent=2) + "\n")

print(f"  authored: {', '.join(applied) or 'none'}")
if missing:
    print(f"  NO ENTRY in life_scenes.json: {', '.join(missing)}")
print(f"  quad fitted to the asset's screen aspect "
      f"{GEOM['sw'] / GEOM['sh']:.3f}")
print(f"  manifest version {man['version']}, {man['frames']} frames, "
      f"{len(man.get('scenes', []))} scene(s)")
