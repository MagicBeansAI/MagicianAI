#!/usr/bin/env python3
"""Generate the shared iOS, Android, and Tauri Magican application icons.

The identity is the Outfit Bold ``magican`` wordmark on the coral field.
Outfit is one of the three families that actually render on the public
landing (see ``ui/unified-ui/src/lib/shared/themeFonts.ts``), so the icon
matches the first surface a new user meets.

The wordmark cannot survive every surface. Measured against real Outfit
Bold, ``magican`` reads cleanly down to 64px and turns to mush by 32px,
while the single ``M`` is still recognisably an ``M`` at 16px. Android
adds its own constraint: an adaptive icon only guarantees the centre 66 of
108 density-independent pixels, so a full-width wordmark gets cropped by
OEM masks.

So the brand resolves at two tiers, both in the same typeface:

  * the ``magican`` wordmark wherever the art is at least
    ``WORDMARK_MIN_PX`` on a side, and
  * the ``M`` mark below that, plus the menu-bar template and the Android
    adaptive foreground.

``.icns`` and ``.ico`` both carry independent artwork per size, so the
tier boundary lives inside a single file rather than forcing one
compromise across every size.

Requires Pillow. Uses ``iconutil`` for per-size ICNS when it is available
(macOS); falls back to Pillow's single-art ICNS writer elsewhere.
"""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]

# Vendored as an explicit build input rather than read out of the iOS app's
# font bundle — generating icons should not depend on what magios happens
# to ship. SIL Open Font License, retained alongside as Outfit-OFL.txt.
FONT_PATH = REPOSITORY_ROOT / "scripts/fonts/Outfit-Regular.ttf"

IOS_ICON_PATH = (
    REPOSITORY_ROOT
    / "magios/Magios/Assets.xcassets/AppIcon.appiconset/icon-1024.png"
)
DESKTOP_ICON_DIR = REPOSITORY_ROOT / "desktop/src-tauri/icons"
ANDROID_MIPMAP = REPOSITORY_ROOT / "magdroid/android/app/src/main/res"
ANDROID_MARK = ANDROID_MIPMAP / "drawable-xxhdpi" / "magican_mark.png"

CANVAS_SIZE = 1_024
SUPERSAMPLE = 4
CORAL = (255, 107, 107)
CREAM = (253, 252, 248)

# The two tiers of the same brand voice.
MARK = "m"
WORDMARK = "magican"

# Below this rendered pixel size the wordmark stops being legible and the
# mark takes over.
WORDMARK_MIN_PX = 32

# Painted widths against the 1024 canvas (86% fill).
IOS_WORDMARK_WIDTH = 880
IOS_MARK_WIDTH = 600
DESKTOP_WORDMARK_WIDTH = 880
DESKTOP_MARK_WIDTH = 550

# macOS wants the art inset inside its 1024 canvas rather than bled to the
# edge; iOS masks its own squircle and takes the full bleed.
DESKTOP_INSET = 64
DESKTOP_SIZE = CANVAS_SIZE - (DESKTOP_INSET * 2)
DESKTOP_CORNER_RADIUS = 208

# An adaptive icon guarantees only the centre 66dp of its 108dp canvas, and
# a typical mask shows about 72dp of it. Fitting inside 66dp is necessary
# but not sufficient: at 0.55 the mark filled ~82% of the visible circle and
# read as bursting out of it. 0.38 puts it at roughly half the visible
# diameter, which matches how the mark sits on the iOS square.
ANDROID_ADAPTIVE_RATIO = 108 / 48
ANDROID_SAFE_FRACTION = 0.38

ICNS_VARIANTS = (
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1_024),
)

ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def brand_font(size: int) -> ImageFont.FreeTypeFont:
    """Outfit Bold. The vendored file is a static instance, so there is no
    variation axis to select — a variable build would still load here."""

    return ImageFont.truetype(str(FONT_PATH), size)


def painted_glyph(text: str, font_size: int) -> Image.Image:
    """The text's real ink, cropped free of the font's metric box.

    Centering the metric box leaves the painted word visibly off-centre,
    because side bearings are not symmetric. Every caller positions the
    cropped coverage instead.
    """

    font = brand_font(font_size)
    raw_mask = font.getmask(text, mode="L")
    mask = Image.frombytes("L", raw_mask.size, bytes(raw_mask))
    painted_bounds = mask.getbbox()
    if painted_bounds is None:
        raise RuntimeError(f"brand text {text!r} rendered an empty mask")
    return mask.crop(painted_bounds)


