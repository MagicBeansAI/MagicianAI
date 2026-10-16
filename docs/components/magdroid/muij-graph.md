# MUIJ Graph Component (Android / Magdroid)

Owner doc: [unified-ui/muij-native-ui-boundaries](../unified-ui/muij-native-ui-boundaries.md)
· renderer: `magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/MuijRenderer.kt`
· model: `magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/today/MuijModels.kt`
· tests: `magdroid/android/bridge/src/test/kotlin/ai/magicbeans/magdroid/today/MuijGraphModelsTest.kt`

## What ships

The published-surface renderer dispatches the MUIJ `Graph` component type
to a native Compose projection. The bounded model
(`MuijGraphModel` behind `MuijComponent.graphModel`) normalizes props
defensively and mirrors the Rust validator caps: at most 200 nodes and 400
edges, flat scalar node metadata capped at 8 sorted entries, dangling edges
dropped, duplicate ids skipped. A hostile graph degrades to a smaller graph
instead of failing the briefing.

## Layouts (deterministic only)

- `layered` — deterministic topological tiers (`MuijGraphModel.assignTiers`);
  cycle members share one final tier, so cyclic graphs never loop or
  deadlock. Rendered as tier `FlowRow`s of tappable chips.
- `radial` — one ring in declaration order; edges drawn in a `Canvas`
  beneath offset chips.
- `list` — vertical stack of rows, each carrying a bounded adjacency summary.

## Interactions (read-only contract preserved)

Selection and focus are local compose state only: tapping a node (or the
`focus_node_id` on first render) reveals a detail panel with the node's
kind, metadata and in/out edge counts. No control dispatches anywhere —
publication is not an authority grant. `reveal_order` renders statically on
Android, which the shared wire contract permits.

## Old documents and old clients

Documents without `Graph` components render exactly as before. Older
Android builds that receive a `Graph` component keep the existing
unknown-type dashed fallback with label and children; no forced upgrade.
