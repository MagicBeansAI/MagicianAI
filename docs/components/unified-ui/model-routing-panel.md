# Model routing panel (Settings)

Server seam: `llm_routing_api` + the install-level override
store in `operation_llm_router` (see
[the harness provider section](../magicllm/multi-llm.md) and the
plan).

Settings links to the dedicated `/settings/model-routing` page. Its
`$lib/settings/ModelRoutingPanel.svelte` (fed by
`$lib/stores/modelRoutingStore.ts`) lists every operation present in the live
config with a purpose description, group, complete conditional mapping,
effective profile, provider, model, and routing source. Search covers all of
those fields and a group filter narrows the operation list. The profile catalog
remains beside it for profile-centric inspection.

There is no UI-owned operation list. `llm-router.yaml`'s
`operation_mapping` is the canonical registry: each key carries its selector
plus optional `description` and `group`. New profiles automatically enter the
catalog and dropdowns; new mapping keys automatically create rows. Legacy
string or metadata-free selectors remain valid and receive generated labels.
Router validation rejects empty, control-bearing, or oversized supplied
metadata before the configuration can serve calls.

Profiles are classified **local** (Ollama) / **api** / **harness** (CLI
subscription, with on-PATH install status), grouped in the per-operation
dropdown. Overridden rows highlight and carry **Use automatic routing**.
Switching one operation never touches another and takes effect immediately — the API
(`GET/PUT/DELETE /api/magician/v2/llm/routing{,/{operation}}`,
session-authenticated) writes an install-level override store
(`system/llm_routing_overrides.json`, 0600) consulted at the router's
resolution sites, and the shipped config file and its defaults are never
edited. **Reload config** is separate: it rereads external YAML changes into
the running backend and refreshes the page.

Adaptive composites appear in the roster for display (several operations
default to them) but render disabled in every dropdown — composites
cannot be override targets, and offering a choice that can never take
would be a lie (`selectable: false` end to end).

The page documents and renders the routing precedence: execution-local sealed
route, install-level operation override, the engine that started the flow for
non-local, tool-free operations unless the operation is pinned, then the
locality- and request-shape-aware config mapping. Each row identifies whether
its current profile came from config, an override, or the **parent engine**
(`routing_source: parent` — inside a flow it would ride the flow's engine).

The parent engine is flow-scoped: a background operation follows
the engine that starts its flow — the chat mouth, the run engine, or a
connected CLI — never a process value. The banner says so and, when either
chat or run is on an external engine, names the engines driving now
(`driving_engines` from `GET /llm/routing`, `magician` for the native loop).
Each row carries a **Parent / Pinned** segmented toggle (`aria-pressed`)
beside the profile select: Parent lets the operation follow, Pinned keeps it
on its own profile. Toggling writes `PUT /llm/routing/{operation}/engine`
`{"engine": ...}` into the install-level pin store (`store` wins over the
config selector's `engine:`), and a row whose rule came from Settings
(`engine_source: override`) offers **Use config engine rule** (`DELETE
…/engine`). A hint beside the toggle names the local floor ("local default —
never follows" for an Ollama default, which never follows whatever the
setting) or, when the operation follows and an external engine drives, the
profile it would ride per engine (`parent_profiles`). Execution-level sealed
routing still outranks Settings changes.

The selected chat/run harness model goes directly to the primary harness turn.
Secondary Magician operations use the active engine's one-shot MagicLLM profile
and that profile's own configured model. `codex_app_server` remains a stateful
plane/coding engine rather than a MagicLLM provider, so its secondary operations
bridge to `op-harness-codex`; its primary turn still uses App Server.
