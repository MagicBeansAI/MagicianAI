# Model routing panel (Settings)

Server seam: `llm_routing_api` + the install-level override store in
`operation_llm_router` (see
[the harness provider section](../magicllm/multi-llm.md) and the
plan).

Settings links to `/settings/model-routing`. Its
`$lib/settings/ModelRoutingPanel.svelte` (fed by
`$lib/stores/modelRoutingStore.ts`) lists every operation in the live config
with purpose description, group, full conditional mapping, effective profile,
provider, model and routing source. Search covers all of those fields; a group
filter narrows the list. The profile catalog sits beside it.

There is no UI-owned operation list. `llm-router.yaml`'s `operation_mapping` is
the canonical registry: each key carries its selector plus optional
`description` and `group`. New profiles enter the catalog and dropdowns, and
new mapping keys create rows, automatically. Legacy string or metadata-free
selectors stay valid and get generated labels. Router validation rejects empty,
control-bearing or oversized metadata before the config can serve calls.

Profiles are classified **local** (Ollama) / **api** / **harness** (CLI
subscription, with on-PATH install status) and grouped that way in each
operation's dropdown. Overridden rows highlight and offer **Use automatic
routing**. Switching one operation never touches another and takes effect at
once: the API (`GET/PUT/DELETE /api/magician/v2/llm/routing{,/{operation}}`,
session-authenticated) writes an install-level override store
(`system/llm_routing_overrides.json`, 0600) read at the router's resolution
sites; the shipped config file is never edited. **Reload config** separately
rereads external YAML changes and refreshes the page.

Adaptive composites appear in the roster for display but are disabled in every
dropdown, since they cannot be override targets (`selectable: false` end to
end).

The page shows the routing precedence: execution-local sealed route,
install-level operation override, the engine that started the flow (for
non-local, tool-free operations unless pinned), then the locality- and
request-shape-aware config mapping. Each row says whether its profile came from
config, an override, or the **parent engine** (`routing_source: parent`).

The parent engine is flow-scoped: a background operation follows the engine
that started its flow (the chat mouth, the run engine, or a connected CLI),
never a process value. When chat or run is on an external engine, the banner
names the engines driving now (`driving_engines` from `GET /llm/routing`,
`magician` for the native loop). Each row has a **Parent / Pinned** toggle
(`aria-pressed`) beside the profile select. Toggling writes
`PUT /llm/routing/{operation}/engine` `{"engine": ...}` into the install-level
pin store (`system/llm_routing_engine.json`; the pin wins over the config selector's `engine:`); a row whose rule
came from Settings (`engine_source: override`) offers **Use config engine rule**
(`DELETE …/engine`). A hint names the local floor ("local default — never
follows" for an Ollama default) or, when an external engine drives, the profile
the operation would ride per engine (`parent_profiles`). Execution-level sealed
routing still outranks Settings.

The selected chat/run harness model goes straight to the primary harness turn.
Secondary Magician operations use the active engine's one-shot MagicLLM profile
and its configured model. `codex_app_server` is a stateful plane/coding engine,
not a MagicLLM provider, so its secondary operations bridge to
`op-harness-codex` while its primary turn uses App Server.
