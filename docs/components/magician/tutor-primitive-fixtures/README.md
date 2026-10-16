# Tutor Primitive — Shared Golden Fixtures

These JSON files are the **cross-platform parity** guarantee for the tutor
recipe interpreter. Each fixture is a self-contained
`{ recipe, shape, space, expected }` case:

- `recipe` — a `TutorRecipe` (same schema the backend serves).
- `shape` — a tutor shape (the `screen-draw` contract).
- `space` — the coordinate space `{ width, height }` (informational; the
  `expected` geometry is in **pre-projection space coordinates**).
- `expected` — an ordered list of `{ op, points, closed? }` — the
  space-coordinate geometry each stroked op produces. Label / text ops carry no
  stroked geometry and are **omitted** (matching the interpreter's
  `geometry(for:)` seam). Ops skipped for a missing coordinate produce no entry.

Both the TypeScript interpreter tests
(`ui/unified-ui/src/lib/tutor/recipeInterpreter.test.ts`) and the Swift
`RecipeInterpreterTests` load these SAME files and assert their
`geometry(for:)` output matches `expected`. If the two interpreters ever drift,
one of the two suites goes red.

Point ordering and the two-corner encodings match the reference implementation
`magios/Magios/RecipeInterpreter.swift` `geometry(for:)`:

- `rect` → `[ [x, y], [x+w, y+h] ]`
- `circle` → `[ [cx, cy], [cx+r, cy] ]`
- `arc` → 25 sampled points (24 segments), `from`/`to` in degrees
- `bezier` → `[ from, c1, (c2?), to ]`
- `arrowhead` → `[ from, to ]`
- `line` → `[ from, to ]`
- `polyline` / `polygon` → the projected vertex list (`polygon` sets `closed`)
