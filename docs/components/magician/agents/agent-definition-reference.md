# Agent Definition Reference

Field-by-field contract for `AgentDefinition` in
`magician/src/magician_v2/agents/types.rs`, the parse and validate surface for
agent authors.

**File format:** YAML, `.agent.yaml`, parsed with `serde(deny_unknown_fields)`:
a typo or extra key fails parse, including the nonexistent keys
`capability_packs`, `excluded_packs`, `observation`, `goals` and `triggers`.
Scheduling lives on `Task.schedule`. Fields are written
**type · default · required**; `aliases`, `wake_spellings` and `disabled` are
not serialized when empty or false.

## Field inventory

Source order: `agent_id`, `version`, `name`, `aliases`, `wake_spellings`,
`description`, `app_tool`, `persona`, `kind`, `disabled`, `tools`,
`excluded_tools`, `denied_tools`, `browser_transports`, `denied_tool_params`,
`constraints`, `trust_level`, `memory_tiers`, `memory_consolidation`,
`prompt_pipeline`, `circuit_breaker`, `feedback_loops`, `notification_rules`,
`retention`, `llm_routing`, `strategy`, `state_machines`, `principal`,
`workspace`, `autonomous_config`, `social_persona`, `harness`, `is_primary`,
`onboarding_completed`, `readable_agents`, `default_personality`,
`user_memory_isolation`, `delegation_targets`, `invocation_policy`,
`auto_surface_policy`, `chat_inline`. Sections group them by concern; nested
types sit under their owner.

## 1. Identity & Classification

| Field | Type · default | Contract |
|-------|----------------|----------|
| `agent_id` | `String` · `""` | Identity, storage prefix, delegation routing key; required after load. Omitted on `POST /agents`, `create_agent_definition_with_store` derives it (`slugify_name`, max 64 chars, `"unnamed"` if empty) and disambiguates. `validate_agent_identifier()`: non-empty ASCII, <= 255 bytes, trimmed, not `.`/`..`, no `/` or `\`, not a reserved infrastructure name |
| `version` | `u32` · `1` | Monotonic, >= 1 |
| `name` | `String` · required | Non-empty display name |
| `aliases` | `Vec<String>` · `[]` | Case-insensitive chat handles (`@alias`, greeting, first word) besides `name` |
| `wake_spellings` | `Vec<String>` · `[]` | In-lexicon spellings that **arm the on-device wake spotter** for a coined name it cannot hear (`[magical, magician]` for "Magican"); arming and wake admission only (display, @-mention, open-vocabulary assistants keep `name` + aliases); non-empty replaces `name` + aliases for that recogniser, empty arms them |
| `description` | `String` · `""` | Free-text purpose |
| `persona` | `String` · required | Non-empty system-prompt identity (`definition.persona` source) |
| `kind` | `AgentKind` · `personal` | `personal` (user-facing) or `worker` (headless, delegated to). Workers need explicit non-empty `tools` (`"*"` alone rejected) and must not set `autonomous_config`, `harness`, `readable_agents`, `is_primary` |
| `disabled` | `bool` · `false` | Hidden from scheduling, manual triggers, inline chat delegation and runtime catalogs; cascades through `delegation_targets` only to private descendants unreachable from an enabled top-level or personal path; an explicitly disabled worker stays disabled |
| `is_primary` | `bool` · `false` | Exactly one per workspace. Personal only |
| `onboarding_completed` | `bool` · `false` | Legacy, kept so persisted definitions round-trip; no active UI reads it |
| `principal`, `workspace` | `Option<String>` · `null` | Both or neither. The principal pipelines run under (not the env-var default), for access control and audit; mutable and user-created agents persist under a concrete `(principal, workspace)` scope that isolates visibility |
| `default_personality` | `Option<String>` · `null` (seed uses `"witty"`) | Personality skill (`lookup_personality_mode`: workspace layer first, then extra skills dirs) that seeds the `personality_profile` tier on the first chat; `true_friend` maps to `true-friend`; a missing skill seeds nothing |
| `chat_inline` | `Option<ChatInlinePolicy>` · omitted (= `off`) | `off`, `auto` (no confirmation), `confirm`: chat may run the agent via `delegate_to_agent` as an ephemeral task-backed run (successful outputs preserved before the backing task is deleted). Product behaviour, not authorization (`invocation_policy` gates) |

> **Scope resolution.** Definitions live under
> `scopes/<principal>/<workspace>/agent_runtime/agents/<agent_id>/` in
> `MAGICIAN_ROOT_DIR`; a loader without the execution's scope fails with
> `Owner definition '<id>' not found` (see `resolve_merged_agent_tools_for_scope`).

### `app_tool`

`Option<AgentAppToolContract>` · omitted (not app-callable). Typed
`agent_as_tool` contract; `input` and `result` use the app workflow-input
object schema (max 256 fields / 64 KiB; byte ceilings in `1..=262144`).
Declaring it grants nothing: installation and task owners still seal
definition, prompt, tools, resources, data policy and digest.

```yaml
app_tool:
  input:  { type: object, fields: { request: { type: text, required: true } } }
  result: { type: object, fields: { answer: { type: markdown, required: true } } }
  max_input_bytes: 16384
  max_result_bytes: 32768
