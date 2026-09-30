# Tutor Primitive Recipes — Authoring Reference

> Contributor-facing authoring reference and the interpreter spec of record.
> Design: `docs/archive/plans/2026-07-13-data-driven-tutor-primitives.md` (§1.4 +
> Appendix A is normative).

The Tutor draws on-screen shapes ("primitives") for the personal tutor overlay.
A primitive is a **declarative JSON recipe**, not per-platform code. The magician backend loads + merges recipes from the Magician root,
serves them from one endpoint, and each client (iOS today, web next) runs a
generic interpreter that executes the recipe's draw-ops against the native
canvas. **A new primitive is a new file, not new code — no app rebuild.**

Recipes are pure geometry data. There is no code execution, no `eval`, no loops
— only a fixed, safe op vocabulary and a tiny pure-expression evaluator.

---

## Where recipes live

```
$HOME/MagicianNotes/                         (= MAGICIAN_ROOT_DIR)
  tutor_primitives/*.json                    ← shared/global built-ins (seeded)
  scopes/<principal>/<workspace>/
    tutor_primitives/*.json                  ← per-scope custom + community recipes
```

- The **built-in set** ships as a seed template under
  `magician_data_v3/system/tutor_primitives/*.json` and is copied to
  `$HOME/MagicianNotes/tutor_primitives/` on first run (create-if-missing — never
  overwrites your edits, never deletes).
- **Merge order:** global built-ins first, then scope files override by `type`.
  Everything is additive. A scope can add a brand-new primitive or override a
  built-in by re-using its `type`.
- Invalid recipes are **skipped + logged** on the backend — a bad community file
  can never crash the run or break the rest of the set.

---

## Recipe schema

```jsonc
{
  "type": "angle_marker",          // required — the primary shape `type`
  "aliases": ["arc", "..."],       // optional — extra shape types that use this recipe
  "version": 1,                    // optional — recipe format version
  "defaults": { "size": 36 },      // optional — fallback values for missing fields
  "draw": [ /* ordered list of draw-ops */ ]
}
```

- **`type`** — the shape `type` this recipe renders. One recipe per file.
- **`aliases`** — additional `type` strings that render with the *same* geometry
  (group identical-geometry variants here instead of duplicating files).
- **`defaults`** — values used when a referenced field is absent on the shape.
  Resolution order for an identifier is: raw shape field → injected derived field
  → recipe `defaults`.
- **`draw`** — an ordered array of ops. They render in order (so a background
  fills before its label, etc.).

Top-level keys other than these are rejected by the validator.

---

## Op vocabulary

Each op produces a `Path` or text. The interpreter strokes/fills it, applying the
shape's draw-on `progress` (0→1) uniformly. Coordinates are authored in the
shape's coordinate space and projected to screen by the interpreter.

| op | params | draws |
|----|--------|-------|
| `line` | `from:[x,y]`, `to:[x,y]` | a segment |
| `polyline` | `points:[[x,y]…]` **or** a field ref (`"points\|d"`) | connected segments |
| `polygon` | `points:[[x,y]…]` **or** a field ref | closed polygon (fillable) |
| `rect` | `x,y,w,h`, `radius?`(6) | rounded rect (fillable) |
| `circle` | `cx,cy,r` | circle/ellipse (fillable) |
| `arc` | `cx,cy,r,from,to` (degrees) | circular arc (sampled 24 segments → projected) |
| `bezier` | `from,to,c1,c2?` | cubic if `c2` present, else quadratic |
| `arrowhead` | `from,to` | a V head at `to` |
| `label` | `at:[x,y]`, `text`, `anchor?`, `threshold?` | text |
| `cursive_label` | `at:[x,y]`, `text`, `size?`, `anchor?` | text in a cursive face |

### Per-op modifiers (all optional)