def glyph_at_width(text: str, target_width: int) -> Image.Image:
    """Painted ink scaled to an exact width, always by downsampling.

    A fixed font size cannot serve both a single letter and a seven-letter
    word, so probe the text's natural proportions first and pick a size
    that renders wider than needed. Arriving at the target by shrinking
    keeps the stroke edges clean; upscaling a small mask would not.
    """

    probe_size = 400
    probe = painted_glyph(text, probe_size)
    oversized = max(8, round(probe_size * target_width / probe.width * 1.15))
    glyph = painted_glyph(text, oversized)
    target_height = round(glyph.height * target_width / glyph.width)
    return glyph.resize((target_width, target_height), Image.Resampling.LANCZOS)


def render_brand_canvas(text: str, glyph_target_width: int) -> Image.Image:
    """An opaque coral canvas carrying the cream text, optically centred."""

    high_resolution_size = CANVAS_SIZE * SUPERSAMPLE
    glyph = glyph_at_width(text, glyph_target_width * SUPERSAMPLE)
    x = (high_resolution_size - glyph.width) // 2
    y = (high_resolution_size - glyph.height) // 2

    icon = Image.new("RGB", (high_resolution_size, high_resolution_size), CORAL)
    cream = Image.new("RGB", glyph.size, CREAM)
    icon.paste(cream, mask=glyph, box=(x, y))
    return icon.resize((CANVAS_SIZE, CANVAS_SIZE), Image.Resampling.LANCZOS)


