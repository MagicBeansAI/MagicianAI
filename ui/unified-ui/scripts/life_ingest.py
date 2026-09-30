#!/usr/bin/env python3
"""Ingest a life-montage plate: judge it, cut it, and describe it.

WHY THIS EXISTS. The montage's first paid scene was accepted by eye and only
later found to break four of the six things the compositing contract asked of
it — one of which (the laptop lid facing away from camera) nobody had thought
to write down until the footage arrived. Eye-checking generated video does not
work: the failures are geometric, they accumulate across a clip, and the frame
you happen to open is rarely the frame that breaks.

So a plate is not usable until this says so. It runs BEFORE any compositing
work, because that is the point at which footage we paid for is either kept or
regenerated, and every hour spent overlaying an unusable plate is wasted.

WHAT IT CHECKS, and what it deliberately does not.

Under the inversion (video supplies the room and the people; the DOM supplies
the whole device) the hardware checks are gone — there is no device in the
plate to be branded, lit, facing the wrong way or drifting out of frame. What
survives is the one requirement the DOM cannot paper over:

  · THE CAMERA MUST BE LOCKED OFF. A DOM device is placed once, in frame
    pixels, by an authored quad. If the plate moves under it, the device
    swims — and matching a moving plate needs per-frame tracking data we
    cannot reliably extract from generated video. This is measured, not
    trusted, because "locked-off" is a thing a generator will cheerfully
    claim and then ignore.

  · THE FOREGROUND MUST BE CLEAR. The device has to sit somewhere. A plate
    whose lower third is full of dough, arms and jars has nowhere to put it.
    Reported as a busy-ness score over the placement band rather than a
    pass/fail, because how much clutter is too much is a judgement about the
    shot, not a threshold.

It also samples the room's light, which the overlay needs in order to grade our
sRGB-blue UI into a golden kitchen instead of glowing out of it.

USAGE
    python3 scripts/life_ingest.py SRC.mp4 --id kitchen
    python3 scripts/life_ingest.py SRC.mp4 --id kitchen --commit

Without --commit it measures and reports only: nothing is written, so a plate
can be judged before a single frame is encoded.
"""

from __future__ import annotations

import argparse
import json
import math
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REEL = Path(__file__).resolve().parent.parent / "static" / "prologue" / "reel-life"

# The montage plays at the film's own frame rate. 12fps is what the prologue
# reels use and it is deliberate: a scrubbed sequence is decoded frame by frame,
# so the cost of the reel is linear in frame count, and 12 reads as film rather
# than as a slideshow once motion is gentle. These plates are gentle by
# construction — a locked-off camera and people moving slowly.
TARGET_FPS = 12
TARGET_W = 1280
# 82 lands photographic plates near the 23KB/frame the existing photoreal
# reel averages, with no visible loss at the size the montage plays.
WEBP_QUALITY = 82

# Drift is CAMERA TRANSLATION in plate pixels — see `best_shift`. It is not a
# difference score: water, grass and people are all supposed to move, and
# scoring them as drift failed a plate whose frame was in fact rock steady.
# What breaks a composited device is the frame translating under it, so that is
# the number, and the leftover difference after alignment is reported
# separately as world motion — which is the thing we are paying for.
DRIFT_PASS = 6.0
DRIFT_WARN = 16.0
# Divergence between the frame's two halves, in plate px. A locked shot holds
# both halves together; anything above this is the camera moving toward or away
# from the scene, which no static overlay can follow.
ZOOM_FAIL = 8.0

# The band, as fractions of frame height, where a foreground device would sit.
PLACEMENT_BAND = (0.45, 1.0)
# ...and as fractions of frame WIDTH. The composition puts the device right of
# centre and the people on the left, so only this span has to be calm.
PLACEMENT_X = (0.44, 1.0)


def run(cmd: list[str]) -> str:
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit(f"command failed: {' '.join(cmd[:3])}…\n{p.stderr.strip()}")
    return p.stdout


