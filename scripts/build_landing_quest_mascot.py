#!/usr/bin/env python3
"""Extend the approved walking star with a seamless coin-collecting loop.

Uses the checked-in on-it rig, preserving its face, proportions, and companion.
Writes light/dark Lottie files to the brand source and runtime asset directories,
and adds the mood to the existing self-contained animation preview.
"""

import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "docs/assets/brand/animations"
RUNTIME = ROOT / "ui/unified-ui/static/brand/animations"
END = 96


def still(value):
    return {"a": 0, "k": value}


def animated(keys):
    frames = []
    for index, (time, value) in enumerate(keys):
        frame = {"t": time, "s": value if isinstance(value, list) else [value]}
        if index < len(keys) - 1:
            frame.update(o={"x": [0.42], "y": [0]}, i={"x": [0.58], "y": [1]})
        frames.append(frame)
    return {"a": 1, "k": frames}


def rgb(hex_color):
    return [int(hex_color[index:index + 2], 16) / 255 for index in (1, 3, 5)]


def path(points, closed=False):
    return {"ty": "sh", "ks": still({"i": [[0, 0]] * len(points),
            "o": [[0, 0]] * len(points), "v": points, "c": closed})}


def ellipse(diameter):
    return {"ty": "el", "p": still([0, 0]), "s": still([diameter, diameter])}


def fill(color):
    return {"ty": "fl", "c": still(rgb(color) + [1]), "o": still(100), "r": 1}


def stroke(color, width):
    return {"ty": "st", "c": still(rgb(color) + [1]), "o": still(100),
            "w": still(width), "lc": 2, "lj": 2}


def group(shapes):
    transform = {"ty": "tr", "p": still([0, 0]), "a": still([0, 0]),
                 "s": still([100, 100]), "r": still(0), "o": still(100),
                 "sk": still(0), "sa": still(0)}
    return {"ty": "gr", "nm": "", "it": shapes + [transform]}


def layer(name, shapes, position, scale, opacity):
    return {"ddd": 0, "ind": 0, "ty": 4, "nm": name, "sr": 1,
            "ks": {"o": opacity, "r": still(0), "p": position,
                   "a": still([0, 0, 0]), "s": scale},
            "ao": 0, "shapes": shapes, "ip": 0, "op": END, "st": 0, "bm": 0}


def stretch_time(value):
    if isinstance(value, dict):
        if value.get("a") == 1 and isinstance(value.get("k"), list):
            for frame in value["k"]:
                if isinstance(frame, dict) and "t" in frame:
                    frame["t"] *= 3
        for child in value.values():
            stretch_time(child)
    elif isinstance(value, list):
        for child in value:
            stretch_time(child)


def coin_shapes():
    stops = [(0, "#FFF4C2"), (0.45, "#FFC93C"), (1, "#D98F14")]
    gradient = {"ty": "gf", "o": still(100), "r": 1, "t": 2,
                "s": still([-8, -9]), "e": still([21, 22]),
                "h": still(0), "a": still(0),
                "g": {"p": len(stops), "k": still([
                    channel for offset, color in stops for channel in [offset, *rgb(color)]
                ])}}
    star = [[(9 if index % 2 == 0 else 4.5) * math.cos(math.radians(-90 + index * 36)),
             (9 if index % 2 == 0 else 4.5) * math.sin(math.radians(-90 + index * 36))]
            for index in range(10)]
    # Lottie paints shape groups in reverse order: keep the engraving on top.
    return [group([path(star, True), fill("#B67512")]),
            group([ellipse(33), stroke("#FFF0AD", 1.5)]),
            group([ellipse(44), gradient, stroke("#C58716", 2.5)])]


