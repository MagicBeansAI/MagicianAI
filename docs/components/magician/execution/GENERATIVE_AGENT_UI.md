# Generative Agent UI (GAUI)

Agents project execution and memory into a trusted component library over
MUIJ (Magician UI JSON). GAUI Live is the delta/snapshot runtime around
that document. Hand-authored product pages are native Svelte unless they
need a generated, published, replayable, or live-agent surface — see
[MUIJ / native boundaries](../../unified-ui/muij-native-ui-boundaries.md).
`/crew/[id]` mounts the production `muijStore` consumer via
`LiveAgentSurface.svelte`.

Not built: dynamic composition ops (`AddComponent` / `RemoveComponent` /
`ClearAll`) and the external-website overlay / Chrome side panel (GAUI-ε).
History: GAUI_PHASE_BOARD.md,
GAUI_IMPLEMENTATION_PLAN.md.

---

## 1. Role

Agents return interactive layouts, not only text. The LLM composes from
`DefaultComponentRegistry`; the frontend never evaluates agent-supplied
JavaScript. Layout is stored separately from agent behavior.

---

## 2. Layout storage

MUIJ layout is scoped runtime state, not part of `AgentDefinition`
(`deny_unknown_fields`):

```
magician_data_v3/scopes/<principal>/<workspace>/agent_runtime/agents/{agent_id}/ui_layout.muij.json
```

Filename constant `MUIJ_FILENAME` in `magician/src/magician_v2/gaui/muij.rs`.
`MuijStorage` serializes writes/deletes with a mutex; reads are lock-free.
`agent_id` must pass `validate_agent_identifier`. Sharing an agent does not
carry its layout unless `ui_layout.muij.json` is copied with the scoped
runtime tree.

---

## 3. The Protocol: MUIJ

Flat adjacency-list documents (`muij_version: "1.0"`), inspired by A2UI.
Agents stream incremental deltas over the V2 WebSocket.

### 3.1 Component Registry

`DefaultComponentRegistry` (`gaui/muij.rs`) is the fail-closed admit list.
`component_type` is a `String` validated at runtime; unknown types fail
`MuijDocument::validate`. New types need review and a MUIJ minor bump.

- Layout: `Container`, `Stack`, `Grid`, `SplitPanel`, `Tabs`, `Panel`, `ScrollArea`, `Divider`
- Display: `Gauge`, `Card`, `Text`, `Table`, `DataList`, `MetricCard`, `Progress`, `Badge`, `Tag`, `EntityGrid`, `Tree`, `TreeNode`, `Graph`
- Charts: `PieChart`, `BarChart`, `LineChart`, `AreaChart`, `ScatterChart`, `TrendChart`, `Sparkline`, `Heatmap`
- Input: `ActionBus`, `Button`, `TextField`, `Select`, `Form`, `TextArea`, `NumberField`, `Slider`, `Checkbox`, `RadioGroup`, `MultiSelect`, `DatePicker`, `Toggle`, `SearchInput`
- Feedback: `Alert`, `Toast`, `Notification`, `ProgressBar`, `Spinner`, `Skeleton`, `EmptyState`, `ConfirmDialog`, `Tooltip`
- Media: `Image`, `Video`, `Audio`, `CodeBlock`, `DiffViewer`, `Markdown`, `QRCode`
- Collaboration: `CommentThread`, `ReactionBar`, `PresenceAvatars`, `ActivityFeed`, `ApprovalFlow`
- Live: `TerminalTransient`, `LiveSelectors`

Runtime-driven: `Gauge` (iteration HUD), `TerminalTransient` (streaming
log), `LiveSelectors` (DOM targets), `EntityGrid` (memory tiers),
`ActionBus` (buttons that trigger GoalCycles), `TrendChart`.

The renderer allow-list is `SUPPORTED_MUIJ_COMPONENT_TYPES` in
`ui/unified-ui/src/lib/magician/components/generative/componentCatalog.ts`;
a type only on that list still fails backend validation.