```

### `social_persona`

`Option<SocialPersonaConfig>` · `null`
([fleet-social-network](../fleet-social-network.md)). Omitted, the social
worker uses introversion `0.5`, `opted_out: false` and config `social.default_daily_tokens`;
present, roster reconciliation budgets from `daily_tokens`.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `introversion` | `f64` | `0.5` | Finite, in `[0, 1]` |
| `daily_tokens` | `u64` | `2000` | Daily LLM token ceiling, `<= 10000000` |
| `opted_out` | `bool` | `false` | Skip autonomous social invites and posts |

### Runtime definition cache

Definitions and template-materialization markers are cached per scope and
invalidated by Magician writes. The scheduler ticks every 5 s on the cache
(`SCHEDULER_SCOPES_PER_TICK = usize::MAX`,
`SCHEDULER_DISPATCH_BUDGET_PER_TICK = 8`) and requeues over-budget fires
(`requeue_due_cron_trigger`), never drops them. After out-of-band YAML edits run
`make refresh-agent-definitions`, which POSTs
`/api/magician/v2/agents/refresh-definitions` (clears cache and markers,
reloads all scopes;
`MAGICIAN_API` selects the backend, `MAGICIAN_BEARER_TOKEN` scopes it when
set), as does Crew's **Reload from disk**.

The runtime keeps its own copy of every definition, which background loops
(the memory consolidation sweep) read. A refresh, and each consolidation sweep
tick, reconciles that copy with the store
(`reconcile_runtime_definitions_with_store`): a definition whose file is gone is
evicted with the same local cleanup as a delete (scheduler entries, queued
dispatches, pause flag) and listed in the response's `definitions_evicted`
(`principal`, `workspace`, `agent_id`); a changed file replaces the runtime
copy. A cron fire whose definition is missing also unregisters that agent's
schedule.

## 2. Tools & Delegation

| Field | Type · default | Contract |
|-------|----------------|----------|
| `tools` | `Vec<String>` · `[]` (= all, except the tool-free public-envoy shape) | Non-empty, unique names; required for workers (`"*"` alone rejected) and for untrusted agents except the exact tool-free public surface (`surface_only`, `delegation: none`, no targets, `allowed_direct_surfaces` only untrusted, e.g. `public_envoy`). `resolved_tools()` = declared (or all) − `excluded_tools` − `denied_tools`, deduplicated, blanks removed |
| `excluded_tools` | `Vec<String>` · `[]` | Trims a broad grant |
| `denied_tools` | `Vec<String>` · `[]` | Always wins: removed from provider schemas, deferred discovery, runtime tools, structural controls, introspection and the dispatch registry, and refused at dispatch even if a stale turn supplies it. Denying `orchestrator` removes spawn, delegate, handover and orchestration |
| `denied_tool_params` | `HashMap<tool, HashMap<param, Vec<prefix>>>` · `{}` | A resolved value starting with a denied prefix (case-insensitive) rejects the action for any implementation (composite, command, compiled, shell); keys whitespace-canonicalized (duplicate padded keys union); unified content requests checked per branch |

```yaml
denied_tool_params:
  gmail: { command: ["users messages trash", "users messages delete", "users messages batchDelete"] }
  shell: { command: ["rm -rf"] }