def build(dark):
    suffix = "-dark" if dark else ""
    data = json.loads((SOURCE / f"on-it{suffix}.json").read_text())
    stretch_time(data)
    data.update(op=END, nm="Magican star · side-quest · coin collector")
    rig = [item for item in data["layers"] if not item["nm"].startswith("speed ")]
    extras = []
    reward_color = "#FFC93C" if dark else "#61506F"
    for index, start in enumerate((0, 28, 56)):
        extras.append(layer(f"quest coin {index + 1}", coin_shapes(),
            still([454, 352, 0]),
            still([100, 100, 100]), still(0)))
        coin = extras[-1]
        keys = [(start, [454, 352, 0]), (start + 12, [412, 318, 0]),
                (start + 25, [348, 365, 0]), (END, [348, 365, 0])]
        coin["ks"]["p"] = animated(([(0, [454, 352, 0])] if start else []) + keys)
        coin["ks"]["s"] = animated([(0, [100, 100, 100]),
            (start + 6, [100, 100, 100]), (start + 12, [24, 100, 100]),
            (start + 18, [100, 100, 100]), (start + 25, [20, 20, 100]),
            (END, [20, 20, 100])])
        coin["ks"]["o"] = animated(([(0, 0)] if start else []) +
            [(start, 0), (start + 3, 100), (start + 21, 100), (start + 25, 0), (END, 0)])
        reward_start = start + 23
        reward_end = min(start + 46, END)
        reward_positions = [(0, [398, 302, 0]), (reward_start, [398, 302, 0]),
                            (reward_end, [406, 263, 0])]
        if reward_end < END:
            reward_positions.append((END, [406, 263, 0]))
        reward = [group([path([[-14, 0], [-4, 0]]), path([[-9, -5], [-9, 5]]),
                        path([[3, -4], [7, -8], [7, 8]]), path([[2, 8], [12, 8]]),
                        stroke(reward_color, 2.5)])]
        extras.append(layer(f"quest reward +1 {index + 1}", reward,
            animated(reward_positions),
            still([100, 100, 100]),
            animated([(0, 0), (reward_start, 0), (reward_start + 3, 100),
                      (reward_end - 3, 100), (reward_end, 0)])))
        sparkle = [[0, -12], [3, -3], [12, 0], [3, 3], [0, 12], [-3, 3], [-12, 0], [-3, -3]]
        extras.append(layer(f"quest collect sparkle {index + 1}",
            [group([path(sparkle, True), fill("#FFC93C")])], still([349, 362, 0]),
            animated([(0, [0, 0, 100]), (reward_start, [0, 0, 100]),
                      (reward_start + 3, [130, 130, 100]),
                      (reward_start + 11, [0, 0, 100]), (END, [0, 0, 100])]), still(100)))
    # Preserve the source rig's parent graph while inserting foreground props.
    old_to_new = {item["ind"]: index + len(extras) + 1 for index, item in enumerate(rig)}
    for index, item in enumerate(extras + rig, 1):
        if "parent" in item:
            item["parent"] = old_to_new[item["parent"]]
        item["ind"] = index
        item["op"] = END
    data["layers"] = extras + rig
    return data


def update_preview(light, dark):
    preview = SOURCE / "preview.html"
    html = preview.read_text()
    decoder = json.JSONDecoder()
    for variable, animation in (("L", light), ("K", dark)):
        # The preview declares light and dark together: const L={...},K={...}.
        start = html.index(f"{variable}=") + len(f"{variable}=")
        moods, length = decoder.raw_decode(html[start:])
        moods["side-quest"] = animation
        html = html[:start] + json.dumps(moods) + html[start + length:]
    if 'id="side-quest"' not in html:
        tile = '<figure><div class=a id="side-quest"></div><figcaption>side-quest · coins</figcaption></figure>'
        html = html.replace('</div>\n<script src=', tile + '</div>\n<script src=', 1)
    preview.write_text(html)


def main():
    light, dark = build(False), build(True)
    for suffix, animation in (("", light), ("-dark", dark)):
        encoded = json.dumps(animation, separators=(",", ":"))
        for directory in (SOURCE, RUNTIME):
            destination = directory / f"side-quest{suffix}.json"
            destination.write_text(encoded)
            print(f"{destination.relative_to(ROOT)}: {len(encoded):,} bytes")
    update_preview(light, dark)


if __name__ == "__main__":
    main()
