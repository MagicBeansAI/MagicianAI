# Crew Native Forms

The crew native renderer powers the `/crew/new` definition editor without the
generic MUIJ renderer. It accepts `CrewNativeComponent` objects and renders
cards, forms, action stacks, data views, and checkbox-based multi-select
surfaces.

## Definition Editor

`/crew/new` uses native form controls for canonical enum-like fields:

- Trust level is a select using the backend trust vocabulary:
  `local`, `reviewed`, `builtin`, and `untrusted`.
- Agent kind is a select using `Worker` and `Personal`.
- Delegation targets are selected through the same checkbox grid used for tool
  allow lists instead of comma-separated text.

The agent-kind explanation belongs beside the agent kind control as a field
hint, not as a separate summary card at the bottom of the page.

## Global Harness Control

The `/crew` landing page includes the company-loop runtime switch. It loads the
effective state from `GET /api/magician/v2/harness/runtime` independently of the
scoped roster request and writes changes through PUT on the same endpoint. The
surface distinguishes config, emergency environment override, and config-error
states and keeps the switch disabled while status or mutation work is pending.
Turning it off is global: the backend blocks new harness work, clears queued
starts, and requests cancellation for active harness cycles.

## Searchable Multi-Selects

Native `MultiSelect` components may set `searchable: true` and
`searchPlaceholder` in `props`. When enabled, the renderer shows a text search
box and filters options by label or value while preserving checked state.

The `/crew/new` page enables this for:

- Delegation allow list (`Filter agents`)
- Tools (`Filter tools`)
- Excluded tools (`Filter excluded tools`)

## Static Crew Detail Routes

The static route-owned portions of `/crew/[id]`, `/crew/[id]/rules`, and
`/crew/[id]/memory/[tier]` render through the native crew renderer as well.
Those pages use the native component surface for cards, tables, tabs, badges,
metrics, code blocks, and action buttons while keeping the live agent surface on
`/crew/[id]` as the explicit runtime GAUI boundary.

## Model pins (`AgentModelPinsPanel`)

The Overview tab of `/crew/[id]` (non-system agents) carries a **Models**
panel beside Effective tools. It shows whether the agent follows global
routing or pins models, and edits the pins: one select per `llm_routing`
lane (Decisions & planning — the catch-all, including agentic decisions —
Evaluation & safety checks, Input interpretation, Memory consolidation) plus
the coding-engine model (`coding_profile`). Options come from
`GET /llm/routing` (concrete router profiles only; adaptive composites cannot
be pinned) and `GET /coding/profiles` (without `auto`). "Default" clears a
lane. Save sends one JSON Merge Patch (`PATCH /agents/{id}` with the record
etag as `If-Match`) carrying only what changed: newly set lanes as profile
endpoints, cleared lanes as `null`, and `{"llm_routing": null}` when nothing
stays pinned, so an unpinned agent carries no routing block. Sending only the
diff is what keeps pins the panel cannot edit: a lane pinned by direct
`provider`/`model` (no profile) shows as "… (direct model pin)" and can be
kept or cleared, and per-operation pins (`llm_routing.operations`) are shown
read-only; neither is rewritten by an unrelated save. A pin to a profile the
config lacks is refused server-side and shown as "Not saved". The save
reaches runs at once — the store writes the definition atomically, and the
handler refreshes the runtime's in-memory copy that dispatch reads — and it
survives a restart, because templates only materialize into a scope when the
agent's file is missing. A run already paused keeps the routing it started
with. After a save the page's record (Config version) is replaced from the
`saved` event. Logic lives in `agentModelPins.ts` (unit-tested, including
the direct-pin round trip); the panel test covers load → pin → save and
clear-all.
