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
`http:api.stripe.com/v1/charges`, `bash:ffmpeg`.

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

Decision prompts render persisted memory through the generic memory-tier
prompt blocks (`memory_prompt_blocks.rs`). `environment_knowledge` ranks in
the environment/strategy/insights band; matching is not a dedicated
domain lookup.

`load_and_match_environment_knowledge` and `match_domain_from_cache` in
`execution/agentic/environment_knowledge.rs` still implement key/pattern
matching (`{kind}:` prefix, then identifier/domain overlap) and a compact
"what you already know" section. They have no production callers. Scratch
`supplemental_environment_knowledge` is only read and cleared, never written.

`AgenticContext.prior_environment_knowledge` is a prompt bucket reused for
durable-artifact summaries, delegated child results, and linked-task
artifact lists — not for seeded environment-tier entries.

## Surface Area

- Lore and memory views read the tier through the standard memory APIs.
- The decision path uses the generic memory-tier renderer (Set-of-Mark
  observation is retired; there is no separate SoM prompt path).

## Current Boundaries

- The tier stores compact heuristics, not full artifacts or long transcripts.
- Matching helpers, when used, are key and pattern based, not semantic
  retrieval across unrelated environments.
