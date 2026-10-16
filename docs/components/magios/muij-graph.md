# MUIJ Graph Component (iOS / Magios)

Owner doc: [unified-ui/muij-native-ui-boundaries](../unified-ui/muij-native-ui-boundaries.md)
· renderer: `magios/Magios/MuijRenderer.swift` · tests: `magios/MagiosTests/MuijGraphTests.swift`

## What ships

The published-briefing renderer dispatches the MUIJ `Graph` component type
to a native SwiftUI projection. The bounded model
(`MuijGraphModel` behind `MuijComponentModel.graphModel`) normalizes props
defensively and mirrors the Rust validator caps: at most 200 nodes and 400
edges, node ids compared exactly (no trim) and capped at 128 characters,
node labels, kinds, and edge labels capped at 200 characters, flat scalar
node metadata capped at 8 sorted entries with 64-character keys, dangling
edges dropped, duplicate ids skipped. Every length cap counts Unicode
scalar values (matching the Rust validator's `chars()`), not composed
grapheme clusters. A hostile graph degrades to a smaller
graph instead of failing the briefing.

## Layouts (deterministic only)

- `layered` — deterministic topological tiers (`assignGraphTiers`); cycle
  members share one final tier, so cyclic graphs never loop or deadlock.
  Rendered as tier rows of tappable chips.
- `radial` — one ring in declaration order; edges drawn as paths beneath
  positioned chips.
- `list` — vertical stack of rows, each carrying a bounded adjacency summary.

## Interactions (read-only contract preserved)

Selection and focus are local view state only: tapping a node (or the
`focus_node_id` on first render) reveals a detail panel with the node's
kind, metadata and in/out edge counts. No control dispatches anywhere —
publication is not an authority grant. `reveal_order` renders statically on
iOS, which the shared wire contract permits.

## Old documents and old clients

Documents without `Graph` components render exactly as before. Older iOS
builds that receive a `Graph` component keep the existing unknown-type
dashed fallback with label and children; no forced upgrade.
