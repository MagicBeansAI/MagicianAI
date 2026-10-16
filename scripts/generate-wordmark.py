"""Render the wordmark to SVG outlines.

GitHub strips <style> and blocks external font loads through its image proxy,
so a README cannot use a webfont. The only way to show the name in Outfit is to
convert the glyphs to paths — the result needs no font installed anywhere.
"""
import sys
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.transformPen import TransformPen
from fontTools.misc.transform import Transform

FONT = Path("scripts/fonts/Outfit-Bold.ttf")
TEXT = "Magician"
# Rendered small in a README, where the bold weight closes the counters up. A
# little tracking keeps them open.
TRACKING = 12

font = TTFont(FONT)
glyphs = font.getGlyphSet()
cmap = font.getBestCmap()

paths, x = [], 0
bounds = BoundsPen(glyphs)
for char in TEXT:
    name = cmap.get(ord(char))
    if name is None:
        sys.exit(f"no glyph for {char!r}")
    pen = SVGPathPen(glyphs)
    glyphs[name].draw(pen)
    if d := pen.getCommands():
        paths.append(f'<path transform="translate({x} 0)" d="{d}"/>')
    # Same glyph again into a bounds pen, shifted, so the crop is the real ink
    # rather than the font's em box — which left a third of the image empty and
    # then clipped the ascenders once the y-flip was applied.
    glyphs[name].draw(TransformPen(bounds, Transform().translate(x, 0)))
    x += glyphs[name].width + TRACKING

if bounds.bounds is None:
    sys.exit("no ink")
x_min, y_min, x_max, y_max = bounds.bounds
pad = 24
width = (x_max - x_min) + pad * 2
height = (y_max - y_min) + pad * 2
# The glyph coordinate system has y up; SVG has y down, so the group is flipped.
# That flip is why the viewBox origin is -y_max and not y_min.
view_x = x_min - pad
view_y = -(y_max + pad)

def svg(colour):
    scale = 68 / height
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'viewBox="{view_x:.0f} {view_y:.0f} {width:.0f} {height:.0f}" '
        f'width="{width * scale:.0f}" height="68" role="img" aria-label="{TEXT}">'
        f'<g fill="{colour}" transform="scale(1 -1)">{"".join(paths)}</g></svg>\n'
    )

out = Path("docs/assets")
out.mkdir(parents=True, exist_ok=True)
# Two files: an <img> cannot inherit currentColor, and one mid-tone would look
# washed on both themes rather than right on either.
(out / "wordmark-light.svg").write_text(svg("#16201C"))
(out / "wordmark-dark.svg").write_text(svg("#E7EDE8"))
print(f"  ink bounds {x_min:.0f},{y_min:.0f} .. {x_max:.0f},{y_max:.0f}")
print(f"  wrote docs/assets/wordmark-{{light,dark}}.svg at {width * 68 / height:.0f}x68")
