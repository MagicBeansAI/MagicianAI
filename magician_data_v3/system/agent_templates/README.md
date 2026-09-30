# Agent Templates

Shipped agent definitions live under `agents/<name>/definition.agent.yaml`.
Each file is the data-driven, fully-declarative source of truth for one agent:
its persona, tools, delegation policy, memory tiers, prompt pipeline, and
operational constraints.

> Agents are **data, not code.** Adding a new agent is a YAML file plus
> `make agent_templates-install` — no Rust changes, no recompile.

## Layout

```
magician_data_v3/system/agent_templates/
├── README.md                       # this file
└── agents/
    └── <name>/
        └── definition.agent.yaml   # the single source of truth
```

Workspace-scoped overrides materialise under
`magician_data_v3/scopes/<principal>/<workspace>/agent_runtime/agents/<name>/`
and shadow the system version on a per-name basis (no merging within a name).

## Definition schema (overview)

| Top-level key | Purpose |
|---|---|
| `name` / `aliases` | Canonical name and alternates the runtime accepts at chat targets and delegation calls |
| `kind` | `personal` (chat-capable, owned by a principal) vs `worker` (delegation-only, no direct chat surface) |
| `persona` | Personality + role description; rendered into the system prompt |
| `tools` | Allow-list of tool/skill names the agent may invoke |
| `delegation_targets` | List of agent names this agent may delegate to (`'*'` wildcard or explicit list) |
| `constraints` | `max_iterations`, `max_delegation_depth`, `max_duration_secs`, self-modification flags |
| `trust_level` | `local`, `network`, `system` — feeds into the capability authority layer |
| `memory_tiers` | Per-agent memory schema (see below) |
| `memory_consolidation` | Per-agent consolidation rules (see below) |
| `prompt_pipeline` | Ordered list of system-prompt sections the agent assembles each turn |
| `social_persona` | Explicit Town Square participation: shipped templates set `opted_out: false`, introversion `0.5`, and `daily_tokens: 2000`. An absent block does not opt an agent in. |

Changing templates does not overwrite existing workspace definitions. Apply the
same social settings to existing agents through their scoped definition API,
then refresh the Town Square roster. Apps bind their runner definition at
installation review, so changing that runner requires renewed review before
its Apps actions can run again. The workspace's social policy and each
behavior's reviewed resource budget still govern autonomous activity.

## Pluggable memory tiers (per-agent schema)

Every agent declares its own typed memory tiers in the definition file. The
runtime owns the storage backend (Parquet/JSONL lakehouse + LanceDB hybrid
index); the *shape* of memory is fully data-driven per agent.

A memory tier declaration looks like:

```yaml
memory_tiers:
  - name: episode
    scope: agent             # agent | user | agent_goal
    description: 'Recent macOS UI driving sessions: target app, task, tools used, outcome'
    schema:
      entries:
        type: collection
        max_items: 50
        item_schema:
          timestamp:    { type: date_time }
          target_app:   { type: text }
          summary:      { type: text }
          outcome:      { type: text }
    render:
      format: compact_summary
      template: '{entries}'
    retention: !days 14

  - name: environment_knowledge
    scope: agent
    description: Learned quirks of specific apps (AX labels, gotchas, coord ranges)
    schema:
      environments:
        type: collection
        max_items: 100
        item_schema:
          name:                  { type: text }
          environment_key:       { type: text }
          successful_patterns:   { type: text }
          failure_modes:         { type: text }
          known_blockers:        { type: key_value_list }
          last_used:             { type: date_time }
          use_count:             { type: text }
          kind:                  { type: text }
    render:
      format: compact_summary
      template: '{environments}'
    retention: forever
```

**Supported field types** (non-exhaustive): `text`, `date_time`, `collection`
(with `max_items` and an `item_schema`), `key_value_list`, `bool`, plus
scope-aware references.

**Scope semantics:**
- `agent` — visible only to this agent
- `agent_goal` — scoped to a specific autonomous goal within this agent
- `user` — shared across agents owned by the same principal (cross-agent
  knowledge transfer surface)

**Retention** is either `forever` or a typed duration (`!days N`, `!hours N`).
The runtime compactor enforces retention windows on the underlying lakehouse.

**Render format** controls how the tier is injected into the agent's system
prompt each turn. The runtime currently supports `compact_summary` (item
template repeated) plus the underlying generative-UI MUIJ surfaces for
operator dashboards.

## Memory consolidation

```yaml
memory_consolidation: []   # empty = no consolidation; runtime persists raw items
# or:
memory_consolidation:
  - tier: episode
    rule: learn_contact_preferences
    cadence: !hours 6
  - tier: environment_knowledge
    rule: extract_environment_knowledge
    cadence: per_episode_completion
```