def render_desktop_icon(desktop_art: Image.Image) -> Image.Image:
    """Place the desktop-sized shared art in a transparent app silhouette."""

    desktop = Image.new("RGBA", (CANVAS_SIZE, CANVAS_SIZE), (0, 0, 0, 0))
    inset_art = desktop_art.convert("RGBA").resize(
        (DESKTOP_SIZE, DESKTOP_SIZE), Image.Resampling.LANCZOS
    )
    desktop.alpha_composite(inset_art, (DESKTOP_INSET, DESKTOP_INSET))

    mask_size = CANVAS_SIZE * SUPERSAMPLE
    mask = Image.new("L", (mask_size, mask_size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (
            DESKTOP_INSET * SUPERSAMPLE,
            DESKTOP_INSET * SUPERSAMPLE,
            (DESKTOP_INSET + DESKTOP_SIZE) * SUPERSAMPLE - 1,
            (DESKTOP_INSET + DESKTOP_SIZE) * SUPERSAMPLE - 1,
        ),
        radius=DESKTOP_CORNER_RADIUS * SUPERSAMPLE,
        fill=255,
    )
    desktop.putalpha(
        mask.resize((CANVAS_SIZE, CANVAS_SIZE), Image.Resampling.LANCZOS)
    )
    return desktop


def resized(image: Image.Image, size: int) -> Image.Image:
    return image.resize((size, size), Image.Resampling.LANCZOS)


def desktop_art_for(size: int, tiers: dict[str, Image.Image]) -> Image.Image:
    """The tier that stays legible once the icon is painted at `size`."""

    tier = tiers["wordmark"] if size >= WORDMARK_MIN_PX else tiers["mark"]
    return resized(tier, size)


def write_icns(tiers: dict[str, Image.Image]) -> None:
    """Per-size ICNS via `iconutil`, so 16px carries the mark and 512px the
    wordmark. Pillow's writer takes a single image, so a machine without
    `iconutil` falls back to the wordmark at every size."""

    iconutil = shutil.which("iconutil")
    if iconutil is None:
        tiers["wordmark"].save(DESKTOP_ICON_DIR / "icon.icns", format="ICNS")
        return

    with tempfile.TemporaryDirectory() as scratch:
        iconset = Path(scratch) / "icon.iconset"
        iconset.mkdir()
        for name, size in ICNS_VARIANTS:
            desktop_art_for(size, tiers).save(iconset / name, format="PNG")
        subprocess.run(
            [
                iconutil,
                "--convert",
                "icns",
                str(iconset),
                "--output",
                str(DESKTOP_ICON_DIR / "icon.icns"),
            ],
            check=True,
        )


def write_ico(tiers: dict[str, Image.Image]) -> None:
    """Per-size ICO frames — Pillow matches an appended image to each
    requested size rather than downsampling the base art.

    The base image must be the *largest* frame: Pillow silently drops any
    requested size bigger than the image it was handed, so seeding this
    with the 16px frame yields a single-frame icon.
    """

    ordered = sorted(ICO_SIZES, reverse=True)
    frames = [desktop_art_for(size, tiers) for size in ordered]
    frames[0].save(
        DESKTOP_ICON_DIR / "icon.ico",
        format="ICO",
        sizes=[(size, size) for size in ordered],
        append_images=frames[1:],
    )


def write_icons() -> None:
    if not FONT_PATH.is_file():
        raise RuntimeError(f"missing vendored brand font: {FONT_PATH}")

    ios_wordmark = render_brand_canvas(WORDMARK, IOS_WORDMARK_WIDTH)
    ios_mark = render_brand_canvas(MARK, IOS_MARK_WIDTH)
    desktop_tiers = {
        "wordmark": render_desktop_icon(
            render_brand_canvas(WORDMARK, DESKTOP_WORDMARK_WIDTH)
        ),
        "mark": render_desktop_icon(
            render_brand_canvas(MARK, DESKTOP_MARK_WIDTH)
        ),
    }

    # Apple rejects iOS AppIcon sources with alpha, so retain an RGB master.
    # The home screen paints this at 180px, well clear of the wordmark floor.
    ios_wordmark.save(IOS_ICON_PATH, format="PNG", optimize=True)

    for size, name in (
        (32, "32x32.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        # Plain 256 alongside the Retina-named twin, for the places that want
        # an icon by its pixel size rather than by Tauri's naming convention.
        (256, "256x256.png"),
        (512, "icon.png"),
    ):
        desktop_art_for(size, desktop_tiers).save(
            DESKTOP_ICON_DIR / name, format="PNG"
        )

    write_ico(desktop_tiers)
    write_icns(desktop_tiers)
    write_android_icons(ios_wordmark, ios_mark)
    write_tray_mask()


def write_android_icons(
    ios_wordmark: Image.Image, ios_mark: Image.Image
) -> None:
    """Legacy square launchers plus the adaptive foreground.

    The adaptive foreground is the cream mark on transparency — the coral
    ground comes from `@drawable/ic_launcher_background`, and the same
    asset doubles as the `<monochrome>` layer, which only works if it is
    a silhouette rather than a filled square.
    """

    densities = {
        "mipmap-mdpi": 48,
        "mipmap-hdpi": 72,
        "mipmap-xhdpi": 96,
        "mipmap-xxhdpi": 144,
        "mipmap-xxxhdpi": 192,
    }
    for folder, size in densities.items():
        dest = ANDROID_MIPMAP / folder
        dest.mkdir(parents=True, exist_ok=True)

        # Pre-26 launchers paint the finished square, so they keep the
        # wordmark wherever it is still large enough to read.
        launcher = resized(
            ios_wordmark if size >= WORDMARK_MIN_PX else ios_mark, size
        )
        launcher.save(dest / "ic_launcher.png", format="PNG")
        launcher.save(dest / "ic_launcher_round.png", format="PNG")

        adaptive_size = round(size * ANDROID_ADAPTIVE_RATIO)
        render_adaptive_foreground(adaptive_size).save(
            dest / "ic_launcher_foreground.png", format="PNG"
        )

    # The splash layout pairs this image with the app name as a TextView, so
    # it takes the mark rather than the wordmark — the wordmark here would
    # print the name twice, once as art and once as text.
    ANDROID_MARK.parent.mkdir(parents=True, exist_ok=True)
    resized(ios_mark, 144).save(ANDROID_MARK, format="PNG")
    drawable = ANDROID_MIPMAP / "drawable"
    drawable.mkdir(parents=True, exist_ok=True)
    resized(ios_wordmark, 192).save(drawable / "ic_launcher.png", format="PNG")


def render_adaptive_foreground(size: int) -> Image.Image:
    """The cream mark on transparency, held inside the adaptive safe zone."""

    supersample = 4
    canvas = size * supersample
    glyph = glyph_at_width(MARK, round(canvas * ANDROID_SAFE_FRACTION))
    foreground = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    cream = Image.new("RGBA", glyph.size, CREAM + (255,))
    foreground.paste(
        cream,
        ((canvas - glyph.width) // 2, (canvas - glyph.height) // 2),
        glyph,
    )
    return foreground.resize((size, size), Image.Resampling.LANCZOS)


def write_tray_mask() -> None:
    """18px template mask of the Magican m for the macOS menu bar.

    The menu bar is monochrome and 18px on a side — two reasons the
    wordmark can never appear here.
    """

    size = 18
    supersample = 8
    canvas = size * supersample
    glyph = painted_glyph(MARK, canvas * 5 // 4)
    # `menu_bar_template_icon_is_a_visible_native_m_mask` requires every
    # border pixel to stay under alpha 4.
    target_width = int(canvas * 0.68)
    target_height = round(glyph.height * target_width / glyph.width)
    glyph = glyph.resize(
        (target_width, target_height), Image.Resampling.LANCZOS
    )
    tray = Image.new("L", (canvas, canvas), 0)
    tray.paste(
        glyph,
        ((canvas - target_width) // 2, (canvas - target_height) // 2),
        glyph,
    )
    tray = tray.resize((size, size), Image.Resampling.LANCZOS)
    (DESKTOP_ICON_DIR / "tray-mask-18.bin").write_bytes(bytes(tray.getdata()))


if __name__ == "__main__":
    write_icons()
    print("Generated Magican iOS, Android, and Tauri application icons.")
