# Install-level LLM routing overrides

UI: [the Settings panel](../unified-ui/model-routing-panel.md).

## Override store

`operation_llm_router` carries an install-level override store,
`system/llm_routing_overrides.json` (0600), loaded at boot by
`install_llm_routing_overrides(runtime_root)` beside the auth store. It is
consulted at BOTH profile-resolution sites BEFORE the config
`operation_mapping`.

- An explicit owner choice bypasses that operation's locality arms (the
  `when_cloud` arm's exemption, one level up) and flips exactly one operation;
  every sibling resolves byte-identically, including siblings that share the
  overridden profile as their own cloud arm.
- A stale override naming a removed profile is ignored, not fatal.
- `llm_routing_api` (magician-api) validates operations and profiles against
  the live config before writing. The shipped config file is never edited.
- Setters (`set_llm_routing_override`, `set_llm_routing_engine`) and their
  `clear_*` siblings hold the store's write guard across read, persist and
  replace. Why: two concurrent saves on different operations must both stick,
  and file and memory must agree. Persist happens before the swap, so a failed
  write leaves memory unchanged.

## Engine pins

The owner's per-operation `engine` choice (`parent` | `pinned`) lives in its
own file, `system/llm_routing_engine.json` (0600, JSON object keyed by
operation), loaded by the same `install_llm_routing_overrides`. Separate file
because the override loader resets to empty on a shape it does not recognise.

The config selector's `engine:` is the shipped default; the store wins over it.
`follows_parent_with(pin, selector)` is the one rule both resolution sites
(`follows_parent_for`) and the routing API read.
`PUT /llm/routing/{operation}/engine` `{"engine": "parent" | "pinned"}` writes
it; `DELETE …/engine` reverts to the config selector.

## Pinning onto dispatch

Resolving is not enough: the choice must be pinned onto the dispatch as
`extra.router_profile_override`, or magicllm's router re-resolves from its own
`operation_mapping.default_profile` and the decision is lost with no error.
Both dispatch paths pin through `resolved_profile_name_for_shape`:
`generate_via_router` (every `generate_for_operation*`, used by background
operations) and `dispatch_execution_native_messages` (the agentic Decide turn).
It returns `None` when the resolved name equals the operation's default, so
ordinary traffic stays unpinned.

Coverage boundary: the panel governs every path that resolves through
`operation_llm_router`. A path calling magicllm's router directly with an
operation name and no profile pin would ignore panel overrides; none exists,
and every dispatch must go through the pin.

## Precedence

1. A durable run's captured `OperationRoutingOverrides` — a Settings change
   cannot move an in-flight sealed execution.
2. Install-level panel override.
3. Engine affinity (below), subject to the engine pin.
4. Config `operation_mapping` with locality resolution.

## Engine affinity

Affinity is flow-scoped: a background operation follows the engine that
started its flow — the chat mouth for a chat turn, the run engine for an
agentic run, the connected CLI's family for an external MCP client — resolved
per request, never per process.

The parent travels two ways; explicit wins over ambient, and empty or
`magician` is no parent:

- explicitly as `OperationRoutingOverrides.parent_engine` where a flow threads
  its overrides (runs, plane grants);
- ambiently in the `query_analysis::parent_engine` task-local, scoped with
  `with_parent_engine` at the flow entry (chat turn, MCP call, run phase).

Selectors say `engine: parent` (default) or `engine: pinned` (stay on own
profile whatever drives). Two floors hold under any parent or setting:

- an operation whose configured default is a local (Ollama) profile never
  follows;
- a request carrying tool definitions (`RequestShape::has_tools`, set by the
  execution-native dispatch) never follows. Why: `op-harness-*` profiles are
  text-only, so Decide, the native chat mouth's tool turns and other tool-using
  operations keep their profile, and switching the chat mouth or run engine
  never stops autonomous runs.

`set_harness_affinity` (written by the chat/run switches) is display only —
resolution never reads it. The field is absent from the wire when unset, so
sealed execution routing and existing pause hashes are unchanged.

### Run parent