| modifier | default | meaning |
|----------|---------|---------|
| `fill` | inherit | `true` forces fill; `false` disables. Absent → fill iff the shape carries a `fill` (rect/circle/polygon only). |
| `stroke` | `true` | set `false` to skip the outline (e.g. a solid callout background). |
| `dashed` | `false` | dashed stroke (dash pattern `[width*2, width*1.5]`). |
| `color` | shape `color` → `#ffcc00` | stroke/fill color (named color or `#rrggbb`). |
| `width` | shape `stroke_width` → `4` | stroke width (min 1). |
| `opacity` | shape `opacity` → `1` (stroke) / `0.22` (fill) | alpha. |
| `radius` | `6` | `rect` corner radius. |
| `anchor` | `center` | `label`/`cursive_label` text anchor (`leading`/`center`/`trailing`). |
| `threshold` | `0.15` labels, `0.85` arrowhead | progress value past which the op appears (draw-on gating). |

### Op behavior (normative)

- `line`/`polyline`/`arc`/`bezier` never fill.
- `rect`/`circle`/`polygon` fill iff `op.fill == true` **OR** (`op.fill != false`
  **AND** the shape has a `fill`).
- All stroked paths animate on via `trimmedPath(0…progress)`.
- `arrowhead` draws only when `progress > 0.85` (head
  length `max(12, width*4)`, ±30° from the segment angle).
- `label`/`cursive_label` render when `progress` exceeds their `threshold`
  (default `0.15`). Labels receive the overlay's `labelYOffset` for overlap
  avoidance.
- `arc` is sampled in 24 segments in space-coords, then each sample is projected.
- `rect` corner radius = `op.radius ?? 6`. `circle` radius comes from `r`,
  projected via the x-axis.

---

## Styling defaults

- stroke color = `op.color ?? shape.color ?? "#ffcc00"`, opacity `op.opacity ?? shape.opacity ?? 1`
- fill color = `op.color ?? shape.fill ?? shape.color`, opacity `op.opacity ?? 0.22`
- width = `op.width ?? shape.stroke_width ?? 4` (min 1)

---

## Values & the expression grammar

Any op param may be one of:

1. a **number** — `28`
2. a **field / coalesce chain** — `"cx|x"` → first defined of `cx`, then `x`
3. an **expression** — `"x+size"`, `"r*0.5"`, `"cos(deg(start_angle))*r"`

`[x, y]` coordinate params are two values, each independently any of the above.
Some ops (`polyline`/`polygon`) accept `points` as either a literal
`[[x,y]…]` array or a single field reference (`"points|d"`) that pulls the shape's
`points` array (or parses its `d` path string).

**Grammar (tiny, safe, pure — identical in Swift + TS):**

- number literals, identifiers (resolved as above)
- parentheses `( )`
- binary `+ - * /`, unary `-`
- coalesce `|` — **lowest precedence**, "first defined operand"
- functions: `sin cos tan sqrt abs min max deg rad`
  (`deg` = degrees→radians; `rad` = radians→degrees)

No variables, no assignment, no loops, no side effects. An undefined identifier in
a coalesce chain is skipped; an undefined identifier **outside** a chain makes the
value "missing", and an op is **skipped** if a required coordinate is missing/NaN.

**Bounds (hostile-input guard):** ≤ 512 recipes, ≤ 64 ops/recipe, ≤ 256
points/op, expression depth ≤ 32.

---

## Injected derived fields

So recipes avoid long alias chains, the interpreter injects these before
evaluation:

| field | definition |
|-------|-----------|
| `sx` | `from_x ?? x1` |
| `sy` | `from_y ?? y1` |
| `ex` | `to_x ?? x2` |
| `ey` | `to_y ?? y2` |
| `cx` | raw `cx` ?? `x` |
| `cy` | raw `cy` ?? `y` |
| `w` | `w ?? width` |
| `h` | `h ?? height` |
| `mx` | `(sx+ex)/2` |
| `my` | `(sy+ey)/2` |
| `side_sign` | `-1` if `side ∈ {"right","-1"}` or `orientation=="right"`, else `1` |
| `text_len` | character count of the shape's resolved `text` |
| `text_w` | **measured** width of the resolved `text` at the label font |
| `text_h` | **measured** glyph-box height (ascent + descent), not line height |
| `text_rise` | measured ink **above the label's own anchor point** |