```

### `browser_transports`

`Vec<String>` · `[]` = **all three** of `cdp`, `headed`, `headless` for a
trusted agent: an **opt-in ceiling**, not default-deny. The split is identity, not window visibility: `cdp` attaches to the
**owner's own signed-in Chrome** (cookies, sessions, accounts) through the magicutor proxy; `headed` and
`headless` launch `agent-browser`'s own Chrome on a per-work-context profile
with no owner identity. An untrusted agent never keeps `cdp` (empty or
`cdp`-only becomes `headless`, `headed`; a YAML `cdp` line does not restore it).

- **Authorization, not request.** `connection_mode`,
  `MAGICIAN_AGENT_BROWSER_MODE` and the default (`cdp`) only request. Outside
  the ceiling the call refuses
  (`NOT BROWSED — this agent may only use the browser transports […]`), never
  substitutes; unknown names refuse; all-blank counts as undeclared.
- **Everywhere enforced.** Compiled launchers (`screenshot_preview`,
  `capture_reference`) refuse unless the definition scoped by runtime-owned `__agent_id` /
  `__principal` / `__workspace` exists and allows headless.
  Sessions key on id plus declared ceiling, not resolved transport
  (`<id>--headed-headless`, no suffix when undeclared); `dispatch_browser_primitive` re-applies it; authenticated
  retrieval (`content_read` / `BrowserContentReader` /
  `refuse_identity_bearing_retrieval`) and `authenticated_*` handoffs obey it.
  `execute_with_runtime` (CLI/evaluator) is ungated. App browser grants and
  meeting media capture are separate authorities.
- **Delegation.** Survives delegation; replaced, never merged, at each owner
  transition. Carrier confinement
  ([`outward-actions.md`](../outward-actions.md)) separately refuses `cdp`
  under an engagement or program.

Shipped declarations (`magician_data_v3/system/agent_templates/agents/`):

| Agent | Ceiling | Why |
|-------|---------|-----|
| `web-researcher`, `web-researcher-opc` | `[headless, headed]` | Delegation target of outsider-steerable agents |
| `presentation-maker` | `[headless, headed]` | Declared (no rationale) |
| `cpo`, `cro` | `[headless, headed]` | Officers; must not act as the owner |
| `envoy` | `[headless, headed]` | `untrusted`; `cdp` is dropped regardless |
| `personal-assistant`, `executive-assistant` | *(none)* | The owner's own agents |

### `delegation_targets`

`Vec<String>` · `[]`; entries non-empty. **Omitting it grants nothing** (no `delegate_to_agent`
or `handover_to_agent`). Literal `"*"` expands to enabled non-system agents
with `invocation_policy.delegation` `wildcard`; a test forbids it in shipped
agents. Delegation and handover sets resolve separately (disabled, scope,
hierarchy, `invocation_policy`, transition surface); membership is checked
before task creation and multi-target batches validate fully first. Wildcard
routing grants no cross-agent memory access (that needs a named relationship
or `readable_agents`).

### `invocation_policy`

`AgentInvocationPolicy` · `discoverability: ambient`, `delegation: wildcard`,
empty `allowed_direct_surfaces`. An authorization input.

```yaml
invocation_policy:
  discoverability: surface_only # ambient | explicit | surface_only
  delegation: none              # wildcard | explicit | none
  allowed_direct_surfaces: [thinking_map]