An agentic run names its parent via `plane::run_parent_engine`: the launch pin
(`ctx.harness_engine`), else a parent its context's overrides already carry
(e.g. a terminal grant's MCP client family), else the process snapshot.

- A launch request can never supply one: every starter (v3 execute, monitors
  run-now, v2 direct routes) drops a wire `parent_engine` in
  `admitted_launch_routing_overrides` before sealing, and
  `OperationRoutingOverrides::merge` never carries one from either side.
- The run's scoped router and decide adapter carry it explicitly
  (`routing_overrides_for_run`); the run seam and scheduler-lane hops carry it
  ambiently via `CapturedRunTaskLocals`.
- Spawned terminal work (output synthesis, evidence critic, reflection, memory
  consolidation) gets it from `load_execution_llm_routing_overrides`, which
  re-derives it from the durable launch pin (else process snapshot) and
  attaches it after the sidecar's seal check, so verified bytes stay as written.
- The synthesis pipeline never calls that loader: its
  `ExecutionFinalizeContext` / `TaskFinalizeContext` overrides come from the
  launch intent (parent stripped by design), so `finalize_routing_overrides`
  names the parent at all three construction sites. Every spawn must apply the
  rule, or synthesis/evidence operations silently ride the config profile.

## Models and harness profiles

The parent turn and secondary operations are distinct model axes. The chat/run
harness receives `chat.harness_model` or `execution.harness_model`; secondary
non-local operations use the active engine's `op-harness-*` profile and its
configured `model`. Aliases: `claude_code` → `op-harness-claude`,
`codex_app_server` → `op-harness-codex`. App Server is the stateful primary
plane/coding engine and is deliberately not a stateless MagicLLM provider.
`op-harness-pi` invokes the installed Pi CLI noninteractive and tool-free with
Pi's own credentials and default model unless the profile pins one; shipped
mappings do not select Pi by default.

## Routing display

`GET /llm/routing` per operation:

- `effective_profile` — the no-flow resolution (panel override, else the
  locality-resolved config profile; the parent is per flow, so nothing is
  "effective" at rest);
- `routing_source` — `override`, `parent` (follows and at least one driving
  engine is external with a resolvable `op-harness-*` profile), or `config`;
- `engine` (effective `parent` | `pinned`), `engine_source`
  (`config` | `override`), `follows_parent` (engine `parent` AND default not
  local), `local_floor`, `parent_profiles: {chat, run}`
  (`parent_profile_for_engine`);
- the serialized configured selector, locality-resolved configured profile,
  and description/group.

Top level: `affinity_scope: "flow"`, `driving_engines: {chat, run}` (normalised;
`magician` is `null`), `engine_pins`, the overrides, and the legacy `affinity` /
`affinity_profile` display cell for older readers. Descriptions and groups come
from the selector entry in `llm-router.yaml` `operation_mapping`, so adding a
mapped operation or profile needs no Settings code change.

## Deployment endpoint overrides

`MAGICIAN_CONTAINER_HOST`, `MAGICIAN_OLLAMA_BASE_URL` and
`MAGICIAN_MEMORY_OLLAMA_URL` retarget ordinary Ollama profiles.
`MAGICIAN_CONTAINER_HOST` also rewrites an app profile's reviewed loopback URL
to the desktop/container host and changes `loopback_managed` to
`trusted_self_hosted` — host Ollama stays usable without claiming an internal
DNS alias is loopback. Arbitrary generation/embedding overrides do not retarget
app-reviewed profiles, and an already-invalid remote URL is not repaired.
`make test-runtime-service-endpoints` covers these boundaries. See
[managed-container acceptance](../scripts/container-runtime.md#persistent-device-pairing-on-headless-linux).

## Apps profiles and shipped mappings

Apps profiles need explicit `context_window_tokens`, `max_output_tokens` and
`timeout_secs` for physical-call resource admission. Both shipped Apps profiles
use a 32,768-token context ceiling (a resource-accounting bound, not the
model's maximum); the remote profile routes reviewed `remote_allowed` work to
OpenAI `gpt-6-luna` with a 60-second physical timeout. A per-call timeout equal
to the whole run lifetime cannot pass admission once the run has started.
Queued Apps calls carry the router snapshot used for their disclosure
admission, including after reload.

Bounded, latency-sensitive and high-volume classification, extraction,
summarization and interactive interpretation operations use `gpt-6-luna`:
`memory_episode_quality_classification`,
`memory_environment_knowledge_extraction`, `memory_entity_extraction`,
`memory_temperature_utility_review`, `memory_attach_stage2`,
`answer_interpretation`, `query_analysis`, `brainstorm_facilitation`,
`thinking_map_interpret`, `screen_observation`, `meeting_summary`,
`meeting_response`, `kapso_envoy_chat_fallback`, `agentic_ledger_compaction`,
and the non-retry durable-task-state helpers. Durable-state retry routes,
autonomous planners, evidence judge, screen grounding and deep understanding,
archive compression and workflow compiler stay on Terra.

Keep the live router and the repo seed (`llm-router.yaml`) in sync. The
settings reload endpoint applies router-only changes without a restart.
