#!/usr/bin/env python3
"""Install a laptop asset and measure its screen, so the swap is one command.

The device is a photograph, and photographs get re-rolled — the first one was
shot from slightly above with the lid past 100 degrees and read as LEANING
BACK, which looks like a tilt even though the composite carries no rotation at
all. Each replacement has a different screen rect, and hand-measuring it and
hand-editing four constants into a Svelte file is how a stale number ends up
stretching the machine in every scene.

So the geometry travels WITH the asset: this measures the dead screen (the
largest contiguous near-black run, which is the only thing on a screen-off
plate that can be) and writes `laptop.json` beside the PNG. `LifeDevice` reads
that, and `life_scenes.json`'s quad takes its aspect from it.

    python3 scripts/life_laptop.py ~/some-laptop-cutout.png

The input must already have a transparent background.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# The IMAGE is an asset and lives in the public dir; the GEOMETRY is source and
# lives beside the component that reads it. `static/` is SvelteKit's public
# directory, not a module root — importing from it is a 403 at dev time, which
# is exactly how this landed the first time.
DEST = ROOT / "static" / "landing"
GEOM_DEST = ROOT / "src" / "lib" / "landing" / "laptopGeom.json"


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    src = Path(sys.argv[1])
    if not src.exists():
        sys.exit(f"no such file: {src}")

    meta = json.loads(subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=width,height",
         "-of", "json", str(src)], capture_output=True, text=True).stdout)["streams"][0]
    w, h = meta["width"], meta["height"]
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", str(src), "-f", "rawvideo",
                          "-pix_fmt", "rgba", "-"], capture_output=True).stdout
    if len(raw) < w * h * 4:
        sys.exit("short read decoding the asset")

    def dark(x: int, y: int) -> bool:
        i = (y * w + x) * 4
        return raw[i + 3] > 200 and max(raw[i], raw[i + 1], raw[i + 2]) < 52

    if raw[3] > 128:
        print("  ! the top-left pixel is opaque — is the background removed?")

    cx = w // 2
    runs, start = [], None
    for y in range(h):
        if dark(cx, y):
            if start is None:
                start = y
        elif start is not None:
            runs.append((start, y - 1))
            start = None
    if start is not None:
        runs.append((start, h - 1))
    if not runs:
        sys.exit("found no dark screen — is the screen actually off?")
    runs.sort(key=lambda r: r[1] - r[0], reverse=True)
    y0, y1 = runs[0]
    xs = [x for x in range(w) if dark(x, (y0 + y1) // 2)]
    x0, x1 = xs[0], xs[-1]

    # How level is it? A generated plate is never exactly square, and a degree
    # of lean in the asset is a degree nothing downstream can correct.
    def top_at(x: int) -> int:
        for y in range(h):
            if dark(x, y):
                return y
        return 0
    lean = top_at(x1 - 80) - top_at(x0 + 80)

    # WHERE THE MACHINE ACTUALLY IS, not where the image ends. A cut-out
    # carries transparent padding around the body, and placing the device from
    # the image edges puts it tens of pixels off — which is how "move it right
    # until only a sliver of plate shows" became guesswork. The silhouette is
    # the thing the eye sees, so it is the thing the placement solves against.
    cols = [x for x in range(0, w, 2)
            if any(raw[((y * w + x) * 4) + 3] > 128 for y in range(0, h, 4))]
    rows = [y for y in range(0, h, 2)
            if any(raw[((y * w + x) * 4) + 3] > 128 for x in range(0, w, 4))]
    geom = {
        "w": w, "h": h,
        "sx": x0, "sy": y0, "sw": x1 - x0, "sh": y1 - y0,
        # The visible machine's bounding box in the asset's own pixels.
        "bx": cols[0], "by": rows[0],
        "bw": cols[-1] - cols[0], "bh": rows[-1] - rows[0],
    }
    DEST.mkdir(parents=True, exist_ok=True)
    subprocess.run(["cp", str(src), str(DEST / "laptop.png")], check=True)
    GEOM_DEST.write_text(json.dumps(geom, indent=2) + "\n")

    print(f"  asset ..... {w}×{h}")
    print(f"  screen .... {geom['sw']}×{geom['sh']} at ({x0}, {y0})  "
          f"aspect {geom['sw'] / geom['sh']:.3f}")
    print(f"  level ..... {lean:+d}px across the screen")
    print(f"  deck ...... {h - y1}px below the screen")
    print(f"  body ...... {geom['bw']}×{geom['bh']} at ({geom['bx']}, {geom['by']}) "
          f"— margins L{geom['bx'] - x0 if False else x0 - geom['bx']} "
          f"R{(geom['bx'] + geom['bw']) - (x0 + geom['sw'])} "
          f"B{(geom['by'] + geom['bh']) - (y0 + geom['sh'])} around the screen")
    print("\n  wrote static/landing/laptop.png + src/lib/landing/laptopGeom.json"
          " — now re-run life_apply.py so the quad picks up the new aspect.")


if __name__ == "__main__":
    main()
