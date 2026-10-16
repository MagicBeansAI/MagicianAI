# MUIJ, GAUI Live, and Native UI Boundaries

MUIJ is the Magician UI JSON document format for serialized, published,
replayed, agent-generated or live-delta UIs. GAUI Live is the runtime around it:
backend `agent.ui.delta` envelopes, the frontend `muijStore`, and visible UI
through `MuijRenderer`. Native UI is hand-authored Svelte. Stable product pages
use native Svelte unless they need those document or runtime properties.

## Current Boundary

Use MUIJ for:

- Published surfaces and briefing render output.
- Agent-generated or synthesized surfaces.
- Replayable or shareable surface documents.
- Live runtime surfaces driven by `agent.ui.delta`, `agent.ui.snapshot` and
  `ui.interaction`.
- Component catalogs meant for agent or compiler composition.

Use native Svelte for:

- Stable product routes with hand-authored layout.
- Form-heavy or focus-sensitive workflows.
- Page chrome, menus, shells, modals and split views that need direct DOM
  control.
- Dense operational screens where JSON indirection obscures behavior.

`/dashboard` is not an app route; Crew is the operational landing surface.
`/briefing/[id]`, `PublishedScrollCanvas` and V3 published-surface render paths
stay on MUIJ.

## Live GAUI Beachhead

`ui/unified-ui/src/lib/magician/components/LiveAgentSurface.svelte` is the first visible
production consumer of the live MUIJ store. It is a runtime surface, not the
agent profile UI; the live document often holds operational instrumentation
(agent logs, target selectors, trace tabs, run trees). It:

- Accepts an `agentId` and subscribes to `getComponentsByAgent(agentId)`.
- Requests a snapshot with `requestAgentSnapshotIfNeeded(agentId, v2Events)`.
- Renders live components with `MuijRenderer`.
- Forwards local interactions while letting `MuijRenderer` send configured
  `ui.interaction` websocket messages.

It mounts in the crew record (`CrewMemberWorkspace.svelte`,
`/work?view=crew&agent=…`), directly below the header, above the static
definition sections.

## Migration Rules

- Behavior first. Replace one region at a time with native components and keep
  interaction handlers.
- Prefer shared native primitives (page headers, metric grids, status badges,
  task cards, action toolbars, data panels, empty states).
- Keep `MuijRenderer` as a boundary renderer for MUIJ documents, not the
  default design system.
- Keep MUIJ component types stable while a published or generated surface can
  reference them.
- Add tests around converted interaction handlers before deleting MUIJ builder
  logic.

## Graph Node Family

`Graph` is a declarative node/edge MUIJ component type rendered natively by all
four MUIJ clients: web (`components/generative/Graph.svelte`), iOS
(`magios/Magios/MuijRenderer.swift`), Android
(`magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/MuijRenderer.kt`), and the Rust validator
(`magician/src/magician_v2/gaui/muij.rs`). It adds no security surface: it is
data-only, and its interactions (select, expand, focus, reveal) are local, not
dispatched authority.

### Schema (component_type `Graph`)

```json
{
  "nodes": [{ "id": "a", "label": "Alpha", "kind": "task",
              "metadata": { "status": "active" } }],
  "edges": [{ "from": "a", "to": "b", "label": "drives" }],
  "layout": "layered",
  "focus_node_id": "a",
  "reveal_order": ["a", "b"]
}
```

- `nodes`: `{ id, label, kind?, metadata? }`; `metadata` is a flat map of
  scalars (string/number/boolean) shown in the detail panel.
- `edges`: `{ from, to, label? }`; both endpoints must be declared node ids.
- `layout`: `layered` (default) | `radial` | `list`. Deterministic only, no
  physics; every position derives from declaration order.
- `focus_node_id`: optional initial selection; must be a declared node.
- `reveal_order`: optional node-id sequence for the staggered reveal; every id
  declared and listed once.

### Bounds (fail-closed in the Rust validator)

| Bound | Value |
|---|---|
| Nodes per component | 200 |
| Edges per component | 400 |
| Reveal-order entries | 200 (each declared once) |
| Node id / label / kind / edge label | 128 / 200 / 200 / 200 chars |
| Metadata per node | 8 entries, 64-char keys, 200-char string values |

A malformed graph (bad shape, duplicate node ids, dangling edge, unknown layout,
undeclared focus or reveal id, or any cap exceeded) fails
`MuijDocument::validate` (`MaxGraphNodes`, `MaxGraphEdges`,
`InvalidGraphProps`). Documents without `Graph` validate exactly as before.

Renderers mirror these caps: ids compare exactly as in the Rust validator (no
trim); JS length caps count Unicode code points; over-long node ids are
dropped.

### Renderer contract

- **layered**: deterministic topological tiers (Kahn); roots form the first
  tier, and cycle members share one final tier. Web draws SVG; iOS renders tier
  rows of chips; Android renders tier `FlowRow`s.
- **radial**: one ring in declaration order (web SVG, iOS positioned ZStack
  with edge paths, Android `Canvas` ring with offset chips).
- **list**: vertical stack, each node with a bounded adjacency summary
  (`→ target labels`).
- **select/expand**: tapping a node shows a detail panel (label, kind,
  metadata, in/out edge counts). Web also emits the standard MUIJ `action`
  interaction (`action: 'select'`, `rowId`); mobile stays read-only per the
  published-surface contract.
- **focus**: `focus_node_id` is the initial selection everywhere.
- **reveal**: `reveal_order` drives a staggered fade-in on web (CSS
  `animation-delay`, off under `prefers-reduced-motion`); iOS and Android
  render statically.
- All clients skip malformed members, drop dangling edges and cap admission at
  200/400, so a hostile document degrades to a smaller graph instead of
  failing the whole briefing.

### Old-client degradation

A client that does not know `Graph` keeps its unknown-node behavior: web
renders the dashed unknown-type chip (dev builds also show the unsupported-type
error); iOS and Android render their dashed fallback with label and children.
No client upgrade is needed to receive a `Graph` document.

### Boundary

`Graph` is a MUIJ-layer family only, not a compilable V1 app-view kind in
`apps/surface_compiler.rs` (see
`docs/components/magician/app-default-surfaces.md`).

## Done Criteria

A route is migrated when:

- It no longer builds ordinary product UI as `MuijComponent[]`.
- It uses MUIJ only for embedded generated, published or live-agent regions.
- Route-specific MUIJ builder code is deleted or renamed as a generated-surface
  builder.
- Keyboard and focus behavior is native and covered by focused tests where
  stateful controls changed.
