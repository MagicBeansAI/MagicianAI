# Model routing panel (Settings)

`/settings/model-routing` assigns a model and native thinking level to each
operation. The [catalog](../magicllm/model-catalog.md) owns provider connections,
capabilities, limits, thinking steps and workload defaults. Users do not manage
token-budget profiles. Auto resolves the model/workload default without adaptive
escalation; Off is available only when the catalog qualifies it.

## Operation routing

`ModelRoutingPanel.svelte` and `modelRoutingStore.ts` load `GET /llm/routing`.
The panel shows purpose, group, conditional mapping, effective model/provider
and routing source. Search and group filters use these server fields. The
adjacent catalog aggregates model usage across generated routes; Reload config
rereads external YAML and reports the number of **Resolved routes**.

`llm-router.yaml`'s `operation_mapping` is the registry. Adding a mapping creates
a row; adding a qualified model record creates its choices. Selectors may carry
`description` and `group`; missing metadata receives generated labels. Invalid,
control-bearing or oversized metadata is rejected during configuration loading.

`PUT /api/magician/v2/llm/routing/{operation}` accepts
`{"selection":{"model_id":"…","thinking":"auto"}}`. Auto resolves to a
concrete route. `DELETE` clears the override. The authenticated API writes
`system/llm_routing_overrides.json` (0600); it does not rewrite shipped YAML.
Unavailable choices are retained visibly until replaced or cleared.

Precedence is execution-local sealed route, install override, eligible parent
engine, then locality/request-shape-aware configuration. Ollama local defaults
never follow a remote parent. Each row reports `routing_source` and exposes
Parent / Pinned via `PUT …/{operation}/engine`; `DELETE` restores the config
engine rule. Pins live in `system/llm_routing_engine.json`. Engine affinity is
flow-scoped, and sealed execution routing outranks these settings.

`driving_engines` names active parent engines. `parent_profiles` contains their
generated secondary-operation routes. A harness's primary turn receives its
native model directly; secondary tool-free, non-local work uses its one-shot
MagicLLM provider. Codex App Server is a stateful execution engine whose secondary
calls bridge through the matching catalog harness connection.

## Shared selection controls

Chat, VibeDev, Settings and crew editors share model/thinking controls. Custom
Engine and Model dropdowns show compact single-line rows, product provider names,
keyboard navigation, search and readiness explanations. API IDs remain searchable
and appear in tooltips. Models whose `availability.state` is `unavailable` (the
source no longer offers them) are hidden by `offeredModels`, except an already
selected one, shown disabled as "no longer offered". See [composer controls](composer-dock.md) for docking,
theme, animation and popup positioning.

`ThinkingControl.svelte` uses a Bits UI stepped slider: filled track, tick marks,
rounded-square grip, tooltip and clickable labels. Arrows and Home/End select
advertised levels. Drag preview stays local and commits on release; Auto remains
separate. Reduced motion and larger touch hit areas are supported.

Chat persists `{model_id, thinking}`; coding also preserves engine identity and
distinguishes coordinator Auto from a model's Auto thinking. The frozen historical
asset converts old preferences once, then validates the exact choice against the
current catalog. Typed preferences win. Missing catalogs, malformed or unavailable
explicit choices block model-based sends until resolved; no profile-endpoint
fallback or runtime alias resolver remains.

Crew lane pins use `operation_choices` from `/llm/routing`; coding pins use
`/coding/models` and offer Follow global coding. Stored pins keep canonical
handles and workload authority. Empty catalogs never expose old profile menus.
Background Pi runs use `/chat/models?engine=pi` and typed `model_selection`;
clearing a pin explicitly restores the configured default. Desktop onboarding
reads coding-engine readiness independently of model descriptors.

Evals lifecycle comparisons use the same controls for up to three different
selectable operation routes; CLI engines are excluded. Unavailable comparisons
block Run until replaced/cleared. With no comparisons, configured routing applies
even without discovery. The evaluator's internal `profiles` list contains
generated handles. Quality additionally restricts choices to bounded, priced
routes and requires independent model families; see [Quality](../magician/relentless.md).