Caps: `children` depth ≤ 32 (`MAX_COMPONENT_DEPTH`); top-level layout ≤ 500
(`MAX_LAYOUT_COMPONENTS`, emitter too); `Graph` ≤ 200 nodes / 400 edges.

### 3.2 MUIJ v1 Schema Envelope

The kind field is `component_type` (not `type`).

```json
{
  "muij_version": "1.0",
  "agent_id": "agent-abc",
  "generated_at": "2026-01-01T00:00:00Z",
  "layout": [
    {
      "id": "revenue_chart",
      "component_type": "TrendChart",
      "label": "Revenue vs Cost",
      "source": "memory.tier[financials]",
      "query": "$.items[*].margin_pct",
      "props": {}
    }
  ]
}
```

Optional `static_snapshot` and `children`. `props` must be an object.
Validation rejects empty/duplicate ids, unknown types, unsupported
`muij_version`, invalid `agent_id`, over-depth or over-size layouts;
`Graph` props are fail-closed (`MuijGraphSpec`).

### 3.3 Delta Protocol

`agent.ui.delta` carries `MuijDelta` (`tag = "op"`, snake_case):

- `Upsert { component_id, data }` — create or replace
- `Remove { component_id }`
- `Reorder { ids }` — set render order

`component_type` is immutable after create; upserts that change it are
ignored.

On reconnect the client sends `agent.ui.snapshot_request { type, agent_id }`;
the server replies with the current `MuijDocument` (`MuijDocumentCache`,
disk fallback). Unknown JSON fields on snapshot/interaction frames are
rejected.

Emitter context is created on `AgenticExecutionStarted`; stale entries
expire 30 min after last activity (`STALE_ENTRY_SECS`), swept every 500
events or 5 minutes.

---

## 4. Component Library

The library is the §3.1 registry plus Svelte renderers under
`ui/unified-ui/src/lib/magician/components/generative/`.

---

## 5. Data Binding via serde_json_path (RFC 9535)

Binding uses `serde_json_path` (RFC 9535, read-only) via `MuijQueryEngine`
(`gaui/query.rs`). Derived arithmetic (e.g. `margin_pct`) is precomputed in
`TierData` during execution, never in the UI. Parsed paths are cached per
engine (512 entries, LRU half-evict); invalid queries return no matches.

JSONata is permanently rejected: `jsonata-rs` panics on unimplemented
features, CVE-2024-27307 prototype pollution, `$eval()` second-order
injection, unbounded lambda recursion; the WASM alternative cost +7 MB and
2–5 ms vs < 0.2 ms.

---

## 6. Real-time Interaction Model (AG-UI)

Bidirectional bus over the V2 WebSocket (`magician-api/src/websocket_handler.rs`):

1. **Agent → UI:** `agent.ui.delta` wrapped in `AgentEventEnvelope`.
2. **UI → Agent:** `ui.interaction { agent_id, component_id, goal_id?, trigger?, request_id? }`.
   ActionBus clicks go through `prepare_manual_trigger()`
   (`magician-api/src/web_api.rs`: trust, pause, active cycle, goal
   validation, execution mode, trigger sequence, TOCTOU). Rate limit 1
   trigger / 5 s per `(agent_id, component_id)`. Ack/error frames echo
   `request_id`.

Emitter event → delta (`gaui/emitter.rs`):

| Event | Effect |
|---|---|
| `AgenticExecutionStarted` | Bind execution→agent; Gauge `fill=0` |
| `AgenticIterationStarted` | Gauge fill; TerminalTransient boundary |
| `AgenticDecisionMade` / `AgenticActionExecuted` / `AgenticWaitingFor{User,Confirmation}` | TerminalTransient line |
| `AgenticExecutionCompleted` | Gauge `fill=1` |
| `AgentCycleStarted` / `AgentCycleCompleted` | Bind / cleanup cycle + seq |
| `agent.execution.mapping` | Recovery if start was lagged |

### 6.1 Fan-out Coalescing

