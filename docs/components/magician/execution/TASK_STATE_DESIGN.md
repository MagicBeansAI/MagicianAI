# Task State: Durable Agent-Writable State Across Runs

Related: [Agentic Execution Design](./AGENTIC_EXECUTION_DESIGN.md).

Scheduled and recurring tasks need structured state that survives a run —
a cursor such as `{ "last_seen_id": 39823500 }`. Task state is one JSON
checkpoint per `task_id`, stored in the scoped durable artifact store and
injected into the next outer-loop decision prompt. The agent chooses its
extra metadata; the runtime validates a typed `DurableTaskState`
(identity, status, micro-goals, evidence). No history is kept; the store's
`last_updated` frontmatter is the revision marker. `Task` records stay
lean: state is an artifact, not a field on every status read.

Why other persistence does not serve as a cursor:

| Mechanism | Why not |
| --- | --- |
| `completion_summary` | Lossy prose, no JSON roundtrip |
| `completion_artifact_names` | Output names, cleared on reset |
| `linked_task_inputs` | Cross-task wiring, not self state |
| Episode memory | Consolidation loses exact IDs |
| `prior_environment_knowledge` | Agent-scoped, not task-scoped |

## Write path: `task_state_action` on the decision

The model does not call a tool to mutate state. Native lowering
(`execution/agentic/native_lowering.rs`) reads an optional
`task_state_action` envelope on the **same** native tool call as the
selected action (alongside optional `decision_metadata`).

- Outer decisions: `TaskStateActionPolicy::Optional`. Missing → `action:
  none`. A present envelope is parsed strictly; malformed fails the
  decision.
- Inner-loop terminals: `TaskStateActionPolicy::Disabled`; the field is
  ignored.

```
TaskStateActionEnvelope
  action: none | create | patch | close
  reason: string, required, ≤ 500 chars
  confidence: optional 0.0–1.0
  next_review: next_outer_iteration | after_capability_return | on_resume | never
  proposed_taskplan: DurableTaskState object (create)
  patch: DurableTaskStatePatch object (patch)
  source_execution_id, source_iteration_range
  evidence_refs: JSON values naming observed evidence
  notes: optional
```

`action: none` must not carry `proposed_taskplan` or `patch`.

### Who authors which field

The model authors content; the runtime stamps identity and versioning
(`schema_version`, `task_id` / `expected_task_id`, `created_at` /
`updated_at`, `expected_updated_at`). `normalize_model_proposed_task_state`
and `normalize_model_task_state_patch` (`durable_task_state.rs`) fill them
before validation; runtime values win. The prompt's vocabulary is accepted:

- create: `micro_goals` or `requirements` (strings or objects with
  `description` / `title` / `text`, optional `id`, `status`,
  `evidence_refs`, `blocked_reason`) are lifted onto the runtime skeleton
  (`synthesize_durable_task_state`). Ids default to `mg_<n>`, status to
  `pending`; `completed` without evidence becomes `in_progress`. No usable
  goal → the skeleton's single goal. Non-terminal `status` and `notes`
  carry over.
- patch without `ops` is translated: known id+description →
  `set_micro_goal_status`, else `upsert_micro_goal`; top-level `status` →
  `set_status`; `blocked_reason` → `set_blocked_reason` on the active goal.
- a patch with no ops after normalization is kept as a progress note
  (state unchanged, not a rejection); its `reason` still reaches the
  transcript and the browser step judge reads it as `current_intent`.

Every fill is logged at `[TASK-STATE] normalized model-authored
create|patch`. Constraint: the model must never be required to supply
runtime-owned fields — the prompt does not render them.

`apply_outer_task_state_action` (executor) applies the mutation after the
decision. It refuses a missing `task_id`, an active `app_disclosure_guard`,
an allowlist/trust miss, or an approval rule requiring approval for the
`task_state` pack; rejects log `[TASK-STATE] Rejected outer
task_state_action` and leave disk unchanged. It then builds the provider
action (`persist_durable_task_state`) and calls
`TaskStateProvider::execute`; the model never assembles `__task_id` /
`__principal` / `__workspace`.

Prompt family: `agentic_decision` v1.3.7
(`magician-core/src/prompts/constants.rs`) renders `{task_state_section}`.

## Prompt section

`build_task_state_section` (`decision.rs`) renders for every task-backed
execution under `## PERSISTED TASK STATE`: the JSON, or on first run
`No persisted durable task state exists yet for this task`, plus the
instruction to include `task_state_action` inline only for a real
create/patch/close. Empty only when `task_id` is `None` or
`app_disclosure_guard` is set.

It ends with `TASK_STATE_ACTION_CONTRACT` (create/patch/close shapes,
micro-goal statuses, the evidence rule, which fields the runtime stamps).
Rendered only here; the tool schema keeps `task_state_action` an open
object so the contract is not repeated across the catalog.

## Typed checkpoint (schema 1.0)

`DurableTaskState` (`durable_task_state.rs`):

| Field | Contract |
| --- | --- |
| `schema_version` | `"1.0"` |
| `task_id` | Must match the executing task |
| `goal`, `success_criteria` | Required strings |
| `status` | `open` \| `in_progress` \| `blocked` \| `completed` \| `abandoned` |
| `created_at`, `updated_at` | RFC3339 |
| `micro_goals` | `{id, description, status, evidence_refs?, blocked_reason?}` |
| `active_micro_goal_id` | Null or a known micro-goal id |

Micro-goal status: `pending` \| `in_progress` \| `completed` \| `blocked`.
Completing one requires non-empty observed `evidence_refs` (not starting
with planned/todo/expected prefixes). Task `completed` requires every
micro-goal completed or blocked.