```

- `discoverability`: `ambient` is in discovery and composer/reference
  catalogs; `explicit` only by exact authorized route; `surface_only` only
  via its typed direct surfaces (at least one, unique).
- `delegation`: `wildcard` joins `delegation_targets: ["*"]`; `explicit` must be named; `none`
  blocks even a named caller. `delegation` and `handover` are enforced
  separately.
- `allowed_direct_surfaces` (`InvocationSurface`): `chat`, `realtime_voice`,
  `task`, `delegation`, `handover`, `thinking_map`, `tutor`, `app_copilot`,
  `contextual_assist`, `public_envoy`, `meeting`, `plane`. Empty = defaults
  (all except `thinking_map`, `meeting`, `plane`, `public_envoy`); non-empty =
  **exact** allowlist. `public_envoy` and `meeting` are `untrusted` audience,
  the rest `owner`. Seed `personal-assistant` restates every default plus `plane`.

A narrowed primary agent must keep `chat`, `task` and `contextual_assist` or
Magican keyboard Write/Ask/Act fail
([magican-keyboard](../../magios/magican-keyboard.md)). Feature-only agents
(Loom: `surface_only + delegation:none + thinking_map`) list every direct tool.
Native `delegate_to_agent` / `handover_to_agent` descriptions tell the model
to use them only when the caller lacks the needed tool.

## 3. Execution Constraints

### `constraints`

`AgentConstraints`. Must be > 0: `max_iterations`, `max_tokens_per_cycle`,
`max_consecutive_failures`, `approval_ttl_secs`, `delegation_timeout_secs`.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `max_iterations` | `u32` | `12000` | Cycles per goal pipeline |
| `max_tokens_per_cycle` | `u64` | `1000000000` | Development token budget per cycle |
| `max_consecutive_failures` | `usize` | `3` | Failures before abort |
| `allow_self_modification` | `bool` | `false` | May edit its own definition |
| `max_duration_secs` | `Option<u64>` | `null` → `GOAL_PIPELINE_TIMEOUT_SECS` (10800 s) | Wall-clock cap per goal pipeline |
| `approval_ttl_secs` | `u64` | `86400` | Global approval-grant TTL; rules may override |
| `requires_approval` | `Vec<ApprovalRule>` | `[]` | See below |
| `coordination` | `CoordinationConfig` | see below | Delegation limits |

### `constraints.requires_approval`

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `tool` | `String` | Yes | Trimmed; `"*"` or a declared tool |
| `action` | `String` or `[String]` | Yes | Non-empty, unpadded |
| `when` | `ApprovalCondition` | No | `param_matches: Map<String,[String]>` (`{}`), `url_contains: [String]` (`[]`); keys, patterns, entries non-empty |
| `ttl_secs` | `u64` | No | Per-rule TTL (> 0), e.g. `300` with `url_contains: ["bank.com"]` |

### `constraints.coordination`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `max_delegation_depth` | `u8` | `2` | Max transitive chain depth |
| `delegation_timeout_secs` | `u64` | `300` | Compatibility only (pause snapshots); > 0 |
| `allow_transitive_delegation` | `bool` | `false` | False: a target (`current_depth > 0`) cannot delegate on |

`delegation_timeout_secs` does **not** cap a delegated run; the soft
active-work budget (checked before new turns and dispatch; queueing, pause and
finalization excluded) is `depth: normal` / `deep` / `thorough` (900 / 2700 / 5400 s), which an explicit
`delegate_to_agent.timeout_secs` can only extend (normal tier when no depth is
named); with neither there is no delegation-specific budget. In-flight work
finishes.

## 4. Trust & Security

### `trust_level`

`TrustLevel` · `"local"` · `builtin`, `local`, `reviewed`, `untrusted`
(legacy `standard` → `local`), trimmed and matched case-insensitively against `TrustPolicy`
definitions. An unknown level is rejected at save, and on disk **refuses to
run** (`load_trust_dispatch_guard`) instead of denying everything.

- `builtin` / `local` / `reviewed` get the full universal pack set minus
  excluded and denied names; `untrusted` gets
  `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` and needs a non-empty `tools`
  (`tools: []` rejected) unless it is the exact tool-free public surface (see `tools`;
  `allowed_direct_surfaces: [public_envoy]`), which gets an empty
  provider/dispatch snapshot.
- Direct chat's compiled pack fast path is `builtin` / `local` only;
  `reviewed` / `untrusted` pack calls and approval-gated or outward calls go
  through the task-backed executor. Untrusted chat starts without spawn,
  delegate and handover.
- Trust denial precedes approval; resume cannot downgrade a protected lane
  (Thinking Map, Tutor) because pause state hashes the invocation context.
  `task_state_action` is an authorized capability. The V2 API and autonomous
  executor share one definition store. Flat-loop `deferred` tools are
  discoverable, not callable until their schema is advertised.

Evals: `make test-agent-tool-visibility-eval-harness`,
`make test-agent-tool-visibility-live-eval`.

### Security layers

| Layer | Mechanism | Guarantee |
|-------|-----------|-----------|
| Tool access | `tools` / `excluded_tools` | Narrow every projection and dispatch |
| | `denied_tools` | Hard denial before side effects |
| | `denied_tool_params` | Prefix-match resolved values |
| | Scoped `CapabilityRegistry` | Each agent sees a filtered registry |
| Shell injection | `ImplementationType::Command` | Most packs: `Command::new(program).arg()`, no shell, so metacharacters stay literal. The `shell` compiled provider uses `sh -c` under runtime-core's `ShellSandboxConfig` (blocked_command_fragments, allowed_working_dirs, allowed_binaries); grant `shell` only for arbitrary command execution, else deny `shell` / `delegation_shell` |
| Prompt injection | Boundary tags | External content (emails, web pages, tool output, user-editable personas) wrapped in `<external_content>`, `<tool_output>`, `<task_context>`, `<agent_identity>`; `neutralize_boundary_tags()` case-insensitively defuses closing tags; prompts treat it as data |

## 5. Memory

### `memory_tiers`

`Vec<MemoryTierDefinition>` · `[]`. Persistent, scoped, schema-aware data.

**Auto-defaults.** A `kind: personal` agent with empty `memory_tiers` and
`memory_consolidation` gets 6 tiers from `apply_defaults()` (`entities`,
`insights`, `recent_activity`, `archive`, `task_progress`,
`environment_knowledge`) and 9 rules, with or without `autonomous_config`.
Workers never get defaults and declare their own; canonical baseline
(`simple-data-analyst`, `dashboard-builder`): `episode`, `semantic`, `personality_profile` and batch rule
`consolidate_to_semantic` (`min_episodes: 15`, `max_staleness_hours: 24`,
`episodes(unprocessed=true)` → `semantic`), plus `environment_knowledge` /
`extract_environment_knowledge` when they drive environments
([`ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md`](../execution/ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md)).

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `name` | `String` | Yes | Unique, trimmed, no dots |
| `scope` | `TierScope` | Yes | `agent`, `agent_goal`, `user` |
| `description` | `String` | Yes | Non-empty |
| `schema` | `Map<String, TierFieldSchema>` | No | Non-empty keys; `text`, `document`, `date_time`, `key_value_list`, `collection` (`max_items`, `item_schema`) |
| `render` | `RenderConfig` | Yes | Non-empty `format`, `template` |
| `retention` | `RetentionMode` | Yes | `goal_lifetime`, `forever`, `!days <n>` (n > 0; legacy `retention: 14` accepted, re-serialized as `!days`) |

```yaml
memory_tiers:
  - name: "entities"
    scope: agent
    description: "Extracted entities with recency tracking"
    schema: { entities: { type: collection, max_items: 1000 } }
    render: { format: compact_summary, template: "{entities}" }
    retention: forever
