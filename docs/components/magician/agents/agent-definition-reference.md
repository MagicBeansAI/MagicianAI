# Agent Definition Reference

Field-by-field contract for `AgentDefinition` in
`magician/src/magician_v2/agents/types.rs`: the parse/validate surface for
agent authors.

**File format:** YAML, `.agent.yaml`. The struct uses
`serde(deny_unknown_fields)`, so a typo or extra key is a hard parse error.
Goals and triggers are not on this struct; scheduling lives on
`Task.schedule`. There is no `capability_packs`, `excluded_packs`,
`observation`, `goals`, or `triggers` field; those keys fail parse.

Each field below is written as **type · default · required**.

---

## Table of Contents

- [Field inventory](#field-inventory)
- [1. Identity & Classification](#1-identity--classification)
- [2. Tools & Delegation](#2-tools--delegation)
- [3. Execution Constraints](#3-execution-constraints)
- [4. Trust & Security](#4-trust--security)
- [5. Memory](#5-memory)
- [6. Prompt Pipeline](#6-prompt-pipeline)
- [7. LLM Routing](#7-llm-routing)
- [8. Resilience](#8-resilience)
- [9. Notifications](#9-notifications)
- [10. Data Lifecycle](#10-data-lifecycle)
- [11. Strategy & publication](#11-strategy--publication)
- [12. Autonomy](#12-autonomy)
- [Global Validation Rules](#global-validation-rules)
- [Part 2: Use Case Recipes](#part-2-use-case-recipes)

---

## Field inventory

Source order on `AgentDefinition`: `agent_id`, `version`, `name`,
`aliases`, `wake_spellings`, `description`, `app_tool`, `persona`,
`kind`, `disabled`, `tools`, `excluded_tools`, `denied_tools`,
`browser_transports`, `denied_tool_params`, `constraints`,
`trust_level`, `memory_tiers`, `memory_consolidation`,
`prompt_pipeline`, `circuit_breaker`, `feedback_loops`,
`notification_rules`, `retention`, `llm_routing`, `strategy`,
`state_machines`, `principal`, `workspace`, `autonomous_config`,
`social_persona`, `harness`, `is_primary`, `onboarding_completed`,
`readable_agents`, `default_personality`, `user_memory_isolation`,
`delegation_targets`, `invocation_policy`, `auto_surface_policy`,
`chat_inline`. Nested types are documented under the field that owns
them.

---

## 1. Identity & Classification

### `agent_id`

`String` · `""` · required after load; omit on `POST /agents` to
auto-generate.

Stable identity assigned once at creation; storage key prefix and
delegation routing key. On create (`create_agent_definition_with_store`)
an empty id is slugified from `name` (`slugify_name`, max 64 chars,
`"unnamed"` if empty) and disambiguated. Direct YAML load requires a
non-empty id.

Validation: non-empty, ASCII, <= 255 bytes, trimmed, not `.`/`..`, no `/`
or `\`, no reserved infrastructure names (`validate_agent_identifier()`).

### `version`

`u32` · `1`. Monotonic; increment on each update. Must be >= 1.

### `name`

`String` · required. Display name; non-empty.

### `aliases`

`Vec<String>` · `[]` (skipped when empty). Extra handles for chat
addressing (`@alias`, greeting by name, alias as first word).
Case-insensitive. `name` is also matchable; aliases add short forms
without changing the display name.

### `wake_spellings`

`Vec<String>` · `[]` (skipped when empty). In-lexicon spellings that
**arm the on-device wake spotter** for names the speech model cannot
recognise (the spotter drops out-of-vocabulary words, so a coined name
never fires). Arming and wake-admission only; display, @-mention, and
open-vocabulary assistants keep `name` + `aliases`. Non-empty replaces the
advertised names for the constrained recogniser; empty arms `name` +
aliases.

```yaml
wake_spellings: [magical, magician]   # "Magican" is not in the wake lexicon
```

### `description`

`String` · `""`. Free-text purpose.

### `app_tool`

`Option<AgentAppToolContract>` · omitted (not callable from an app).

Typed `agent_as_tool` child-task contract. `input` and `result` use the
closed object schema of app workflow inputs (max 256 fields / 64 KiB
schema; byte ceilings in `1..=262144`). Declaration is not authority:
installation/task owners still seal definition, prompt, tools, resources,
data policy, and digest.

```yaml
app_tool:
  input:  { type: object, fields: { request: { type: text, required: true } } }
  result: { type: object, fields: { answer: { type: markdown, required: true } } }
  max_input_bytes: 16384
  max_result_bytes: 32768
```

### `persona`

`String` · required, non-empty. System-prompt identity and behaviour,
injected via the `definition.persona` prompt source. Use a YAML block
scalar for multi-line.

### `kind`

`AgentKind` · `personal` · values `personal`, `worker`.

- **`personal`**: user-facing, conversational. May set
  `autonomous_config`, `harness`, `readable_agents`, `is_primary`.
- **`worker`**: headless, invoked by delegation. Must have explicit
  non-empty `tools` (`"*"` alone rejected). Must not set
  `autonomous_config`, `harness`, `is_primary`, or `readable_agents`.

### `disabled`

`bool` · `false` (skipped when false). Hides the agent from scheduling,
manual triggers, inline chat delegation, and runtime catalogs. Disablement
cascades through `delegation_targets` only to private descendants not
reachable from an enabled top-level or personal path; shared workers stay
usable through enabled roots. An explicitly disabled worker is always
disabled.

### `is_primary`

`bool` · `false`. The primary personal agent; exactly one per workspace.
Personal only.

### `onboarding_completed`

`bool` · `false`. Legacy marker kept for persisted-definition
compatibility; no active UI surface reads it.

### `principal`

`Option<String>` · `null`. Principal that pipelines spawned for this agent
run under (instead of the env-var default); used for access control and
audit. Must be paired with `workspace`.

### `workspace`

`Option<String>` · `null`. Workspace ownership. Mutable / user-created
agents persist under a concrete `(principal, workspace)` scope, which
isolates visibility. Must be paired with `principal`.

> **Scope resolution.** Definitions live under
> `scopes/<principal>/<workspace>/agent_runtime/agents/<agent_id>/` in
> `MAGICIAN_ROOT_DIR`, not the unscoped store. Loaders must pass the
> execution's scope or they miss the agent (`Owner definition '<id>'
> not found`). See `resolve_merged_agent_tools_for_scope`.

### `default_personality`

`Option<String>` · `null` (runtime seed falls back to `"witty"`).
Personality-mode skill name, resolved by `lookup_personality_mode`
(workspace layer first, then extra skills dirs). Seeds the
`personality_profile` memory tier on the first chat session. Underscore
names (`true_friend`) are bridged to kebab (`true-friend`). If the skill
is not installed the seed is a no-op: no tier rather than stale data.

### `social_persona`

`Option<SocialPersonaConfig>` · `null`. Fleet social-network posture. When
omitted the social worker uses introversion `0.5`, `opted_out: false`, and
config `social.default_daily_tokens`. When present, roster reconciliation
creates the agent's daily budget from `daily_tokens`. See
[fleet-social-network](../fleet-social-network.md).

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `introversion` | `f64` | `0.5` | Finite, in `[0, 1]` |
| `daily_tokens` | `u64` | `2000` | Daily LLM token ceiling, `<= 10000000` |
| `opted_out` | `bool` | `false` | Skip autonomous social invites and posts |

### `chat_inline`

`Option<ChatInlinePolicy>` · omitted (= `off`) · values `off`, `auto`,
`confirm`.

Whether chat may invoke the agent through `delegate_to_agent` as an
ephemeral task-backed run (successful outputs are preserved before the
backing task is deleted). `auto` runs without confirmation (cheap /
read-only workers); `confirm` asks the user first. Product behaviour, not
authorization: `invocation_policy` still gates discovery and dispatch.

#### Runtime definition cache

The backend caches each scope's parsed definition list and which scopes
have materialized built-in templates; writes through Magician invalidate
the affected scope. The scheduler ticks every 5 s against this cache
(`SCHEDULER_SCOPES_PER_TICK = usize::MAX`,
`SCHEDULER_DISPATCH_BUDGET_PER_TICK = 8`); due fires beyond the dispatch
budget are requeued (`requeue_due_cron_trigger`), never dropped.

Out-of-band YAML edits bypass that write path. Refresh without restart
with `make refresh-agent-definitions`, which POSTs
`/api/magician/v2/agents/refresh-definitions`, clears the cache and
template-materialization markers, and reloads all scopes. Override
`MAGICIAN_API` for another backend; the request is scoped by the bearer in
`MAGICIAN_BEARER_TOKEN` (sent when set). Crew's **Reload from disk** button
hits the same endpoint.

---

## 2. Tools & Delegation

### `tools`

`Vec<String>` · `[]` (= all tools, except the tool-free public-envoy
shape) · required non-empty for workers (`"*"` alone rejected).

`resolved_tools()` = declared list (or all registered tools when empty)
− `excluded_tools` − `denied_tools`, with duplicates and blank names
removed. Untrusted agents must declare a non-empty allowlist unless they
are the exact tool-free public surface (`surface_only`,
`delegation: none`, no delegation targets, `allowed_direct_surfaces`
only untrusted surfaces such as `public_envoy`). Names must be non-empty
and unique.

### `excluded_tools`

`Vec<String>` · `[]`. Removes tools from a broad grant without listing
every other tool. Applied after resolution (see `tools`).

### `denied_tools`

`Vec<String>` · `[]`. Structural denylist; always wins. Like
`excluded_tools`, it narrows the effective policy snapshot before
provider schemas, deferred discovery, runtime tools, structural controls,
introspection, and the dispatch registry are projected. It is also a hard
authorization boundary: a denied name cannot dispatch even if a stale
model turn or side channel supplies it. Denying `orchestrator` removes
the structural spawn/delegate/handover/orchestration controls.

### `denied_tool_params`

`HashMap<tool, HashMap<param, Vec<prefix>>>` · `{}`. Before dispatch, a
resolved parameter value that starts with a denied prefix
(case-insensitive) rejects the action. Matching is independent of
implementation type (composite, command, compiled, shell). Keys are
whitespace-canonicalized (duplicate padded keys union); unified content
requests are inspected branch by branch.

```yaml
denied_tool_params:
  gmail:
    command: ["users messages trash", "users messages delete", "users messages batchDelete"]
  shell:
    command: ["rm -rf"]
```

### `browser_transports`

`Vec<String>` · `[]` = **all three** of `cdp`, `headed`, `headless`.

An **opt-in ceiling**, not default-deny: an agent that declares nothing
keeps every transport. The distinction is identity, not window
visibility: `cdp` attaches to the **owner's own signed-in Chrome** through
the magicutor proxy (cookies, sessions, accounts); `headed` and `headless`
launch `agent-browser`'s own Chrome on a per-work-context profile with no
owner identity.

```yaml
browser_transports: [headless, headed]
```

- **Request vs authorization.** A tool call's `connection_mode` is a
  request; this key is the authorization, applied after the call's
  `connection_mode`, `MAGICIAN_AGENT_BROWSER_MODE`, and the runtime
  default (`cdp`). A transport outside the ceiling **refuses**
  (`NOT BROWSED — this agent may only use the browser transports […]`);
  it is never substituted. Unrecognised names refuse; a list of only
  blanks reads as "declared nothing".
- **Compiled launchers** (`screenshot_preview`, `capture_reference`) load
  the scoped definition from runtime-owned `__agent_id` / `__principal` /
  `__workspace` and refuse when it is missing or excludes headless.
  App-installation browser grants and meeting media capture are separate
  authority domains.
- **Session cache** is keyed by session id plus the declared ceiling
  (`<id>--headed-headless`, no suffix when undeclared), not by resolved
  transport. `dispatch_browser_primitive` re-applies the ceiling to the
  attached session. Authenticated retrieval (`content_read` /
  `BrowserContentReader` / `refuse_identity_bearing_retrieval`) and typed
  `authenticated_*` handoffs obey the same ceiling. `execute_with_runtime`
  is ungated (CLI/evaluator, no agent).
- **Delegation.** The ceiling is a fact about the agent: it survives
  delegation and is replaced (never merged) at every owner transition.
  Complements carrier confinement in
  [`outward-actions.md`](../outward-actions.md), which refuses `cdp` under
  an engagement or program regardless of this key.

Shipped declarations (`magician_data_v3/system/agent_templates/agents/`):

| Agent | Ceiling | Why |
|-------|---------|-----|
| `web-researcher`, `web-researcher-opc` | `[headless, headed]` | Delegation target of agents an outsider can steer |
| `presentation-maker` | `[headless, headed]` | Declared in its template (no inline rationale) |
| `cpo`, `cro` | `[headless, headed]` | Officers doing company work; must not act as the owner |
| `personal-assistant`, `executive-assistant` | *(none)* | The owner's own agents |

### `delegation_targets`

`Vec<String>` · `[]` (no targets). **Omitting the field grants nothing**:
with an empty list the catalog does not offer `delegate_to_agent` /
`handover_to_agent` at all. A literal `"*"` must be written out and
expands to every enabled non-system agent whose own
`invocation_policy.delegation` is `wildcard`; no shipped agent may carry
`"*"` (enforced by test). Entries must be non-empty.

One shared resolver builds separate delegation and handover target sets
(disabled / scope / hierarchy / `invocation_policy` / transition surface);
allowing one does not imply the other. Dispatch checks membership before
creating a task. Wildcard routing grants no cross-agent memory access
(that needs a named relationship or `readable_agents`). Multi-target
batches validate completely before the first child is created.

### `invocation_policy`

`AgentInvocationPolicy` · `discoverability: ambient`,
`delegation: wildcard`, empty `allowed_direct_surfaces`.

Where an agent may be discovered, delegated to, or bound directly. An
authorization input for discovery, catalog construction, and dispatch
(unlike `chat_inline`, which is product behaviour).

```yaml
invocation_policy:
  discoverability: surface_only # ambient | explicit | surface_only
  delegation: none              # wildcard | explicit | none
  allowed_direct_surfaces: [thinking_map]
```

- `discoverability`: `ambient` appears in generic discovery and
  composer/reference catalogs; `explicit` is hidden there but reachable by
  an exact authorized route; `surface_only` is hidden from both and needs
  one of its typed direct surfaces (must list at least one, no duplicates).
- `delegation`: `wildcard` lets `delegation_targets: ["*"]` include the
  agent; `explicit` requires the caller to name it; `none` blocks
  delegation even when a stale caller definition names it. `delegation`
  and `handover` are enforced independently on named targets.
- `allowed_direct_surfaces` (`InvocationSurface`): `chat`,
  `realtime_voice`, `task`, `delegation`, `handover`, `thinking_map`,
  `tutor`, `app_copilot`, `contextual_assist`, `public_envoy`, `meeting`,
  `plane`. Empty = the default set; non-empty = an **exact** allowlist.

Default surfaces: `chat`, `realtime_voice`, `task`, `delegation`,
`handover`, `tutor`, `app_copilot`, `contextual_assist`. `thinking_map`,
`meeting`, and `plane` are opt-in, so no existing agent is reachable from
a room or a terminal without a definition edit. `public_envoy` and
`meeting` have `untrusted` audience; all others are `owner`. The seed
`personal-assistant` restates every default plus `plane`.

A primary agent that narrows `allowed_direct_surfaces` must keep `chat`,
`task`, and `contextual_assist` (Magican keyboard Write/Ask/Act fail
independently otherwise; see
[magican-keyboard](../../magios/magican-keyboard.md)). Feature-only
definitions opt into a narrow policy (Loom: `surface_only +
delegation:none + thinking_map`) and list every direct tool explicitly.
Native `delegate_to_agent` / `handover_to_agent` descriptions tell the
model to use them only when the caller lacks the needed tool.

---

## 3. Execution Constraints

All sub-fields live under `constraints` (`AgentConstraints`).

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `max_iterations` | `u32` | `4000` | Observe-decide-execute cycles per goal pipeline (> 0) |
| `max_tokens_per_cycle` | `u64` | `20000000` | Development token budget per cycle; definitions may set lower (> 0) |
| `max_consecutive_failures` | `usize` | `3` | Consecutive failures before the pipeline aborts (> 0) |
| `allow_self_modification` | `bool` | `false` | Agent may modify its own definition at runtime |
| `max_duration_secs` | `Option<u64>` | `null` → `GOAL_PIPELINE_TIMEOUT_SECS` (10800 s) | Wall-clock cap per goal pipeline |
| `approval_ttl_secs` | `u64` | `86400` | Global approval-grant TTL; rules may override (> 0) |
| `requires_approval` | `Vec<ApprovalRule>` | `[]` | See below |
| `coordination` | `CoordinationConfig` | see below | Delegation limits |

### `constraints`

```yaml
constraints:
  max_iterations: 30
  max_tokens_per_cycle: 30000
  max_consecutive_failures: 2
  max_duration_secs: 600
  requires_approval:
    - tool: "browser"
      action: "navigate"
      when: { url_contains: ["bank.com", "payment"] }
      ttl_secs: 300
    - tool: "*"
      action: "delete"
  coordination:
    max_delegation_depth: 2
    allow_transitive_delegation: false
```

### `constraints.requires_approval`

Each `ApprovalRule`:

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `tool` | `String` | Yes | Tool name or `"*"`; must be trimmed and `"*"` or a declared tool |
| `action` | `String` or `[String]` | Yes | Action pattern(s); entries non-empty, not padded |
| `when` | `ApprovalCondition` | No | `param_matches: Map<String,[String]>` (`{}`), `url_contains: [String]` (`[]`); keys/patterns/entries non-empty |
| `ttl_secs` | `u64` | No | Per-rule TTL override (> 0) |

### `constraints.coordination`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `max_delegation_depth` | `u8` | `2` | Max depth of transitive chains (A → B → C …) |
| `delegation_timeout_secs` | `u64` | `300` | Compatibility value only (deserialization, pause snapshots); > 0 |
| `allow_transitive_delegation` | `bool` | `false` | When false, an agent already running as a delegation target (`current_depth > 0`) cannot delegate further. Leaf workers keep false; sub-delegating coordinators set true |

`delegation_timeout_secs` is **not** a cap on a delegated run. Explicit
`delegate_to_agent.timeout_secs` only extends its tier (the named depth, or
normal when none is named); `depth: normal` / `deep` /
`thorough` resolve to 300 / 900 / 1800 s; omitting both leaves no
delegation-specific budget. The resolved value is a soft active-work
budget, checked before new model turns and new dispatch (in-flight work
finishes); queueing, paused wall time, and finalization do not consume it.

---

## 4. Trust & Security

### `trust_level`

`TrustLevel` (String newtype) · `"local"` · recognized values `builtin`,
`local`, `reviewed`, `untrusted` (legacy `standard` → `local`).

Resolves against declarative `TrustPolicy` definitions at runtime to
decide allowed tool/action combinations. Matching trims and is
case-insensitive. A level matching no policy entry would deny every
action, so an unrecognized value is rejected at create/save, and a
definition already on disk **refuses to run** at execution start
(`load_trust_dispatch_guard`) rather than silently denying everything.

- `builtin` / `local` / `reviewed` get the full universal pack set minus
  excluded/denied names.
- `untrusted` gets only `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` and must
  declare a non-empty `tools` allowlist (`tools: []` = all is rejected).
  Exception: the exact tool-free public surface (`surface_only`,
  `delegation: none`, no delegation targets,
  `allowed_direct_surfaces: [public_envoy]`) gets an empty
  provider/dispatch snapshot.
- Direct chat keeps the compiled pack fast path for `builtin` / `local`;
  `reviewed` / `untrusted` pack calls go through the task-backed executor.
  Untrusted chat withholds spawn/delegate/handover from its initial
  catalogue.
- Trust denial is evaluated before approval. Pause state hashes the
  invocation context so resume cannot downgrade a Thinking Map, Tutor, or
  other protected lane. `task_state_action` is an authorized capability,
  not metadata. The V2 API and autonomous executor share one
  definition-store instance.
- In the flat loop, `deferred` means authorized for bounded discovery,
  not callable until a later provider decision advertises the schema.

Harnesses: `make test-agent-tool-visibility-eval-harness`,
`make test-agent-tool-visibility-live-eval`.

### Security layers

**Layer 1: tool access control.**

| Mechanism | Scope | How |
|-----------|-------|-----|
| `tools` / `excluded_tools` | Authorization + visibility | Narrow direct, deferred, provider, introspection, and dispatch projections of the effective snapshot |
| `denied_tools` | Hard authorization | Removes provider/runtime/deferred/structural grants and the scoped dispatch entry; dispatch fails before side effects |
| `denied_tool_params` | Parameters | Prefix-match resolved values before dispatch |
| Scoped `CapabilityRegistry` | Registry isolation | Each agent gets a filtered registry view |

**Layer 2: shell injection prevention.** Most packs use
`ImplementationType::Command` (`Command::new(program).arg()`, no shell),
so metacharacters in model-supplied values stay literal. The `shell`
compiled provider still uses `sh -c`, guarded by runtime-core's
`ShellSandboxConfig` (blocked_command_fragments, allowed_working_dirs,
allowed_binaries). Grant `shell` only to agents that need arbitrary
command execution.

**Layer 3: prompt injection defense.** External content (emails, web
pages, tool output, user-editable personas) is wrapped in boundary tags
(`<external_content>`, `<tool_output>`, `<task_context>`,
`<agent_identity>`); `neutralize_boundary_tags()` case-insensitively
neutralizes tag-closing attempts inside them, and every system prompt
tells the model to treat tagged content as data.

Minimal-privilege posture: name `tools` explicitly, put `shell` /
`delegation_shell` in `denied_tools`, and deny dangerous parameter
prefixes via `denied_tool_params`.

---

## 5. Memory

### `memory_tiers`

`Vec<MemoryTierDefinition>` · `[]` (see auto-defaults). Named, scoped,
schema-aware data that persists across cycles.

**Auto-defaults.** When `kind: personal` and both `memory_tiers` and
`memory_consolidation` are empty, `apply_defaults()` injects 6 tiers
(`entities`, `insights`, `recent_activity`, `archive`, `task_progress`,
`environment_knowledge`) and 9 consolidation rules, whether or not
`autonomous_config` is set. Workers never get defaults.

**Worker baseline.** Workers declare their own tiers and rules. The
canonical baseline (`simple-data-analyst`, `dashboard-builder`, other
deterministic workers): tiers `episode`, `semantic`,
`personality_profile`, plus one batch rule `consolidate_to_semantic`
(`min_episodes: 15`, `max_staleness_hours: 24`, source
`episodes(unprocessed=true)`, target `semantic`). Workers that drive
distinct environments add an `environment_knowledge` tier and
`extract_environment_knowledge` rule; see
[`ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md`](../execution/ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md).

`MemoryTierDefinition`:

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `name` | `String` | Yes | Unique, non-empty, trimmed, no dots |
| `scope` | `TierScope` | Yes | `agent`, `agent_goal`, `user` |
| `description` | `String` | Yes | Non-empty |
| `schema` | `Map<String, TierFieldSchema>` | No | Field types: `text`, `document`, `date_time`, `key_value_list`, `collection` (optional `max_items`, `item_schema`); keys non-empty |
| `render` | `RenderConfig` | Yes | `format` and `template`, both non-empty |
| `retention` | `RetentionMode` | Yes | `goal_lifetime`, `forever`, or `!days <n>` (n > 0) |

The loader also accepts the older bare-integer retention form
(`retention: 14`); re-serialization writes `!days`.

```yaml
memory_tiers:
  - name: "entities"
    scope: agent
    description: "Extracted entities with recency tracking"
    schema:
      entities: { type: collection, max_items: 1000 }
    render: { format: compact_summary, template: "{entities}" }
    retention: forever
```

### `memory_consolidation`

`Vec<MemoryConsolidationRule>` · `[]` (see auto-defaults). Rules that
transform data between episodes and tiers.

**Auto-default rules:** `extract_entities`, `extract_insights`,
`summarize_recent_activity`, `update_task_progress`, `distill_insights`,
`archive_old_episodes`, `expire_old_episodes`,
`extract_environment_knowledge`, `promote_to_user`. Default entity
collection is capped at 1,000 items. LLM rules over episode backlogs use
`batch` cadence (`summarize_recent_activity` and
`extract_environment_knowledge` fire at 10 unprocessed episodes or 24 h
staleness).

`MemoryConsolidationRule`:

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `String` | Unique, non-empty |
| `trigger` | `ConsolidationTrigger` | When to run |
| `source` | `String` | `episodes(goal_id, limit=N, unprocessed=true/false)` or `tiers(a, b)` |
| `target` | `String` | Tier name (root must exist in `memory_tiers`), `report:<channel>`, or `user.<path>` |
| `transform` | `ConsolidationTransform` | How to transform |

Triggers:

| Trigger | Form | Meaning |
|---------|------|---------|
| `cycle_completed` | String | After each cycle; for compact structured transforms, not high-volume LLM backlog extraction |
| `step_completed` | String | After each successful pipeline step |
| `retention_expiry` | String | When retention cleanup runs |
| `batch` | Mapping | `interval_hours`, `interval_days`, `min_episodes` (episode sources only), `max_staleness_hours`; at least one, all > 0 |

Transforms:

| Type | Fields | Notes |
|------|--------|-------|
| `structured` | `builtin` | `map_episode_to_task`, `append_strategy_record`, `promote_shared_insights` |
| `llm` | `prompt`, `operation?`, `system_prompt?`, `merge?` | Merge: `upsert_by_name`, `upsert_by_name_per_source`, `upsert_by_similarity`, `append_period` |
| `render` | `template` | Template rendering |

LLM transforms should set one typed `operation`:
`memory_entity_extraction`, `memory_environment_knowledge_extraction`,
`memory_insight_distillation`, `memory_user_promotion`,
`memory_archive_summary`. Invalid LLM output fails closed (tier unwritten,
cursor not advanced).

Behaviour:

- **User-tier fan-out.** Target `user.<tier>` routes each emitted item to
  `user.<target_tier>` from the item's `target_tier`. Allowed:
  `preferences`, `skills`, `contacts`, `workflows`, `identity`,
  `organization`, `accounts`, `channels`. `apply_user_target` stamps
  RFC3339 `updated_at` on every object item.
- **Empty-source skip.** A batch rule with an empty resolved source skips
  the LLM (`skipped_empty_source`); `StepResult` sources never skip.
- **Paid retry guard.** Batch LLM failures are fingerprinted in
  `memory_consolidation_runs.json`. Provider/parse/deadline/schema
  failures retry with capped exponential backoff (15m, 30m, …);
  configuration failures quarantine. A changed rule or semantic source
  clears the guard; timestamp-only touches do not.
- **Schema normalization.** Unknown fields fail closed. Lossless aliases
  are normalized before paid schema repair and on merge
  (`confidence_level` → `confidence`; research fact keys → `finding`;
  `source_urls` → `sources`; scalar `key_value_list` → `{summary: ...}`;
  `account_used` → `account`; `best_settings`/`effective_settings` →
  `typical_settings`). Collections are capped to `max_items` on ingest
  and after upsert.
- **Overflow archive.** Items evicted from a `max_items` collection move
  into sibling tier `<source_tier>_archive` (same field and
  `item_schema`) when it exists, else are dropped. The archive is
  terminal (`archive_overflow_items`).
- **Archive checkpoints.** Canonical `archive_old_episodes`
  (`memory_archive_summary` + `archive` + `append_period` +
  `episodes(unprocessed=true)`) commits one group of up to six episodes
  per checkpoint; the cursor advances only when the bounded snapshot has
  fully committed. Archive items carry reserved `source_episode_ids` for
  crash-safe replay. Retention shares that state lane and defers episodes
  owned by an active checkpoint.
- **Template ↔ scope sync.** Changes to `memory_tiers`,
  `memory_consolidation`, or `prompt_pipeline` must land in both
  `magician_data_v3/system/agent_templates/agents/<id>/definition.agent.yaml`
  and the scoped
  `scopes/<principal>/<workspace>/agent_runtime/agents/<id>/definition.agent.yaml`;
  the runtime reads the scoped copy.

Validation beyond the tables: `report:` needs a channel, `user.` needs a
sub-path; `min_episodes`-only batch needs `unprocessed=true`;
`agent_goal` tiers cannot be source/target of `batch` or
`retention_expiry`; no cross-agent tier sources; LLM prompt and render
template non-empty.

```yaml
memory_consolidation:
  - name: "extract_entities"
    trigger: step_completed
    source: "episodes(unprocessed=true)"
    target: "entities"
    transform:
      type: llm
      operation: memory_entity_extraction
      prompt: "Extract named entities."
      merge: upsert_by_name
  - name: "update_task_progress"
    trigger: step_completed
    source: "episodes(unprocessed=true)"
    target: "task_progress"
    transform: { type: structured, builtin: map_episode_to_task }
```

### `readable_agents`

`Vec<String>` · `[]`. Agent ids whose memory this agent may read.
Personal only.

### `user_memory_isolation`

`UserMemoryIsolation` · `shared` · values `shared` (shares the user's
memory tiers), `fully_isolated` (cannot read the user's personal tiers).

---

## 6. Prompt Pipeline

### `prompt_pipeline`

`Option<PromptPipelineConfig>` · `null`. How the system prompt is
assembled from sources.

#### `prompt_pipeline.sections`

Ordered `PromptSection` list:

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `String` | Unique, non-empty |
| `source` | `String` | Dynamic reference; exactly one of `source` / `content` |
| `content` | `String` | Inline text |
| `required` | `bool` (`false`) | Section must be present |
| `condition` | `String` | Inclusion condition (non-empty if set) |
| `format` | `String` | Rendering override (non-empty if set) |
| `filter` | `String` | Filter (non-empty if set); not allowed on tier or inline sources |

Sources:

| Source | Meaning |
|--------|---------|
| `definition.persona` | Agent persona |
| `definition.auto_surface_guidance` | Dashboardable-output guidance (with `auto_surface_policy.enabled`) |
| `memory.user_profile`, `memory.user_knowledge`, `memory.corrections` | User memory |
| `memory.episodes(limit=10)` | Episode history (no `unprocessed` selector) |
| `memory.tier[<name>]`, `tiers(a, b)` | Declared tiers only; no cross-agent references |
| `derived.failure_analysis`, `derived.success_patterns` | Derived analysis |
| `feedback.<name>` | Feedback-loop output |
| `strategy_context` | Strategy effectiveness data |

`definition.goals[...]` is not a source (goals live on `Task`); it fails
with `goal definitions removed from agents; use Task intent instead`.
When `auto_surface_policy.enabled` is true and no section declares
`definition.auto_surface_guidance`, the assembler auto-injects a
"Dashboardable Output" section.

#### `prompt_pipeline.output_rules`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `max_context_tokens` | `u32` | `4000` | Budget for the assembled prompt (> 0) |
| `truncation_priority` | `[String]` | `[]` | Declared section names, truncated first-to-last over budget; no duplicates |

```yaml
prompt_pipeline:
  sections:
    - { name: "persona", source: "definition.persona", required: true }
    - { name: "recent_history", source: "memory.episodes(limit=10)" }
    - { name: "entities", source: "memory.tier[entities]" }
    - { name: "failure_context", source: "feedback.failure_context", condition: "has_recent_failures" }
  output_rules:
    max_context_tokens: 8000
    truncation_priority: ["recent_history", "entities"]
```

---

## 7. LLM Routing

### `llm_routing`

`Option<LlmRoutingConfig>` · `null`. Pins the agent's operations to LLM
profiles. No shipped agent pins a model, so agents follow the global
`operation_mapping` in `llm-router.yaml` (and, on a harness engine, its
eligible side-calls follow that harness). Pins are set from the Crew
page's **Models** panel (`/crew/[id]` → Overview) or the YAML tab.

**Precedence.** A pin beats the owner's Settings routing override, the
flow's parent (harness) engine, and the config mapping for every
operation its lane covers. `planning` is the catch-all lane: every
operation not claimed by `evaluation` (tool evaluation,
parameter/discovery safety), `correction_extraction` (agentic input
interpretation), or `memory_consolidation` (memory extraction/review),
including `agentic_decision`. On a harness engine the harness replaces
the decision step, so a `planning` pin governs only remaining side-calls.

**Write validation.** `PUT`/`PATCH /agents/{id}` refuse a pin naming an
undefined profile (`422 unknown_pinned_profile`,
`unknown_llm_routing_references`): LLM lanes resolve against
`llm.router.profiles`, `coding_profile` against `coding.profiles[].id`.
Only names the write introduces are checked, so an untouched stale pin
does not block an unrelated edit.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `planning` | `LlmEndpoint` | `null` | Plan generation (catch-all lane) |
| `evaluation` | `LlmEndpoint` | `null` | Outcome evaluation |
| `correction_extraction` | `LlmEndpoint` | `null` | Correction extraction |
| `memory_consolidation` | `LlmEndpoint` | `null` | Memory consolidation transforms |
| `operations` | `BTreeMap<String, LlmEndpoint>` | `{}` | Per-operation overrides keyed by `operation_mapping` names; beat named lanes |
| `coding_profile` | `Option<String>` | `null` | `run_coding_task` profile when the call passes none; `null` → `coding.default_profile` |

`LlmEndpoint`: either `profile` (a `magician-config.yaml` profile id that
supplies model, reasoning effort, timeout, metadata; provider/model then
ignored) or both `provider` and `model`.

```yaml
llm_routing:
  planning: { profile: "opus47-messages-toolsany-rnone" }
  evaluation: { provider: "openai", model: "gpt-5.6-terra" }
  operations:
    agentic_decision: { profile: "opus47-messages-toolsany-rhigh" }
  coding_profile: "coding-premium"
```

---

## 8. Resilience

### `circuit_breaker`

`Option<CircuitBreakerPolicy>` · `null`. Escalating responses to
consecutive failures.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `thresholds` | `[CircuitBreakerThreshold]` | 1 → `inject_failure_context`; 3 → `open_circuit` notify `["chat"]` | Escalation steps; non-empty, strictly increasing, unique `failures` |
| `recovery` | `CircuitRecovery` | `user_reset` | `trigger: user_reset` (no `cooldown_hours`) or `trigger: time_based` + `cooldown_hours` (> 0) |
| `per_goal_override` | `Map<goal, {max_failures}>` | `{}` | Keys valid goal ids; `max_failures` > 0 |
| `idle_failure_threshold` | `u32` | `3` | Consecutive idle-goal failures before suppressing |
| `idle_open_duration_minutes` | `u32` | `60` | Minutes an idle circuit stays open |

`CircuitBreakerThreshold`: `failures` (`usize`, > 0), `action`
(`inject_failure_context` | `open_circuit`), optional `escalation`
(non-empty), optional `notify` (`chat`, `webhook`, `agent_memory`; no
empties or duplicates).

```yaml
circuit_breaker:
  thresholds:
    - { failures: 2, action: inject_failure_context }
    - { failures: 5, action: open_circuit, notify: ["chat", "webhook"] }
  recovery: { trigger: time_based, cooldown_hours: 6 }
```

### `feedback_loops`

`Vec<FeedbackLoopDefinition>` · `[]`. Extract signals from episodes and
inject them into the prompt pipeline.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `String` | Unique |
| `trigger` | `String` | e.g. `episode.outcome.is_failed` |
| `extract` | `FeedbackExtract` | `source` (required), `filter?`, `fields?` |
| `transform` | `String` | e.g. `failure_context` |
| `inject_into` | `String` | e.g. `prompt_pipeline.failure_context`; the pipeline and section must exist |

All strings non-empty. `FeedbackLoopDefinition::defaults()` provides
`failure_adaptation` (failure context), `success_reinforcement` (success
patterns), and `strategy_effectiveness` (strategy outcomes).

```yaml
feedback_loops:
  - name: "failure_adaptation"
    trigger: "episode.outcome.is_failed"
    extract: { source: "episodes(goal_id, limit=5)", filter: "outcome.is_failed" }
    transform: "failure_context"
    inject_into: "prompt_pipeline.failure_context"
```

---

## 9. Notifications

### `notification_rules`

`Vec<NotificationRule>` · `[]`. Routes lifecycle notifications through
progress channels `chat`, `webhook`, `agent_memory`; unregistered ids are
ignored during reconciliation.

- `chat` rules become session-scoped subscriptions when a chat session is
  active.
- `webhook` needs `MAGICIAN_PROGRESS_WEBHOOK_URL` or `SLACK_WEBHOOK_URL`.
- `agent_memory` records into the episode store with
  `{ agent_id, tier_name: "episode" }`.
- `circuit_breaker.thresholds[*].notify` already materializes
  `agent.circuit.opened` delivery; add an explicit rule only for a custom
  message or severity.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `match` | `String` | Yes | Event pattern |
| `severity` | `NotificationSeverity` | Yes | `low`, `medium`, `high` |
| `channels` | `[String]` | Yes | At least one; supported ids; no empties/duplicates |
| `condition` | `String` | No | Gates delivery |
| `message` | `String` | No | Custom message |

```yaml
notification_rules:
  - match: "agent.cycle.failed"
    severity: high
    channels: ["chat", "agent_memory"]
    message: "Agent cycle failed — may need manual intervention"
```

---

## 10. Data Lifecycle

### `retention`

`Option<RetentionPolicy>` · `null` (sub-field defaults apply when
created). All day counts > 0.

#### `retention.episodes`

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `default_days` | `u32` | `90` | Episode retention |
| `on_failure` | `u32` | `null` | Override for failed episodes |
| `per_goal_override` | `Map<String, u32>` | `{}` | Per-goal days |
| `consolidate_before_delete` | `bool` | `false` | Run consolidation before deleting |

When personal memory defaults are injected (see [Memory](#5-memory)),
episode retention becomes `default_days: 7`,
`consolidate_before_delete: true`. With `consolidate_before_delete`,
deletion is fail-closed: every applicable `retention_expiry` rule must
succeed, or the episodes stay for a later retry. Episodes claimed by a
durable archive checkpoint are excluded from deletion.

#### `retention.corrections`

`resolved_days` (`u32`, `30`); `active` (`u32` or `"forever"`, default
`"forever"`).

#### `retention.definition_versions`

`keep_last` (`u32`, `10`): definition versions retained.

```yaml
retention:
  episodes: { default_days: 30, on_failure: 60, consolidate_before_delete: true }
  corrections: { resolved_days: 14, active: forever }
  definition_versions: { keep_last: 5 }
```

---

## 11. Strategy & publication

### `strategy`

`Option<StrategyPreference>` · `null` (runtime default
`fixed: "atomic_composition"`). Execution strategy for plan generation
and execution.

| Variant | YAML | Meaning |
|---------|------|---------|
| `fixed` | `fixed: "atomic_composition"` | Always this strategy (non-empty) |
| `ordered` | `ordered: ["guided_search", "atomic_composition"]` | Try in order, fall back on failure (non-empty, unique) |
| `auto_select` | `auto_select` | Runtime picks from effectiveness data |

### `state_machines`

`HashMap<String, serde_json::Value>` · `{}`. Named state-machine
definitions (keys non-empty), interpreted by the declarative
`StateMachineInterpreter` that also drives ApprovalService.

```yaml
state_machines:
  login_flow:
    initial: "check_session"
    states:
      check_session: { transitions: { logged_in: "ready", not_logged_in: "authenticate" } }
      authenticate:  { transitions: { success: "ready", failure: "error" } }
```

### `auto_surface_policy`

`Option<AutoSurfacePolicy>` · `null`. When `enabled`, artifacts carrying
`RenderHints` are assembled into a surface and published to `route`; the
prompt pipeline auto-injects `definition.auto_surface_guidance` unless a
section already declares it.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `enabled` | `bool` | Yes | Publication on/off |
| `route` | `String` | Yes | Target route, e.g. `/briefing` |
| `title_template` | `String` | Yes | May use `{agent_name}`, `{goal}`, `{task_title}` |
| `materialize_as` | `Option<String>` | No | V3 render materialization mode (e.g. `muij_surface`) |
| `surface_kind` | `Option<String>` | No | Surface kind override (e.g. `dashboard`) |
| `placement_kind` | `Option<String>` | No | `task`, `workspace`, `thread`, `global` |
| `pinned` | `Option<bool>` | No | Pinned override for the placement |

---

## 12. Autonomy

### `autonomous_config`

`Option<AutonomousConfig>` · `null`. Autonomous execution cycles;
personal only. (Memory defaults apply to all personal agents, not just
autonomous ones.)

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `schedule` | `String` | required | Cron, >= 5 fields |
| `focus_areas` | `[FocusArea]` | required, non-empty | Cycle focus areas |
| `max_tasks_per_cycle` | `u32` | `3` | Tasks created per cycle |
| `max_steps_per_plan` | `u32` | `10` | Steps per plan |

`FocusArea`: `name` and `description` (required; slugs from `name` must
be unique), `priority` (`low` | `medium` (alias `med`) | `high`, default
`medium`), optional `schedule` (cron override), optional `program`
(harness program doc override), optional `scope` (harness scope
override; `["*"]` means the resolved harness scope, not the whole
workspace). `program` / `scope` non-empty when set.

```yaml
autonomous_config:
  schedule: "0 */4 * * *"
  focus_areas:
    - name: "inbox_triage"
      description: "Check email inbox, flag urgent items, draft responses"
      priority: high
      schedule: "0 * * * *"
      program: "daily_ops.md"
      scope: ["support-lead", "triage-worker"]
  max_tasks_per_cycle: 5
```

Each focus area registers as goal `harness:<agent_id>:<focus-area-slug>`.
A manual trigger without `goal_id` uses the single derived goal when
exactly one exists, else the caller must pass `goal_id`. Tasks inherit
`principal`/`workspace` from parent or request scope when the definition
omits them; the scheduler requires explicit scoped ownership.

### `harness`

`Option<HarnessConfig>` · `null`. Harness capabilities for personal
agents (workers must not set it). Harness tools are injected at runtime,
hidden from non-harness agents even when `tools` exposes all, and run
against scoped stores inside the owner's resolved harness scope.

- Read tools (`HARNESS_READ_TOOL_NAMES`): `list_episodes`, `read_trace`,
  `list_agents`, `inspect_agent`, `system_status`, `evaluate_harness`,
  `read_program_state`, `list_proposals`, `magician_work_ledger`,
  `inspect_backlog_delivery`.
- Acting tools (`HARNESS_ACTION_TOOL_NAMES`): `create_task`,
  `reassign_task`, `create_agent`, `update_agent`, `retire_agent`,
  `update_delegation`, `create_proposal`, `create_dashboard`,
  `update_program_state`, `propose_backlog_item`, `promote_backlog_item`,
  `review_backlog_delivery`. (`notify_owner` is a compiled handler, not a
  harness tool.)
- `create_task` / `create_agent` are spend-gated; new harness tasks are
  Internal unless `user_visible: true`. Structural tools are
  proposal-backed, evidence-required, and revalidated against scoped
  trust policy; `yaml_after` cannot redirect into another
  `principal`/`workspace`. `delegation_targets` and `readable_agents` must
  stay explicit and in-scope (`*` rejected). `retire_agent` is a scoped
  revision plus pause; read surfaces report `paused`, not `idle`.

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `program_section` | `Option<String>` | `null` | Section of canonical `program.md` injected for focus areas without their own `program`; non-empty if set |

Program docs load from the workspace program-spec root (`programs/`
locally, `Programs/` on SilverBullet). A focus-area `program` is injected
as-is; otherwise `program.md` plus `harness.program_section`. An optional
`Success Metrics` section accepts bullets such as
`harness_success_rate_30d >= 0.7`.

---

## Global Validation Rules

1. **String length:** every string field is capped at 32 KiB
   (`MAX_STRING_FIELD_BYTES`).
2. **`deny_unknown_fields`** on `AgentDefinition` and most nested structs:
   unknown keys fail at deserialization, not validation.
3. **Worker tools:** explicit, non-empty `tools`; `"*"` alone rejected.
4. **Scope pairing:** `principal` and `workspace` are both set or both
   omitted.
5. **Kind gates:** workers must not set `autonomous_config`, `harness`,
   `is_primary`, or `readable_agents`. `autonomous_config` needs at least
   one focus area and a 5-field cron.

---

## Part 2: Use Case Recipes

Fragments use only real `AgentDefinition` fields.

### Recipe 1: Scheduled Web Automation

Personal agent on a flaky site: narrow browser ceiling, circuit breaker,
failure notification.

```yaml
agent_id: "keka-clocker"
name: "Keka Clock-In/Out"
kind: personal
persona: |
  Clock in and out on the Keka HR portal every workday. Verify page state after clicking.
tools: ["browser", "files"]
browser_transports: [headed, headless]
constraints: { max_iterations: 30, max_consecutive_failures: 2 }
circuit_breaker:
  thresholds:
    - { failures: 2, action: inject_failure_context }
    - { failures: 4, action: open_circuit, notify: ["chat"] }
  recovery: { trigger: time_based, cooldown_hours: 12 }
notification_rules:
  - { match: "agent.cycle.failed", severity: high, channels: ["chat"] }
```

### Recipe 2: Research Worker with Quality Controls

Workers declare explicit `tools` and their own memory (no personal
auto-defaults); see the tier and rule examples in [Memory](#5-memory).

```yaml
agent_id: "deep-researcher"
kind: worker
name: "Deep Research Worker"
persona: |
  Search with variety, read full pages, cross-reference claims, cite sources.
tools: ["websearch", "htmltotext", "http", "files"]
browser_transports: [headless, headed]
chat_inline: auto
trust_level: "local"
```

### Recipe 3: Multi-Agent Coordinator

Primary personal agent that delegates to workers and reads their memory.

```yaml
agent_id: "coordinator"
name: "Project Coordinator"
kind: personal
is_primary: true
persona: |
  Break requests into sub-tasks, delegate to workers, synthesize answers.
tools: ["browser", "files", "websearch"]
delegation_targets: ["web-researcher", "simple-data-analyst"]
readable_agents: ["web-researcher", "simple-data-analyst"]
constraints:
  coordination: { max_delegation_depth: 2, allow_transitive_delegation: true }
```

### Recipe 4: Internal Scheduler Boundary

The scheduler is not an agent definition. It is an internal engine /
wake-queue path that evaluates scoped schedules and resumes eligible
goals; it is not materialized under `agent_runtime/agents/`, owns no
tools, and is not a delegation target.

---

## Runtime note: dedicated background runtime

Supervisor sweep loops (scheduler, approval expiry, memory consolidation,
WakeUpQueue, task scheduler, harness steward/autofix) run on a dedicated
`magician-bg` tokio runtime from `spawn_agent_supervisor_tasks`
(`magician-api/src/web_api.rs`), not the HTTP request runtime. Shutdown uses
`Runtime::shutdown_background()` after join handles complete. Agent
**runs** go to the main execution runtime via `spawn_execution_job`; only
sweep work is isolated.