def probe(src: Path) -> dict:
    out = run(
        [
            "ffprobe", "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height,r_frame_rate,nb_frames,duration",
            "-of", "json", str(src),
        ]
    )
    s = json.loads(out)["streams"][0]
    num, den = (int(x) for x in s["r_frame_rate"].split("/"))
    return {
        "width": int(s["width"]),
        "height": int(s["height"]),
        "fps": num / den,
        "frames": int(s.get("nb_frames") or 0),
        "duration": float(s.get("duration") or 0),
    }


def sample_frames(src: Path, out_dir: Path, count: int, width: int) -> list[Path]:
    """Evenly-spaced greyscale samples, for measurement rather than shipping."""
    run(
        [
            "ffmpeg", "-y", "-v", "error", "-i", str(src),
            "-vf", f"scale={width}:-1,format=gray",
            "-frames:v", str(count),
            "-vsync", "cfr", "-r", f"{count / max(probe(src)['duration'], 0.001):.6f}",
            str(out_dir / "s%03d.png"),
        ]
    )
    return sorted(out_dir.glob("s*.png"))


def read_gray(path: Path) -> tuple[list[int], int, int]:
    """Decode a PNG to a flat luma list without requiring Pillow or numpy."""
    # Bytes, not text: `run()` decodes stdout as UTF-8, which mangles a raw
    # luma plane. This one path talks to ffmpeg directly.
    p = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-f", "rawvideo",
         "-pix_fmt", "gray", "-"],
        capture_output=True,
    )
    data = p.stdout
    meta = probe(path)
    w, h = meta["width"], meta["height"]
    if len(data) < w * h:
        sys.exit(f"short read decoding {path.name}")
    return list(data[: w * h]), w, h


def best_shift(a: list[int], b: list[int], w: int, h: int, reach: int = 8) -> tuple[int, int, float]:
    """The (dx, dy) that best aligns b onto a, and the residual after aligning.

    THE METRIC HAD TO LEARN THE DIFFERENCE between a camera that moves and a
    lake that ripples. Raw frame difference cannot: water, grass and people are
    all supposed to move, and on the first usable plate they scored 3.68 —
    enough to fail a plate that was, in the region that mattered, rock steady.
    What actually breaks a composited device is the frame TRANSLATING under it,
    so that is what gets measured: search a small window of offsets, and the one
    that minimises the difference IS the camera's motion. Whatever is left after
    aligning is the world moving inside a still frame, which is exactly what we
    are paying for.
    """
    best = (0, 0, float("inf"))
    step = 2  # every other pixel: this runs (2·reach+1)² times
    for dy in range(-reach, reach + 1):
        for dx in range(-reach, reach + 1):
            tot = n = 0
            for y in range(reach, h - reach, step * 2):
                ra, rb = y * w, (y + dy) * w + dx
                for x in range(reach, w - reach, step):
                    tot += abs(a[ra + x] - b[rb + x])
                    n += 1
            m = tot / max(n, 1)
            if m < best[2]:
                best = (dx, dy, m)
    return best


def crop(buf: list[int], w: int, h: int, x0: int, x1: int) -> list[int]:
    """A vertical slice of a luma band, as its own tightly-packed buffer."""
    out: list[int] = []
    for y in range(h):
        o = y * w
        out.extend(buf[o + x0 : o + x1])
    return out