```

### `memory_consolidation`

`Vec<MemoryConsolidationRule>` · `[]`. Auto-default rules:
`extract_entities`, `extract_insights`, `summarize_recent_activity`,
`update_task_progress`, `distill_insights`, `archive_old_episodes`,
`expire_old_episodes`, `extract_environment_knowledge`, `promote_to_user`
(entities max 1,000; `summarize_recent_activity` and
`extract_environment_knowledge` batch at 10 unprocessed episodes or 24 h
staleness).

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `String` | Unique |
| `trigger` | `ConsolidationTrigger` | `cycle_completed` (compact structured work only), `step_completed` (each successful step), `retention_expiry` (at retention cleanup), or `batch` with `interval_hours`, `interval_days`, `min_episodes` (episode sources), `max_staleness_hours` (one or more, > 0) |
| `source` | `String` | `episodes(goal_id, limit=N, unprocessed=true/false)` or `tiers(a, b)` |
| `target` | `String` | Tier (root declared in `memory_tiers`), `report:<channel>`, or `user.<path>` |
| `transform` | `ConsolidationTransform` | `structured` + `builtin` (`map_episode_to_task`, `append_strategy_record`, `append_archive_summary`, `promote_shared_insights`); `llm` + `prompt`, `operation?`, `system_prompt?`, `merge?` (`upsert_by_name`, `upsert_by_name_per_source`, `upsert_by_similarity`, `append_period`); `render` + `template` |

LLM `operation`: `memory_entity_extraction`,
`memory_environment_knowledge_extraction`, `memory_insight_distillation`,
`memory_user_promotion`, `memory_archive_summary`.

- **Fail closed.** Invalid output or unknown fields leave the tier and cursor
  unchanged. Lossless aliases normalize before paid repair and on merge (`confidence_level`
  → `confidence`, `source_urls` → `sources`, `account_used` → `account`,
  `best_settings`/`effective_settings` → `typical_settings`, research facts →
  `finding`, scalar `key_value_list` → `{summary: ...}`).
- **User fan-out.** `user.<tier>` routes items to `user.<target_tier>`
  (`preferences`, `skills`, `contacts`, `workflows`, `identity`,
  `organization`, `accounts`, `channels`); `apply_user_target` stamps
  RFC3339 `updated_at` on every object item.
- **Cost guards.** Empty batch sources skip the LLM
  (`skipped_empty_source`; not `StepResult`). Failures fingerprinted in
  `memory_consolidation_runs.json`: provider/parse/deadline/schema failures
  back off exponentially (15 min, doubling, capped at 24 h;
  `BATCH_LLM_RETRY_BASE_MINUTES` / `BATCH_LLM_RETRY_MAX_HOURS`), configuration
  failures quarantine. A changed
  rule or semantic source clears the guard; timestamp-only touches do not.
- **Caps.** Collections cap at `max_items` on ingest and after upsert;
  overflow goes to a declared sibling `<source_tier>_archive` (same field and
  `item_schema`), else is dropped; the archive is terminal
  (`archive_overflow_items`).
  Canonical `archive_old_episodes` (`memory_archive_summary`, `archive`,
  `append_period`, `episodes(unprocessed=true)`) commits up to six episodes per checkpoint and advances
  only on full commit; `source_episode_ids` enable crash-safe replay and
  retention skips checkpointed episodes.
- **Template ↔ scope sync.** Changes to `memory_tiers`, `memory_consolidation`
  or `prompt_pipeline` go in both
  `magician_data_v3/system/agent_templates/agents/<id>/definition.agent.yaml`
  and `scopes/<principal>/<workspace>/agent_runtime/agents/<id>/definition.agent.yaml`;
  the runtime reads the scoped copy.

Validation: `report:` needs a channel, `user.` a sub-path; a
`min_episodes`-only batch needs `unprocessed=true`; `agent_goal` tiers stay out
of `batch` and `retention_expiry`; no cross-agent tier sources; prompts and
templates non-empty.

```yaml
memory_consolidation:
  - name: "extract_entities"
    trigger: step_completed
    source: "episodes(unprocessed=true)"
    target: "entities"
    transform: { type: llm, operation: memory_entity_extraction, prompt: "Extract named entities.", merge: upsert_by_name }