- **create**: valid `proposed_taskplan`, else a synthesized `in_progress`
  state with one `mg_initial` micro-goal.
- **patch**: `DurableTaskStatePatch` with `expected_task_id`,
  `expected_updated_at` (CAS against current `updated_at`), and non-empty
  `ops`: `set_status`, `upsert_micro_goal`, `set_micro_goal_status`,
  `set_active_micro_goal`, `set_blocked_reason`. Unknown ops and backward
  transitions fail closed. Missing state synthesizes an initial checkpoint
  first.
- **close**: requires existing state. Terminal status from `reason`/`notes`
  keywords (`blocked` / `abandoned`, else `completed`); open micro-goals are
  resolved; `completed` needs evidence.

A terminal `yield` can close or project a partial into
`metadata.latest_partial_yield` (`prepare_terminal_task_state_transition` /
`commit_terminal_task_state_transition`). Only the synthetic `mg_initial`
may remain open on a successful close.

## Provider and storage

`TaskStateProvider` (`execution/task_state_provider.rs`) holds
`{ workspace_layout, pack_def }` and no shared task-id state; identity
arrives per action as `__task_id`, `__principal`, `__workspace` (optional
`__execution_id` / `__agent_id`), so concurrent executions are isolated.
`lower()` wraps `ExecutableAction::Pack` with `maybe_wrap_with_spend_gate`.
Execute opens a per-(principal, workspace) store
(`open_local_durable_artifacts`, `<scope>/durable_artifacts/`) and writes
namespace `task_state`, name `{task_id}.json`.

Pack YAML is embedded at
`magician/src/magician_v2/execution/embedded_pack_defs/task_state.yaml`
(required `state` object param, `compiled` / `provider_name: task_state`,
timeout 5 s). Registered in `COMPILED_PROVIDERS` as `("task_state", true)`
(deferred until `set_compiled_handlers` binds it). The decision prompt,
not the pack guide, is the agent-facing contract.

Preload (`preload_task_state_for_direct_execution`) reads
`{task_id}.json` from the scope's store into `AgenticContext.task_state`
(pretty JSON). Missing → `None`; unreadable → logged, continue without.
`AgenticPauseState.task_state` carries it across pause/resume;
`load_persisted_durable_task_state` prefers that in-memory copy when it
parses.

## 16 KB cap

`MAX_STATE_BYTES = 16_384` pretty-printed bytes. Sized so an ordinary
multi-goal state with a terminal partial yield (4–6 KB) fits while a
runaway graph is refused. `trim_to_fit` first runs
`compact_durable_task_state_for_storage` on valid typed state (head/tail
compaction with a `…[compacted in task state]…` marker):

| Field | Cap |
| --- | --- |
| `goal`, `success_criteria` | 768 bytes each |
| micro-goal `description` | 512 bytes |
| `blocked_reason`, close/partial summaries | 384 bytes |
| partial `open` / `blockers` lists | 4 items × 192 bytes |

Identity, graph, status, timestamps and evidence refs are kept; compaction
sets `metadata.storage_compacted` / `storage_original_bytes`. Typed state
still over 16 KB is **refused** rather than dropping the graph. Untyped
JSON falls back to trimming the newest suffix of the largest top-level
array (keep ≥ 1 element); if nothing fits, the write fails closed.

Frontmatter: `created_by` / `last_updated_by` `task_state_provider`,
`content_type` `application/json`, `source_task_id`, optional
`source_execution_id` / `source_agent_id`.

## Tool visibility

The compiled `task_state` tool stays in the catalog as the persistence
backend (`native_integration.rs`).
`apply_task_state_tool_runtime_gating` injects it when `task_id` is set and
strips it otherwise; `replace_merged_agent_tools_preserving_runtime_task_state`
keeps it across catalog replacements and drops it for non-task runs or an
active app disclosure guard. Non-task executions see neither tool nor
prompt section.

## Example: HN daily monitor

Run 1 (no state yet): sweep `/topstories`, then on the progress decision:

```json
{
  "action": "create",
  "reason": "Seed cursor after first HN sweep",
  "proposed_taskplan": {
    "goal": "Check HN for Databases/Automation",
    "success_criteria": "New matching stories surfaced; cursor advanced",
    "micro_goals": [
      { "id": "mg_initial", "description": "Sweep HN and persist last_seen_id",
        "status": "completed", "evidence_refs": ["hn:39823500"] }
    ],
    "metadata": { "last_seen_id": 39823500 }
  },
  "evidence_refs": ["hn:39823500"]
}
```

Run 2: the prompt carries that JSON; filter `id <= last_seen_id`, then
`action: patch`. Omit `task_state_action` on steps that do not move the
cursor.

## Constraints

| Constraint | Value |
| --- | --- |
| Max size | 16 KB pretty-printed, fail-closed |
| Namespace / name | `task_state` / `{task_id}.json` |
| Store | Per-scope `DurableArtifactStore` (atomic write, fs2 lock, `ArtifactDomain::Durable`) |
| Agent-facing write | Inline `task_state_action` on the outer decision |
| Non-task / app-guard | No section, no tool, no mutation |

```
Run N decision
  └─ native tool call + optional task_state_action
       └─ apply_outer_task_state_action → persist_durable_task_state
            └─ TaskStateProvider::execute → DurableArtifactStore::write("task_state", "{task_id}.json")

Run N+1 setup
  └─ preload_task_state_for_direct_execution → ctx.task_state
       └─ build_task_state_section → {task_state_section}
```
