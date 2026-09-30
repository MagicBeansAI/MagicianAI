# MUIJ, GAUI Live, and Native UI Boundaries

MUIJ is the Magician UI JSON document format: serialized, published, replayed, agent-generated, or live-delta UIs. GAUI Live is the runtime around it: backend `agent.ui.delta` envelopes, frontend `muijStore`, visible UI through `MuijRenderer`. Native UI is hand-authored Svelte. Stable product pages use native Svelte unless they need those document/runtime properties.

## Current Boundary

Use MUIJ for:

- Published surfaces and briefing render output.
- Agent-generated or synthesized surfaces.
- Replayable/shareable surface documents.
- Live runtime surfaces driven by `agent.ui.delta`, `agent.ui.snapshot`, and `ui.interaction`.
- Component catalogs intended for agent or compiler composition.

Use native Svelte for:

- Stable product routes whose layout is hand-authored.
- Form-heavy or focus-sensitive workflows.
- Page chrome, menus, shells, modals, and split views that need direct DOM control.
- Dense operational screens where JSON surface indirection makes behavior harder to reason about.

`/dashboard` is not an app route. Crew is the operational landing surface.
`/briefing/[id]`, `PublishedScrollCanvas`, and V3 published-surface render paths stay on MUIJ.

## Live GAUI Beachhead

`src/lib/magician/components/LiveAgentSurface.svelte` is the first visible production consumer of the live MUIJ store. It is a runtime surface, not primary agent profile UI; the live document often contains operational instrumentation (agent logs, target selectors, trace tabs, run trees).

It:

- Accepts an `agentId`.
- Subscribes to `getComponentsByAgent(agentId)`.
- Requests a snapshot with `requestAgentSnapshotIfNeeded(agentId, v2Events)`.
- Renders live components with `MuijRenderer`.
- Forwards local renderer interactions while allowing `MuijRenderer` to send configured `ui.interaction` websocket messages.

The initial mount is `/crew/[id]`, directly below the route header where agent identity is already known and the live execution state belongs above the slower/static definition sections.

## Migration Rules

- Keep behavior first. Replace one surface region at a time with native components and preserve interaction handlers.
- Prefer shared native primitives such as page headers, metric grids, status badges, task cards, action toolbars, data panels, and empty states.
- Leave `MuijRenderer` as a boundary renderer for MUIJ documents, not as the default design system.
- Keep MUIJ component types stable while a published or generated surface can reference them.
- Add tests around converted interaction handlers before deleting the MUIJ builder logic.

## Graph Node Family

`Graph` is a declarative node/edge MUIJ component type rendered natively by
all four MUIJ clients: web (`Graph.svelte` in this package), iOS
(`magios/Magios/MuijRenderer.swift`), Android (`magdroid/.../ui/MuijRenderer.kt`),
and the Rust validator (`magician/src/magician_v2/gaui/muij.rs`). It adds no
new security surface: like every MUIJ component it is data-only, and its
interactions are local (select/expand/focus/reveal) rather than dispatched
authority.

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
  scalar values (string/number/boolean) shown by the detail interaction.
- `edges`: `{ from, to, label? }`; both endpoints must reference declared
  node ids.
- `layout`: `layered` (default) | `radial` | `list`. Deterministic only —
  no physics, no simulation; every position derives from declaration order.
- `focus_node_id`: optional initial selection; must reference a declared node.
- `reveal_order`: optional node-id sequence for the staggered reveal
  interaction; every id must be declared and appear once.

### Bounds (fail-closed in the Rust validator)

| Bound | Value |
|---|---|
| Nodes per component | 200 |
| Edges per component | 400 |
| Reveal-order entries | 200 (each declared once) |
| Node id / label / kind / edge label | 128 / 200 / 200 / 200 chars |
| Metadata per node | 8 entries, 64-char keys, 200-char string values |

A malformed graph (bad shape, duplicate node ids, dangling edge endpoint,
unknown layout, undeclared focus/reveal id, or any cap exceeded) fails
`MuijDocument::validate` with the same error style as the other validators
(`MaxGraphNodes`, `MaxGraphEdges`, `InvalidGraphProps`). Existing documents
without `Graph` components validate byte-identically to before.

Renderers mirror these caps: ids (nodes, edge endpoints, focus, reveal)
compare exactly like the Rust validator with no trim; JS length caps count
Unicode code points; over-long node ids are dropped.

### Renderer contract

- **layered** — deterministic topological tiers (Kahn); roots form the first
  tier, and cycle members share one final tier so cyclic graphs render
  without deadlocking. Web draws an SVG canvas; iOS renders tier rows of
  chips; Android renders tier `FlowRow`s.
- **radial** — one ring in declaration order. Web uses an SVG ring, iOS a
  positioned ZStack with edge paths, Android a `Canvas` ring with offset
  chips.
- **list** — vertical stack, each node carrying a bounded adjacency summary
  (`→ target labels`).
- **select/expand** — tapping a node selects it and reveals a detail panel
  (label, kind, metadata, in/out edge counts). Web also emits the standard
  MUIJ `action` interaction (`action: 'select'`, `rowId`); mobile stays
  read-only per the published-surface contract.
- **focus** — `focus_node_id` becomes the initial selection everywhere.
- **reveal** — `reveal_order` drives a staggered fade-in on web (CSS
  `animation-delay`, disabled under `prefers-reduced-motion`); iOS and
  Android render statically, which the wire contract permits.
- Mobile and web normalize defensively and mirror the Rust caps
  (skip malformed members, drop dangling edges, cap admission at 200/400),
  so a hostile document degrades to a smaller graph instead of failing the
  whole briefing.

### Old-client degradation

An old client that has never heard of `Graph` keeps its existing
unknown-node behavior unchanged: web renders the dashed unknown-type chip
(dev builds additionally surface the existing unsupported-type error), iOS
and Android render their dashed fallback with label and children. No client
upgrade is required to receive a document containing a `Graph` component.

### Boundary

`Graph` is a MUIJ-layer family only, not a compilable V1 app-view kind in
`apps/surface_compiler.rs` (see
`docs/components/magician/app-default-surfaces.md`).

## Done Criteria

A route is considered migrated when:

- It no longer builds ordinary hand-authored product UI as `MuijComponent[]`.
- It uses MUIJ only for embedded generated/published/live-agent regions.
- Route-specific MUIJ builder code has either been deleted or renamed as a generated-surface builder.
- Keyboard/focus behavior is native and covered by focused tests where stateful controls were changed.