```

### `readable_agents` and `user_memory_isolation`

`readable_agents` (`Vec<String>`, `[]`, personal only): agents whose memory
this one may read. `user_memory_isolation` (`UserMemoryIsolation`): `shared`
(default) or `fully_isolated` (cannot read the user's personal tiers).

## 6. Prompt Pipeline

### `prompt_pipeline`

`Option<PromptPipelineConfig>` · `null`.

`prompt_pipeline.sections` (`PromptSection`, ordered): unique `name`; exactly
one of `source` / `content`; `required` (`false`); `condition`, `format`,
`filter` non-empty if set (`filter` not on tier or inline sources).

Sources: `definition.persona`; `definition.auto_surface_guidance` (injected as
"Dashboardable Output" when `auto_surface_policy.enabled` and undeclared);
`memory.user_profile`, `memory.user_knowledge`, `memory.corrections`;
`memory.episodes(limit=10)` (no `unprocessed` selector); `memory.tier[<name>]`, `tiers(a, b)` (own tiers);
`derived.failure_analysis`, `derived.success_patterns`; `feedback.<name>`;
`strategy_context`. `definition.goals[...]` fails with
`goal definitions removed from agents; use Task intent instead`.

`prompt_pipeline.output_rules`: `max_context_tokens` (`u32`, `4000`, > 0);
`truncation_priority` (`[String]`, `[]`: unique section names truncated first
to last).

```yaml
prompt_pipeline:
  sections:
    - { name: "persona", source: "definition.persona", required: true }
    - { name: "failure_context", source: "feedback.failure_context", condition: "has_recent_failures" }
  output_rules: { max_context_tokens: 8000, truncation_priority: ["failure_context"] }
```

## 7. LLM Routing

### `llm_routing`

`Option<LlmRoutingConfig>` · `null`. No shipped agent pins, so agents follow
`operation_mapping` in `llm-router.yaml` (on a harness engine, eligible
side-calls follow the harness). Edit in Crew **Models**
(`/crew/[id]` → Overview) or YAML.

- **Precedence.** A pin beats the owner's Settings override, the parent
  (harness) engine and the mapping. `planning` catches every operation not
  claimed by `evaluation`, `correction_extraction` or `memory_consolidation`,
  including `agentic_decision`; on a harness engine it covers side-calls only.
- **Write validation.** `PUT`/`PATCH /agents/{id}` refuse newly introduced
  unknown profiles (an untouched stale pin does not block an edit) (`422 unknown_pinned_profile`,
  `unknown_llm_routing_references`), checked against `llm.router.profiles`
  and, for `coding_profile`, `coding.profiles[].id`.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `planning` | `LlmEndpoint` | `null` | Catch-all lane |
| `evaluation` | `LlmEndpoint` | `null` | Tool evaluation, parameter/discovery safety |
| `correction_extraction` | `LlmEndpoint` | `null` | Agentic input interpretation |
| `memory_consolidation` | `LlmEndpoint` | `null` | Memory extraction/review |
| `operations` | `BTreeMap<String, LlmEndpoint>` | `{}` | Per `operation_mapping` name; beats lanes |
| `coding_profile` | `Option<String>` | `null` → `coding.default_profile` | `run_coding_task` default |

`LlmEndpoint`: `profile` (a `magician-config.yaml` id; overrides
provider/model) or `provider` + `model`.

```yaml
llm_routing:
  planning: { profile: "opus47-messages-toolsany-rnone" }
  evaluation: { provider: "openai", model: "gpt-5.6-terra" }
  operations: { agentic_decision: { profile: "opus47-messages-toolsany-rhigh" } }
  coding_profile: "coding-premium"