def measure(src: Path, meta: dict) -> dict:
    """Camera motion over the placement band, and how busy that band is."""
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        frames = sample_frames(src, tmp, 12, 320)
        if len(frames) < 2:
            sys.exit("could not sample enough frames to measure drift")

        first, w, h = read_gray(frames[0])
        y0 = int(h * PLACEMENT_BAND[0])
        y1 = int(h * PLACEMENT_BAND[1])
        band = slice(y0 * w, y1 * w)
        base = first[band]
        bh = y1 - y0

        shift = 0.0
        zoom = 0.0
        residual = 0.0
        scale = meta["width"] / w
        for f in frames[1:]:
            cur, cw, ch = read_gray(f)
            if cw != w or ch != h:
                continue
            cband = cur[band]
            dx, dy, res = best_shift(base, cband, w, bh)
            shift = max(shift, math.hypot(dx, dy) * scale)
            residual = max(residual, res)
            # A PUSH-IN IS NOT A TRANSLATION, so the search above is blind to
            # it: under a zoom the frame's centre stays put and only its edges
            # move, which scores a shift of zero. That is exactly how the first
            # image-free plate passed while visibly dollying in.
            #
            # Under a true zoom the two halves of the frame move APART — the
            # left half drifts left, the right half drifts right. Measuring each
            # half separately and taking the difference turns that divergence
            # into a number, and it costs one extra pass rather than a whole
            # scale search.
            half = w // 2
            lb = crop(base, w, bh, 0, half)
            rb = crop(base, w, bh, half, w)
            lc = crop(cband, w, bh, 0, half)
            rc = crop(cband, w, bh, half, w)
            ldx = best_shift(lb, lc, half, bh, 6)[0]
            rdx = best_shift(rb, rc, w - half, bh, 6)[0]
            zoom = max(zoom, abs(rdx - ldx) * scale)

        # BUSY-NESS ONLY WHERE THE DEVICE GOES. Measuring the full width scored
        # the climbing-wall plate at 14.93 and called its foreground cluttered —
        # but every one of those coloured holds is on the LEFT, which is where
        # the PERSON goes and is supposed to be busy. The device stands in the
        # right half. Scoring the left half against it flags exactly the plates
        # the brief asks for.
        x0 = int(w * PLACEMENT_X[0])
        x1 = int(w * PLACEMENT_X[1])
        edges = 0.0
        for r in range(bh):
            o = r * w
            edges += sum(abs(base[o + c + 1] - base[o + c]) for c in range(x0, x1 - 1))
        busy = edges / max(bh * (x1 - x0 - 1), 1)

    return {
        "drift": round(shift, 2),
        "zoom": round(zoom, 2),
        "residual": round(residual, 2),
        "busy": round(busy, 2),
    }


def light_sample(src: Path) -> list[int]:
    """The room's average colour — what the overlay grades itself towards."""
    p = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(src), "-vf",
         "scale=1:1", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        capture_output=True,
    )
    if len(p.stdout) < 3:
        return [255, 255, 255]
    return [int(b) for b in p.stdout[:3]]


def has_ffmpeg_webp() -> bool:
    out = subprocess.run(["ffmpeg", "-hide_banner", "-encoders"],
                         capture_output=True, text=True).stdout
    return " libwebp" in out or " webp" in out