### Sizing a background around text

**Use `text_w`/`text_h`/`text_rise`, not `text_len`.** A character count cannot
see glyph width and `label` does not wrap, so a count-sized box overflows.

**`text_rise` is anchor-relative, not the font ascent.** The web draws SVG
`<text>` with no `dominant-baseline` (`y` is the baseline, rise = ascent); iOS
draws via `GraphicsContext.draw(at:anchor:)` with `.leading` =
`UnitPoint(0, 0.5)` (vertically centred, rise = half the glyph box). Write
`y-text_rise-pad` to get a correct box on both.

Measurement is per-platform, so a text-sized recipe has no single cross-platform
golden fixture: `tutor-primitive-fixtures/callout.json` pins a count-based recipe
for expression evaluation only; the shipped callout is covered by per-renderer
tests. Measurement is injected — `renderRecipe({ measureText })` on the web,
`RecipeInterpreter.measureLabel` on iOS. Without a web measurer,
`estimateTextMetrics` reproduces the `text_len * 9` width, so unmeasured callers
keep their horizontal geometry.

### An ellipse is a first-class request

A shape may carry `rx`/`ry` instead of `r`; `angle_marker` (aliased `arc`)
coalesces `rx|r|size` so circular callers are unchanged. `rx`/`ry` must be
readable at every layer (TS `RecipeShape` whitelist, Swift `TutorShape`, the
recipe's coalesce chain). The backend passes shapes through as opaque JSON, so
**a field the model can send but no layer can read fails as a plausible-looking
default, not as an error.**

### Shipping a change to an existing recipe — bump `version`

Built-ins are seeded from `magician_data_v3/system/tutor_primitives/` into the
runtime root's `tutor_primitives/` at boot. **Editing a shipped recipe without
bumping its `version` does not reach any machine that already has the file.**
`seed_builtin_recipes` overwrites only when the built-in's `version` is strictly
higher:

| runtime file | outcome |
|---|---|
| absent | created |
| lower `version` | **upgraded** |
| equal or higher `version` | untouched |
| no `version` | untouched — this is the shape of a hand-written recipe |
| unparseable | untouched, with a `warn!` naming the path |

"No version" never reads as "old", which keeps user recipes safe; an unparseable
file is already inert (`load_dir` skips it) but may be a half-finished edit.
`SeedOutcome { created, upgraded }` is logged at boot; non-zero `upgraded` shows
a built-in fix landed.

### Parametric solids — proportions the model cannot get wrong

`cone` and `sector` take **real measurements** and derive every point from them.

| primitive | inputs | derives |
|---|---|---|
| `cone` | `cx`, `cy`, `r` (base radius), `h` (height) | base ellipse (`rx=r`, `ry=r*ry_ratio`), both slants, the shared apex |
| `sector` | `cx`, `cy`, `r` (slant length), `size` (base radius) | sweep `360*size/r`, the arc, both radii |

Why: `r` and `h` are the taught quantities; hand-assembled cones let base width
and slant drift independently. `sector` bakes in `θ·l = 2πr` (unrolled lateral
surface arc = base circumference), so the unrolled figure matches its solid.
Pass `end_angle` to teach a general sector.

`arc` accepts optional `rx`/`ry`, each falling back to `r`, so old arc recipes
keep their geometry. Sampling adapts to the sweep (24 segments per 90°, capped
at 96).

Note: derived `cx`/`cy` prefer a **raw `cx`/`cy`** over `x`/`y`. Primitives that
want `x`/`y` first (e.g. `right_angle_marker`) write the explicit chain `"x|cx"`.

---

## Worked examples

### `angle_marker` — an arc (with aliases)

```json
{
  "type": "angle_marker",
  "aliases": ["arc", "perpendicular_marker", "parallel_marker"],
  "version": 1,
  "defaults": { "size": 36, "start_angle": 0, "end_angle": 90 },
  "draw": [
    { "op": "arc", "cx": "cx", "cy": "cy", "r": "r|size",
      "from": "start_angle", "to": "end_angle" }
  ]
}
```

Center resolves via derived `cx`/`cy` (raw `cx`/`cy` else `x`/`y`); radius is the
first defined of `r`, `size`, then the default `36`; angles default 0→90 degrees.

### `right_angle_marker` — an L polyline

```json
{
  "type": "right_angle_marker",
  "version": 1,
  "defaults": { "size": 28 },
  "draw": [
    { "op": "polyline",
      "points": [
        ["(x|cx)+size", "y|cy"],
        ["(x|cx)+size", "(y|cy)+size"],
        ["x|cx", "(y|cy)+size"]
      ] }
  ]
}
```

A small L at the vertex: `(x+s,y) → (x+s,y+s) → (x,y+s)`. This one wants `x`/`y`
**before** `cx`/`cy`, so it writes explicit `"x|cx"`
chains instead of the derived `cx`.

### `dimension_line` — a new example (line + end ticks + measurement label)

```json
{
  "type": "dimension_line",
  "aliases": ["dimension"],
  "version": 1,
  "defaults": { "size": 10 },
  "draw": [
    { "op": "line", "from": ["sx", "sy"], "to": ["ex", "ey"] },
    { "op": "line",
      "from": ["sx-(-(ey-sy)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size",
               "sy-((ex-sx)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size"],
      "to":   ["sx+(-(ey-sy)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size",
               "sy+((ex-sx)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size"] },
    { "op": "line",
      "from": ["ex-(-(ey-sy)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size",
               "ey-((ex-sx)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size"],
      "to":   ["ex+(-(ey-sy)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size",
               "ey+((ex-sx)/sqrt((ex-sx)*(ex-sx)+(ey-sy)*(ey-sy)))*size"] },
    { "op": "label", "at": ["mx", "my-14"], "text": "text",
      "anchor": "center", "threshold": 0.75 }
  ]
}
```

The main segment plus perpendicular end-ticks (the perpendicular unit vector is
`(-(ey-sy), (ex-sx))` normalized by the segment length via `sqrt`), and a
centered measurement label at the midpoint. Purely declarative — proof that novel
primitives compose from the fixed op vocabulary with **no code change**.

---

## How to add a new primitive

1. Write a JSON file — `my_primitive.json` — with a unique `type`, optional
   `aliases`/`defaults`, and a `draw` list built from the op vocabulary above.
2. Drop it in your scope's folder:
   `$HOME/MagicianNotes/scopes/<principal>/<workspace>/tutor_primitives/`
   (or `$HOME/MagicianNotes/tutor_primitives/` to add it globally).
3. Validate it parses:
   `python3 -m json.tool my_primitive.json` (or the loop below for a whole folder).
4. Restart / re-fetch — the backend loads + merges it and serves it from
   `GET /api/magician/v2/tutor/primitives`; clients cache and render it. **No app
   rebuild.**

To validate a whole folder at once:

```sh
for f in *.json; do python3 -m json.tool "$f" > /dev/null || echo "BAD: $f"; done
```

Tips:
- Keep it declarative — express geometry with ops + expressions, never assume a
  new op. A genuinely new op is a rare, deliberate cross-platform addition to
  *both* interpreters.
- Group identical-geometry variants via `aliases` rather than copying files.
- Reference the built-in seed set
  (`magician_data_v3/system/tutor_primitives/*.json`) for patterns — every
  current primitive is expressed there as a recipe.