```

## 8. Resilience

### `circuit_breaker`

`Option<CircuitBreakerPolicy>` · `null`.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `thresholds` | `[CircuitBreakerThreshold]` | 1 → `inject_failure_context`; 3 → `open_circuit` notify `["chat"]` | Non-empty; strictly increasing `failures` |
| `recovery` | `CircuitRecovery` | `user_reset` | `trigger: user_reset` (no `cooldown_hours`), or `trigger: time_based` + `cooldown_hours` (> 0) |
| `per_goal_override` | `Map<goal, {max_failures}>` | `{}` | Valid goal ids; `max_failures` > 0 |
| `idle_failure_threshold` | `u32` | `3` | Idle-goal failures before suppressing |
| `idle_open_duration_minutes` | `u32` | `60` | Idle circuit open time |

`CircuitBreakerThreshold`: `failures` (`usize`, > 0), `action`
(`inject_failure_context` | `open_circuit`), optional `escalation` (non-empty),
optional `notify` (`chat`, `webhook`, `agent_memory`; non-empty, unique).

```yaml
circuit_breaker:
  thresholds:
    - { failures: 2, action: inject_failure_context }
    - { failures: 4, action: open_circuit, notify: ["chat"] }
  recovery: { trigger: time_based, cooldown_hours: 12 }
```

### `feedback_loops`

`Vec<FeedbackLoopDefinition>` · `[]`. Episode signals fed to the prompt.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `String` | Unique |
| `trigger` | `String` | e.g. `episode.outcome.is_failed` |
| `extract` | `FeedbackExtract` | `source` (required), `filter?`, `fields?` |
| `transform` | `String` | e.g. `failure_context` |
| `inject_into` | `String` | Existing section, e.g. `prompt_pipeline.failure_context` |

All strings non-empty; the pipeline and section must exist.
`FeedbackLoopDefinition::defaults()`: `failure_adaptation`,
`success_reinforcement`, `strategy_effectiveness`.

## 9. Notifications

### `notification_rules`

`Vec<NotificationRule>` · `[]`. Channels: `chat` (session subscription while a chat session is active),
`webhook` (needs `MAGICIAN_PROGRESS_WEBHOOK_URL` or `SLACK_WEBHOOK_URL`),
`agent_memory` (`{ agent_id, tier_name: "episode" }`); unregistered ids are
ignored during reconciliation. `circuit_breaker.thresholds[*].notify` already
sends `agent.circuit.opened`; add a rule only for a custom message or severity.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `match` | `String` | Yes | Event pattern, e.g. `agent.cycle.failed` |
| `severity` | `NotificationSeverity` | Yes | `low`, `medium`, `high` |
| `channels` | `[String]` | Yes | At least one supported id; no empties or duplicates |
| `condition` | `String` | No | Gates delivery |
| `message` | `String` | No | Custom message |

## 10. Data Lifecycle

### `retention`

`Option<RetentionPolicy>` · `null`. Day counts > 0.

#### `retention.episodes`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `default_days` | `u32` | `90` | Episode retention |
| `on_failure` | `u32` | `null` | Failed-episode override |
| `per_goal_override` | `Map<String, u32>` | `{}` | Per-goal days |
| `consolidate_before_delete` | `bool` | `false` | Consolidate first |

Personal memory defaults ([Memory](#5-memory)) set `default_days: 7` and
`consolidate_before_delete: true`; deletion then fails closed unless every
`retention_expiry` rule succeeds (else episodes wait for a retry).
Episodes claimed by a durable archive checkpoint are kept.

#### `retention.corrections`

`resolved_days` (`u32`, `30`); `active` (`u32` or `"forever"`, default
`"forever"`).

#### `retention.definition_versions`

`keep_last` (`u32`, `10`).

## 11. Strategy & publication

### `strategy`

`Option<StrategyPreference>` · `null` (runtime `fixed: "atomic_composition"`):
`fixed` (non-empty), `ordered: ["guided_search", "atomic_composition"]`
(non-empty, unique, falls back in order on failure), or `auto_select` (from effectiveness data).

### `state_machines`

`HashMap<String, serde_json::Value>` · `{}` (keys non-empty), run by the declarative
`StateMachineInterpreter` that also drives ApprovalService.

```yaml
state_machines:
  login_flow:
    initial: "check_session"
    states:
      check_session: { transitions: { logged_in: "ready", not_logged_in: "authenticate" } }