def encode(src: Path, out_dir: Path, start_index: int) -> int:
    """Frames as WebP, by whichever route this machine can actually take.

    WEBP IS NOT A PREFERENCE, IT IS THE BUDGET. These plates ship in the repo
    and are fetched frame by frame at runtime. A 1280px JPEG of a photographic
    scene runs about 97KB; the existing photoreal reel averages 23KB in WebP for
    the same material. Six scenes is the difference between roughly 9MB and
    roughly 70MB of binaries — and the browser pays it again on every load.

    Many Homebrew ffmpeg builds ship without the WebP encoder ("Default encoder
    for format image2 (codec webp) is probably disabled"), which is exactly the
    trap this hit the first time. So: use ffmpeg when it can, and otherwise cut
    PNGs and hand them to `cwebp`, which is a dependency of practically every
    image toolchain on the machine already.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    vf = f"fps={TARGET_FPS},scale={TARGET_W}:-2"

    if has_ffmpeg_webp():
        run(["ffmpeg", "-y", "-v", "error", "-i", str(src), "-vf", vf,
             "-quality", str(WEBP_QUALITY), "-start_number", str(start_index),
             str(out_dir / "f%03d.webp")])
    elif shutil.which("cwebp"):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            run(["ffmpeg", "-y", "-v", "error", "-i", str(src), "-vf", vf,
                 "-start_number", str(start_index), str(tmp / "f%03d.png")])
            for png in sorted(tmp.glob("f*.png")):
                run(["cwebp", "-quiet", "-q", str(WEBP_QUALITY),
                     str(png), "-o", str(out_dir / (png.stem + ".webp"))])
    else:
        sys.exit(
            "no WebP encoder: this ffmpeg was built without one and `cwebp` is "
            "not on PATH. Install it (brew install webp) rather than shipping "
            "JPEGs — see the note in this function."
        )
    return len(list(out_dir.glob("f*.webp")))


def verdict(m: dict) -> tuple[str, list[str]]:
    notes: list[str] = []
    if m["zoom"] > ZOOM_FAIL:
        notes.append(
            f"the frame's halves diverge by {m['zoom']}px — that is a PUSH-IN or "
            "pull-back, not a locked shot. Generate from a still image with "
            "`--start-image` and ask only for movement inside the frame."
        )
        return "FAIL", notes
    if m["drift"] <= DRIFT_PASS:
        state = "PASS"
    elif m["drift"] <= DRIFT_WARN:
        state = "WARN"
        notes.append(
            f"camera shifts {m['drift']}px — a static overlay will show a little swim; "
            "usable if the device sits away from the moving region"
        )
    else:
        state = "FAIL"
        notes.append(
            f"camera shifts {m['drift']}px — it is not locked off. A DOM device "
            "placed once will not stay put. Regenerate with an explicitly "
            "static camera."
        )
    if m["busy"] > 12:
        notes.append(
            f"foreground busy-ness {m['busy']} — the placement band is cluttered; "
            "there may be nowhere clean to stand the device"
        )
    return state, notes


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src", type=Path)
    ap.add_argument("--id", required=True, help="scene id, e.g. kitchen")
    ap.add_argument("--device", default="laptop", choices=["laptop", "phone"])
    ap.add_argument("--commit", action="store_true",
                    help="encode frames and write the manifest (default: measure only)")
    args = ap.parse_args()

    if not shutil.which("ffmpeg") or not shutil.which("ffprobe"):
        sys.exit("ffmpeg and ffprobe are required")
    if not args.src.exists():
        sys.exit(f"no such file: {args.src}")

    meta = probe(args.src)
    m = measure(args.src, meta)
    light = light_sample(args.src)
    state, notes = verdict(m)

    print(f"\n  {args.src.name}  ·  {meta['width']}×{meta['height']}  "
          f"{meta['fps']:.0f}fps  {meta['duration']:.1f}s")
    print(f"  camera ............ {state}  (shift {m['drift']}px, "
          f"pass ≤ {DRIFT_PASS}, warn ≤ {DRIFT_WARN})")
    print(f"  zoom .............. {m['zoom']}px divergence  (pass < {ZOOM_FAIL})")
    print(f"  world motion ...... {m['residual']}  (movement INSIDE a still "
          f"frame — this is what we are paying for)")
    print(f"  foreground ........ busy-ness {m['busy']}")
    print(f"  room light ........ rgb{tuple(light)}")
    for n in notes:
        print(f"  → {n}")

    if state == "FAIL" and args.commit:
        sys.exit("\n  refusing to commit a plate whose camera is not locked off.\n")
    if not args.commit:
        print("\n  measure-only. Re-run with --commit to encode and write the "
              "manifest.\n")
        return

    man_path = REEL / "manifest.json"
    man = json.loads(man_path.read_text()) if man_path.exists() else {
        "frames": 0, "width": TARGET_W, "height": 0,
        "pattern": "f%03d.webp", "fps": TARGET_FPS, "version": 1, "scenes": [],
    }
    start = int(man.get("frames", 0)) + 1
    total = encode(args.src, REEL, start)
    end = total

    man["frames"] = total
    man["height"] = round(TARGET_W * meta["height"] / meta["width"] / 2) * 2
    man["version"] = int(man.get("version", 0)) + 1
    scenes = [s for s in man.get("scenes", []) if s.get("id") != args.id]
    scenes.append({
        "id": args.id,
        "start": start,
        "end": end,
        "device": args.device,
        # AUTHORED, not measured. Under the inversion the quad is where we
        # DECIDE to stand the device, so it starts as a sensible foreground
        # placement and is tuned by eye against the plate. There is nothing in
        # the video to detect.
        "screenQuad": None,
        "lightSample": light,
        "drift": m["drift"],
    })
    man["scenes"] = sorted(scenes, key=lambda s: s["start"])
    man_path.write_text(json.dumps(man, indent=2) + "\n")

    print(f"\n  wrote f{start:03d}..f{end:03d}.webp and manifest.json "
          f"(version {man['version']})")
    print(f"  screenQuad for '{args.id}' is null — author it against the plate "
          f"before the device will appear.\n")


if __name__ == "__main__":
    main()
