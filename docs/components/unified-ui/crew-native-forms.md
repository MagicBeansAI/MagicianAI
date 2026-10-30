# Crew Native Forms

The web Crew native renderer powers the `/work/crew/new` and
`/work/crew/[id]/edit` definition editor without the
generic MUIJ renderer. It accepts `CrewNativeComponent` objects and renders
cards, forms, action stacks, data views, and checkbox-based multi-select
surfaces.
The old `/crew` routes are compatibility redirects. This renderer is a Svelte
component; its name does not imply iOS/Android guided-form coverage.

## Definition Editor

The Work member editor uses native form controls for canonical enum-like fields:

- Trust level is a select using the backend trust vocabulary:
  `local`, `reviewed`, `builtin`, and `untrusted`.
- Agent kind is a select using `Worker` and `Personal`.
- Delegation targets are selected through the same checkbox grid used for tool
  allow lists instead of comma-separated text.

The agent-kind explanation belongs beside the agent kind control as a field
hint, not as a separate summary card at the bottom of the page.

## Global Harness Control

The Work Crew view includes the company-loop runtime switch. It loads the
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

The Work member editor enables this for:

- Delegation allow list (`Filter agents`)
- Tools (`Filter tools`)
- Excluded tools (`Filter excluded tools`)

## Static Crew Detail Routes

Work member details, rules and memory-tier inspectors render through the web
native Crew renderer as well. These components use its surface for cards, tables, tabs, badges,
metrics, code blocks, and action buttons while keeping the live agent surface on
member details as the explicit runtime GAUI boundary. Rules and tier inspectors
remain read-only; definition editing and user-memory merge own their write paths.

## Model pins (`AgentModelPinsPanel`)

The Work member Overview tab (non-system agents) carries a **Models**
panel beside Effective tools. It shows whether the agent follows global
routing or pins models, and edits the pins: one select per `llm_routing`
lane (Decisions & planning — the catch-all, including agentic decisions —
Evaluation & safety checks, Input interpretation, Memory consolidation) plus
the coding-engine model (`coding_profile`). Options come from
`GET /llm/routing` (canonical assignment/model handles) and
`GET /coding/models` (model choices projected without coordinator `auto`). "Default" clears a
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

## Shared documents per focus area (`FocusAreaSharedDocuments`)

When a Personal agent has autonomous mode on, the definition editor shows a
**Shared documents** card with one fieldset per focus area and a radio group:
**Use global list** (omits `shared_programs`, so the area follows
`harness.shared_program_docs`), **None** (saves `shared_programs: []`) and
**Choose documents** (saves the checked names; nothing checked saves `[]` and
says so). Choices are Playbooks from `GET /api/magician/v2/programs`,
shown by title and file name. A stored name missing from that list stays listed
as "not found" and can be unchecked. If the list fails to load, the card shows
the error with Retry and the rest of the form still saves. The read/write
mapping lives in `focusAreaSharedPrograms.ts` (unit-tested for all three
states); the component test covers loading, choosing and the failure path.
