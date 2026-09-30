# Plan flow — how the planner finds tools

How Magician's planning tier decides what to do, where its tool catalog comes
from, and what happens to a plan once it exists.

## Strategies

Two exist (`strategy/types.rs::StrategyType`):

- `GuidedSearch` — beam search over tool matches.
- `AtomicComposition` — a reasoning-model call that emits one executable plan
  directly, with no matching phase.

`magician-config.yaml` sets `consumer_mode: true`, which forces
`AtomicComposition` (`v2_orchestrator.rs`, the consumer-mode guard before
`execute_strategy_with_retry`) and blocks escalation or simplification to
`GuidedSearch`. In this deployment `AtomicComposition` is the only planner.

## Where the tool catalog comes from

```
AtomicCompositionStrategy::explore            strategy/atomic_composition.rs
  → StrategyContext::merged_agent_tools()     strategy/types.rs
  → planner_agent_catalog[SelfAgent].tools    ← the resolved surface
  → group_atomic_tools                        strategy/atomic_filter.rs
  → format_for_prompt_compact                 + render_planner_tool_catalog
                                              + render_available_procedure_skills
  → validate_plan                             strategy/plan_validator.rs
```

`V2Orchestrator::visible_tools_for_definition` resolves one agent's surface:

1. **The agent's allowlist.** `ToolCatalog::agent_filtered_tools(definition.tools,
   definition.excluded_tools, ctx)`. An empty `tools` list means "no
   restriction"; a non-empty one matches on tool `name` or on `categories`.
2. **Harness tools** — injected for `Personal` agents that declare `harness`,
   stripped for everyone else.
3. **`denied_tools`** — removed.

`build_planner_agent_catalog_for_scope` then unions the universal backend packs
onto the **owner's** entry (`merge_universal_backend_packs`), re-applies deny,
and re-sorts. `StrategyContext::merged_agent_tools` reads that entry, merges
the delegate catalogs on top, and applies `denied_tools` once more — that final
pass is load-bearing, because deny is documented as "must never appear in plan
steps" and a delegate exposing a denied name would otherwise push it back in.

> **The union is planner-only, and must stay that way.**
> `visible_tools_for_definition` also feeds
> `resolve_merged_agent_tools_for_scope` → `OwnerExecutionProfile` →
> `AgenticContext::merged_agent_tools`, and the executor reads *that* list as
> "what the YAML granted" **before** injecting the substrate itself.
> `build_catalog_context` derives `granted_capability_names` from it, and
> `coordinator_grant_gate` decides delegate-only status by asking whether the
> agent holds `shell` / `read_file` / `write_file` / `edit_file` / `grep` /
> `glob` / `http` — seven universal packs. Unioning upstream makes
> `holds_work_tool` unconditionally true, so no agent is ever delegate-only and
> the FIX #4 coordinator tool gate stops firing entirely.
> `derive_allowed_action_types` reads the same list via
> `has_direct_pack_tools`. The planner catalog has no such consumers: it is
> read only for prompt rendering and `providing_agent_id_for_tool`, and is
> never persisted.

Underneath, `agent_filtered_tools` reaches `LocalToolServices` in
`magician-bin`, whose surface for a scope is the tool-runtime registry (empty
by default: `tool-runtime-config.yaml` has `registry.paths: []`) unioned with
`scope_pack_surface`, i.e. `load_pack_defs_for_scope(principal, workspace)`
pruned by `prune_unexecutable_pack_defs` + `prune_runtime_disabled_pack_defs`.
The executor's `ScopedCapabilitySnapshot` starts from the same
`load_pack_defs_for_scope` and the same two prunes, so plan-time and run-time
agree on what a scope contains.

A pack def comes from either `skills/<skill>/tool_schema.yaml` or a
`runtime_contract:` block inside `skills/<skill>/SKILL.md`, with the embedded
compiled defs (`execution/embedded_pack_defs/`) filling any name a disk source
did not provide.

## The universal backend packs

`UNIVERSAL_BACKEND_PACKS` (`execution/agentic/native_integration.rs`) is the
substrate every agent gets at run time regardless of its YAML: `shell`,
`files`, `http`, `read_file` / `write_file` / `edit_file`, `glob`, `grep`,
`web_search` / `web_fetch`, `content_search` / `content_read`, the memory and
read-only introspection tools, non-face-coupled task management,
`activate_skill` / `deactivate_skill`, `tool_search`, `time_math`.

Agent definitions deliberately omit these — Presto's `tools:` block says so in
a comment. **That is why the planner must union them in too** — otherwise the
planner's catalog misses every atomic primitive the executor holds.
`merge_universal_backend_packs` does so, mirroring every gate
`build_catalog_context` applies:

| gate | effect |
|---|---|
| `trust_level` is untrusted | only `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` |
| `discoverability == SurfaceOnly` | no substrate at all |
| catalog carries `app_commit_mutations` | no substrate — app-workflow runs dispatch under the installation's grant |
| `excluded_tools` / `denied_tools` | stripped |

**Delegates are excluded.** A delegate receives the substrate when *it* runs,
but enumerating it per delegate would dominate the prompt for an owner with
`delegation_targets: ['*']`, so `render_planner_tool_catalog` emits a one-line
note in each delegate section instead.

**A delegate never displaces the substrate.** `merged_agent_tools` normally
lets a delegate replace an owner's same-named tool — the delegate is the
specialist. That rule does not extend to universal packs, because the owner
runs those natively anyway. Delegates do list universal names (e.g.
`vc-researcher` → `read_file`), so without the guard a plan step for a trivial
file read would carry `Providing agent: vc-researcher`. The executor is the authority and disagrees:
`extract_direct_capabilities` drops every delegate-owned tool and re-adds the
substrate from the embedded defs, making these the owner's own direct
capabilities. `is_universal_backend_pack` is the shared predicate.

`native_integration.rs::universal_backend_pack_contract_has_no_schema_or_safe_subset_drift`
pins that every universal name has an embedded def and that the untrusted
subset stays a subset.

## Prompt shape and cost

`render_atomic_tool_prompt` renders the catalog per phase. The **outline** call
gets names and descriptions only — it is choosing *which* tools. The
**expansion** call also gets parameter signatures, because it has to fill a
`parameters` object per step and `validate_plan` books every unfilled required
parameter as an `UnresolvedInput`.

That split is a cost decision: signatures take the flat listing from ~2.2k to
~12.5k tokens, and the expansion already retries up to three times, so sending
them to the outline too would add ~10k redundant tokens to every planning run.

Both phases also carry the agent-grouped catalog, which re-lists the same tools
with their owning agent. That duplication is deliberate — it is what tells the
planner which delegate provides what — but it is why the catalog is the
dominant term in this prompt.

## Procedure playbooks

A procedure skill carries no `runtime_contract:`, so it produces no pack and
can never appear in a tool catalog. Its only entry point is `activate_skill`,
whose `name` is a free-form string. Both the executor
(`AgenticContext::available_procedure_skills`) and the planner
(`StrategyContext::available_procedure_skills`) therefore enumerate them from
`skills::agent_procedure_skill_catalog`, keyed on the agent's `tools:`
allowlist.

Skill-only entries are **not** pushed into the tool catalog itself;
enumeration is the discovery mechanism.

## What happens to a plan

**Plans are advisory prose, by design.** `V2Orchestrator::execute_plan_graph`
does not run the plan as a step program. It renders it through
`render_plan_graph_runtime_context` — goal, success criteria, objectives,
constraints, and a numbered `Planned Steps` list carrying `Tool:` and
`Providing agent:` per step — and hands that to the agentic loop as runtime
context, prefixed with:

> Use this approved PlanGraph as advisory execution context. Follow it when it
> matches runtime observations; adapt when the live environment proves it
> stale or incomplete.

The executor then selects tools from its own `ScopedCapabilitySnapshot`.

`execution/lowering.rs` still holds the `PlanGraph → ExecutableStep`
translation (`lower_plan_to_executable_steps*`,
`lower_step_to_executable_action`), but **every caller is a test**. It is
retained deliberately, not wired. Do not read its presence as evidence that
plans execute as step programs.

Because the plan is advisory, a planner/executor catalog mismatch does not
fail loudly — it produces a plan naming tools the executor may not hold, or
omitting ones it does. That is what makes the parity tests in
`strategy/types.rs` worth keeping.

## Failure behaviour

`validate_plan` rejects a plan that references a tool outside the catalog it
was given, cycles, or dangling edges; `compose_with_retry` then retries up to
three times with corrective feedback. Because `AtomicComposition` is terminal
under consumer mode, exhausting those retries returns an empty exploration
result — `trigger_execution` finds no PlanGraph and falls back to
`trigger_execution_direct` with `skip_planning`. The task still completes on
the agentic loop, so this path is logged at `warn!` naming the blocked
fallback — a degraded planning tier must stay visible.

`validate_plan` also enforces browser session/observation metadata and a
shell/file rationale. Those gates read `composition_category`, falling back to
`categories` when the pack omits it, so a missing optional field cannot switch
validation off.

## Known gaps

- **MCP tools** reach the runtime through
  `execution/primitive_dispatch/governed_mcp.rs` and are planner-visible only
  when a scope skill publishes them as a pack.
- **App computed capabilities** are planner-invisible on purpose — the
  fail-closed app-origin gate in `scoped_capability_resolver.rs` requires them
  to be reached through the app-workflow path.
- **`keywords` and `use_cases`** on `ToolInfo` are always empty in production
  (`LocalToolServices::tool_to_info` hardcodes them), so
  `AtomicToolSet::format_for_prompt`'s sections for them are inert. That
  formatter has no production caller either; the live path is
  `format_for_prompt_compact`.