```

### `auto_surface_policy`

`Option<AutoSurfacePolicy>` · `null`. Publishes artifacts with `RenderHints`
as a surface at `route`.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `enabled` | `bool` | Yes | On/off |
| `route` | `String` | Yes | e.g. `/briefing` |
| `title_template` | `String` | Yes | `{agent_name}`, `{goal}`, `{task_title}` |
| `materialize_as` | `Option<String>` | No | e.g. `muij_surface` |
| `surface_kind` | `Option<String>` | No | e.g. `dashboard` |
| `placement_kind` | `Option<String>` | No | `task`, `workspace`, `thread`, `global` |
| `pinned` | `Option<bool>` | No | Pin override |

## 12. Autonomy

### `autonomous_config`

`Option<AutonomousConfig>` · `null`. Personal only (memory defaults apply to
every personal agent, not just autonomous ones).

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `schedule` | `String` | required | Cron, >= 5 fields |
| `focus_areas` | `[FocusArea]` | required, non-empty | Cycle focus areas |
| `max_tasks_per_cycle` | `u32` | `3` | Tasks per cycle |
| `max_steps_per_plan` | `u32` | `10` | Steps per plan |

`FocusArea`: `name` (unique slug), `description`, `priority` (`low` |
`medium`/`med` | `high`, default `medium`), optional `schedule`, `program`
and `scope` overrides (non-empty when set; `["*"]` = resolved harness scope,
not the whole workspace). Each becomes goal
`harness:<agent_id>:<focus-area-slug>`; a manual trigger may omit `goal_id`
only when there is one. Tasks inherit `principal`/`workspace` from parent or
request when the definition omits them; the scheduler requires explicit scoped ownership.

### `harness`

`Option<HarnessConfig>` · `null`; personal only. Harness tools are injected at
runtime, hidden from other agents even when `tools` exposes all, and run on
scoped stores in the owner's harness scope.

- Read (`HARNESS_READ_TOOL_NAMES`): `list_episodes`, `read_trace`,
  `list_agents`, `inspect_agent`, `system_status`, `evaluate_harness`,
  `read_program_state`, `list_proposals`, `magician_work_ledger`,
  `inspect_backlog_delivery`.
- Acting (`HARNESS_ACTION_TOOL_NAMES`): `create_task`, `reassign_task`,
  `create_agent`, `update_agent`, `retire_agent`, `update_delegation`,
  `create_proposal`, `create_dashboard`, `update_program_state`,
  `propose_backlog_item`, `promote_backlog_item`, `review_backlog_delivery`
  (`notify_owner` is a compiled handler).
- `create_task` / `create_agent` are spend-gated; tasks are Internal unless
  `user_visible: true`. Structural tools are proposal-backed, need evidence,
  are revalidated against scoped trust policy and cannot leave the
  `principal`/`workspace` via `yaml_after`; `delegation_targets` and
  `readable_agents` stay explicit and in scope (`*` rejected). `retire_agent` is
  a scoped revision plus pause (reported `paused`, not `idle`).

`harness.program_section` (`Option<String>`, `null`, non-empty if set):
`program.md` section injected for focus areas without their own `program` (a
focus-area `program` is injected as-is). Program docs load from `programs/`
(locally) or `Programs/` (older notes-folder layouts); a `Success Metrics` section takes
bullets like `harness_success_rate_30d >= 0.7`.

## Global Validation Rules

1. Strings cap at 32 KiB (`MAX_STRING_FIELD_BYTES`).
2. `deny_unknown_fields` on `AgentDefinition` and most nested structs: unknown
   keys fail at deserialization, not validation.
3. Workers: explicit `tools`, not `"*"` alone; no `autonomous_config`,
   `harness`, `is_primary`, `readable_agents`.
4. `principal` and `workspace` together or not at all.
5. `autonomous_config`: one or more focus areas, 5-field cron.

## Part 2: Use Case Recipes

- **Scheduled web automation:** personal; `tools: ["browser", "files"]`,
  `browser_transports: [headed, headless]`, `max_consecutive_failures: 2`,
  the [`circuit_breaker`](#circuit_breaker) example, a `chat` rule for
  `agent.cycle.failed`.
- **Research worker:** `kind: worker`,
  `tools: ["websearch", "htmltotext", "http", "files"]`,
  `browser_transports: [headless, headed]`, `chat_inline: auto`,
  `trust_level: "local"`, own memory.
- **Coordinator:** `is_primary: true`;
  `delegation_targets: ["web-researcher", "simple-data-analyst"]` and the
  same `readable_agents`;
  `coordination: { max_delegation_depth: 2, allow_transitive_delegation: true }`.
- **Scheduler boundary:** the scheduler is an internal engine and wake-queue
  path, not an agent: not under `agent_runtime/agents/`, no tools, not a
  delegation target.

## Runtime note: dedicated background runtime

Supervisor sweeps (scheduler, approval expiry, memory consolidation,
WakeUpQueue, task scheduler, harness steward/autofix) use a dedicated
`magician-bg` tokio runtime from `spawn_agent_supervisor_tasks`
(`magician-api/src/web_api.rs`), stopped with
`Runtime::shutdown_background()` after join handles complete. Agent **runs**
use the main execution runtime via `spawn_execution_job`; only sweeps are
isolated.

**Shell sandbox.** With `execution.shell_sandbox.os_enforce` the shell runs in
an OS box (writes only under working-dir roots, temp and tool caches; secret
stores unreadable; nested agent CLIs blocked); network follows
`execution.network_policy`. See [god-mode.md](../god-mode.md) and
the design.