Consolidation rules are named identifiers the runtime knows how to execute
(typically LLM-driven extraction passes that write back into other tiers).
The current rule set ships in
`magician/src/magician_v2/learning/memory_bridge.rs` and the prompt
templates live under `data/magician_v2/prompts/memory_*`. Adding a new
consolidation rule is a Rust + prompt change; *applying* an existing rule to
a new agent is purely declarative.

## What this means for pluggability

| Dimension | Per-agent? | How |
|---|---|---|
| Memory shape (tiers, schemas, retention) | ✅ YAML | Edit `memory_tiers:` |
| Consolidation cadence + rule selection | ✅ YAML | Edit `memory_consolidation:` |
| Tool allow-list | ✅ YAML | Edit `tools:` |
| Delegation policy | ✅ YAML | Edit `delegation_targets:` |
| Prompt assembly order | ✅ YAML | Edit `prompt_pipeline.sections:` |
| Storage backend (Parquet, LanceDB) | ❌ runtime-owned | One canonical backend; no third-party plug-in |
| Consolidation *implementations* | partly | Existing rules: pure YAML. New rules: Rust + prompt |

This is the inverse of frameworks like Hermes that ship a `MemoryProvider`
ABC with 8 third-party backends (mem0, honcho, supermemory, etc.) — those
make the **storage backend pluggable but the schema fixed**; Magician makes
the **schema pluggable per agent but the backend fixed**.

## Closed self-improvement loop

Agents don't just consume their memory tiers — they *write back* through the
learning bridges:

- **`LearningProcedureSkillPromotionBridge`** — after a procedure executes
  successfully N times (default 3) with M evidence rows (default 2), the
  procedure is auto-promoted to a skill candidate. The candidate goes through
  the capability-evolution backlog plus an eval-backlog item before
  becoming a runtime-callable skill.
- **`LearningMemoryBridge`** — writes learning events into the appropriate
  memory tiers based on candidate type and scope.
- **`LearningCapabilityEvolutionBridge`** — routes promoted candidates into
  the capability backlog for runtime registration.
- **`LearningEvalBridge`** — auto-creates eval candidates so promotions are
  evaluated before they reach production runtime.
- **`LearningProgramStateBridge`** — autonomous program-state transitions
  (advance / block / review) for harness-driven agents.

Full lifecycle (extraction, retrieval, feedback, promotion, growth
evaluation) is documented in
[`docs/components/magician/learning-procedures.md`](../../../docs/components/magician/learning-procedures.md).

## Installation

```bash
# System-wide (replaces magician_data_v3/system/agent_templates/agents/<name>/):
make agent_templates-install

# Scoped (only applies to one principal/workspace):
make agent_templates-install-scope SCOPE=<principal>/<workspace>
```

Both targets are idempotent. The runtime picks up changes on the next chat
session for that agent — no process restart needed.

## Always-on tools

Some skills are baseline for an entire agent class — granted in every
template of that class rather than opted-in case-by-case.

| Skill | Granted to | Why |
|---|---|---|
| `dugite` (bundled git) | `architect`, `cto`, `engineering-manager`, `frontend-engineer`, `junior-frontend-engineer`, `junior-software-engineer`, `principal-software-engineer`, `senior-software-developer` | Engineering work is git-shaped. The dugite skill vendors a pinned git binary so agents don't depend on a host-installed `git`, and the per-scope install pipeline symlinks it into `<scope>/skills/dugite/bin/git` automatically. Chat-line agents (e.g. `personal-assistant`) intentionally don't get it — they reach git by delegating to an engineering agent. |
| `document-to-markdown` (AnyDoc) | `brainstorm-facilitator`, `company-assistant`, `creative-mind`, `executive-assistant`, `personal-assistant`, `simple-data-analyst`, `vc-researcher`, `writing-assistant` | These agents already accept local document context. AnyDoc is their ordinary Office/OpenDocument/EPUB/CSV/text-PDF reader; `ocr` remains the scan/image fallback and `pdftotext` remains the PDF-specialist path. `web-researcher` reaches the same reader through governed `content_read` instead of a direct grant. |

When you add a new agent in one of these classes, copy the
class-baseline `tools:` block from a sibling template rather than
hand-picking; this keeps the class invariant intact.

## Cross-references

- Closed self-improvement loop: [`docs/components/magician/learning-procedures.md`](../../../docs/components/magician/learning-procedures.md)
- Memory retrieval + evals: [`docs/components/magician/memory-evals.md`](../../../docs/components/magician/memory-evals.md)
- Memory index (hybrid LanceDB): [`docs/components/magician/memory-index.md`](../../../docs/components/magician/memory-index.md)
- Delegation ownership + transfer: `docs/archive/plans/2026-03-21-delegation-v2-ownership-transfer-design.md`
- Resource authority design: `docs/archive/plans/2026-05-20-agent-resource-authority-design.md`
- Architecture overview: [`docs/ARCHITECTURE_V2.md`](../../../docs/ARCHITECTURE_V2.md)