`RuntimeTransportBroadcaster` is a `tokio::broadcast`. Before it, a
three-task pipeline coalesces per agent over a 200 ms window: event loop →
`MuijCoalescer` (merge coalescable `Upsert` by `component_id`, last write
wins) → broadcast (persist to `MuijStorage`, wrap, send).

Gauge upserts coalesce; TerminalTransient upserts do not (every line kept).
`Remove` drops any pending upsert for that id; `Reorder` flushes pending
upserts first.

---

## 7. Security & Isolation

- **Data-only.** The frontend never `eval`s agent strings.
- **Read-only queries.** `serde_json_path` has no write/eval/side effects.
- **Source taint.** Components read only the memory tiers named in `source`.
- **ActionBus admission** always via `prepare_manual_trigger()`; ActionBus
  `goal_id` must resolve against `definition.goals[]`.
- **WebSocket Origin.** Must be well-formed; accept localhost / `127.0.0.1`
  / `[::1]` or hostname equal to `Host`. Missing Origin allowed for
  non-browser clients; malformed rejected. Token auth still required for
  remote access.

---

## 8. Shadow Workspace Visibility

The emitter drives ordinary MUIJ components that track the loop.

### 8.1 Gauge (Iteration Progress HUD)

`fill = iteration / max_iterations`, clamped to 1.0, from
`AgenticExecutionStarted.max_iterations` (0 treated as 1) and
`AgenticIterationStarted.iteration`. Decision `confidence` is not used
(most emit sites hardcode 1.0).

### 8.2 TerminalTransient and LiveSelectors

TerminalTransient shows decision type, action summary, reasoning (capped
2048 chars) and iteration. LiveSelectors emit alongside (dual delta) with
action context and cycle isolation. There is no Magicutor SSE backchannel;
agent-browser / CDP-proxy runs start no Magician-side SSE listener.

---

## 10. Layout API

`magician-api/src/gaui_api.rs`:

- `GET /api/magician/v2/agents/{id}/layout` — materialized snapshot
- `PUT /api/magician/v2/agents/{id}/layout` — persist a validated document

Both need a resolved principal/workspace; missing agent → 404. PUT requires
`doc.agent_id` to match the path and registry validation (422 otherwise);
success updates `MuijDocumentCache`.

---

## 12. Architecture Rules

- MUIJ is data-only; no agent JS is evaluated.
- Component types are strings validated by `DefaultComponentRegistry::is_known_type`.
- Layout stored separately at `.../agent_runtime/agents/{agent_id}/ui_layout.muij.json`.
- All UI actions route through `prepare_manual_trigger()`.
- Read-only data binding: `serde_json_path` only.
- Deltas use the generic `AgentEventEnvelope`.
- `component_type` is immutable after create.

---

## 13. Implementation Plan

Module map:

| Piece | Path | Role |
|---|---|---|
| Types + storage | `magician/src/magician_v2/gaui/muij.rs` | `MuijDocument`, `MuijComponent`, `MuijDelta`, registry, file storage |
| Snapshot | `magician/src/magician_v2/gaui/snapshot.rs` | Shared REST + WS materialization |
| Query | `magician/src/magician_v2/gaui/query.rs` | RFC 9535 JSONPath |
| Coalesce | `magician/src/magician_v2/gaui/coalesce.rs` | 200 ms upsert merge |
| Emitter | `magician/src/magician_v2/gaui/emitter.rs` | Event → delta, cache, stale sweep |
| Layout API | `magician-api/src/gaui_api.rs` | GET/PUT layout |
| WS | `magician-api/src/websocket_handler.rs` | `agent.ui.snapshot_request`, `ui.interaction`, Origin |
| Admission | `magician-api/src/web_api.rs` | `prepare_manual_trigger()` |
| Store | `ui/unified-ui/src/lib/stores/muijStore.ts` | Apply deltas, snapshot on reconnect |
| Live surface | `ui/unified-ui/src/lib/magician/components/LiveAgentSurface.svelte` | `/crew/[id]` consumer |
| Renderers | `ui/unified-ui/src/lib/magician/components/generative/` | Component renderers + catalog |
