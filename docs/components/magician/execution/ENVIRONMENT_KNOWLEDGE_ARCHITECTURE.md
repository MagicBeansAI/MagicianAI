# Environment Knowledge Architecture

## Purpose

`environment_knowledge` is the agent-scoped memory tier for durable knowledge
about websites, HTTP APIs, CLI tools, and local execution environments. Its job
is to stop the runtime from rediscovering the same blockers, layouts, auth
requirements, and successful interaction patterns on every run.

## Storage Model

- Tier name: `environment_knowledge`
- Scope: agent
- Retention: forever
- Merge strategy: `UpsertByName`
- Key format: `{kind}:{identifier}`

Representative keys: `browser:github.com/login/*`,
`http:api.stripe.com/v1/charges`, `bash:ffmpeg`, `tool:metabase-mcp`.

Each entry is compact heuristics, not a transcript: `page_type`,
`layout_notes`, `known_blockers`, `successful_patterns`, `failure_modes`,
`auth_required`, `last_used`, `use_count`.

## Write Path

Environment knowledge is produced during memory consolidation:

1. The executor records a cycle episode.
2. The `extract_environment_knowledge` rule runs on a **Batch** trigger
   (`min_episodes: 10`, `max_staleness_hours: 24`), not on `CycleCompleted`.
3. Prompt templates extract environment-specific observations from unprocessed
   episodes (`$ref:memory_extract_environment_knowledge:1.1.0` and
   `$ref:memory_extract_environment_knowledge_system:1.1.0`).
4. The memory service upserts one canonical entry per environment key.
5. **Run end.** When a *task* run finishes (success or failure; not a chat
   turn), `consolidate_cycle_completed_v3` also runs the agent's batch
   `extract_environment_knowledge` rule against that one episode
   (`episodes(limit=1)`, rule name suffixed `_run_end`), so a lesson about a
   tool lands before the next run instead of up to a day later. Agents that
   already declare a `CycleCompleted` environment rule are left alone. The batch
   rule still runs; the merge is an upsert by name.

### Wiring per agent

Personal agents (`kind: personal`) receive the tier and rule via auto-defaults
when both `memory_tiers` and `memory_consolidation` are empty — see
[agent-definition-reference.md](../agents/agent-definition-reference.md).
Worker agents never receive those defaults and must declare the rule explicitly.

Templates under `magician_data_v3/system/agent_templates/agents/` that declare
the rule today: workers `mac-operator`, `web-researcher`, `web-researcher-opc`,
`executive-assistant`, `simple-data-analyst`, `internal-system-analyst`; personal
templates `creative-mind` and `personal-assistant` (explicit, not auto-default).
Other environment-driving workers should declare the same batch rule:

```yaml
memory_consolidation:
- name: extract_environment_knowledge
  trigger:
    batch:
      min_episodes: 10
      max_staleness_hours: 24
  source: episodes(unprocessed=true)
  target: environment_knowledge
  transform:
    type: llm
    prompt: $ref:memory_extract_environment_knowledge:1.1.0
    system_prompt: $ref:memory_extract_environment_knowledge_system:1.1.0
    merge: upsert_by_name
```

## Read Path

The generic memory-tier prompt blocks deliberately exclude
`environment_knowledge` (`memory_prompt_blocks.rs`, `include_environment_knowledge:
false`); the tier reaches prompts only through the paths below.

**Tool knowledge (`tool:` keys).** At the start of a direct (task) execution,
`inject_tool_environment_knowledge_for_direct_execution` (`v2_orchestrator.rs`)
loads the tier and keeps `tool:<identifier>` entries whose identifier is one of
the agent's granted tools — exactly, or a dotted/`__` member of it, so knowledge
about a retired tool (`tool:metabase`) never attaches to its successor
(`metabase-mcp`). At most six, most recently used first, rendered by
`load_tool_environment_knowledge` (`execution/agentic/environment_knowledge.rs`)
as "WHAT YOU LEARNED ABOUT YOUR TOOLS IN EARLIER RUNS" inside data fences, and
appended to `AgenticContext.prior_environment_knowledge`. Executions confined to
an engagement or program skip it: entries carry no engagement labels to filter
on.

`load_and_match_environment_knowledge` and `match_domain_from_cache` still
implement URL/command matching for `browser:`/`http:`/`bash:`/`file:` keys and
have no production callers. Scratch `supplemental_environment_knowledge` is
written by runtime notices (task workspace, unloaded families, working-set
routing) and rendered in full and delta prompts.

`AgenticContext.prior_environment_knowledge` also carries durable-artifact
summaries, delegated child results, linked-task artifact lists, and — for a
same-owner sub-goal — the parent's progress (see
[AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md#data-by-reference-and-sub-goal-hand-off)).

## Surface Area

- Lore and memory views read the tier through the standard memory APIs.
- The decision path renders tool knowledge as above (Set-of-Mark observation is
  retired; there is no separate SoM prompt path).

## Current Boundaries

- The tier stores compact heuristics, not full artifacts or long transcripts.
- Matching helpers, when used, are key and pattern based, not semantic
  retrieval across unrelated environments.
