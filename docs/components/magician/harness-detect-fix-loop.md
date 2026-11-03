# Harness CTO Detect-and-Fix Loop (Phase 1)

Phase 1 of the Autonomous Company Loop
(implementation plan):
turn each Autopilot cycle into a deliberate bug-catcher — detect
anomalies, surface them, and (by default) auto-draft a fix as an owner-review-gated
diff proposal.

> **Phase 2** builds on this: a shared company backlog, `propose_backlog_item` /
> `promote_backlog_item` action tools, real backlog + repo state injected into
> every cycle, and the CPO agent. See
> [Team routines — backlog and roles](./harness-company-loop-phase2.md).

## Flow

```
harness cycle finishes ─▶ classify (run_post_cycle_pipeline) ─▶ programs/anomalies/<sig>.json
                                                                     │
GET /api/magician/v2/harness/anomalies ◀── AnomalyStore ────────────┤
/harness UI page  ◀──────────────────────────────────────────────────┤
CTO cycle prompt  ◀── "### Anomalies caught (open)" (append_harness_program_context)
                                                                     │
autofix sweep (every 15m, default ON) ── select top open+uncooled ──▶ create_task(engineer)
                                                                     │
                                                                     ├─ start_execution(task)
                                                                     │
engineer task runs run_coding_task ─▶ Pending CodeChangeProposal ─▶ OWNER diff-approval ─▶ applied
```

## Detect

`run_post_cycle_pipeline` (`magician-api/src/web_api.rs`), right after `record_harness_cycle_outcome`,
classifies the just-finished cycle via `harness::anomaly::classify_cycle_anomaly(outcome, episode_failed, detail)`.
Only `harness:`-goal cycles are considered (`is_harness_goal`). Best-effort: a
failure to persist an anomaly logs a `warn!` and never blocks the pipeline.

Anomaly kinds (`harness::anomaly::AnomalyKind`): a failed cycle is
`CycleFailed` unless its failure detail text pins it to `SandboxDenied`,
`StuckHitl`, `ToolUnavailable` or `NoProgress` (checked in that order); a cycle
dropped before running is `CycleDropped`. `RosterDrift` is reserved.
Intentional drops (`dropped_disabled`/`_paused`/`_duplicate`/
`_reservation_changed`) are NOT anomalies, and neither is a cycle whose
failure is the cancellation itself (`Execution cancelled`, `... by user`), as
when the owner pauses the harness mid-cycle, unless its episode could not be
persisted.

## Store

`harness::AnomalyStore` persists one `programs/anomalies/<signature>.json` per
anomaly under the scope (`ArtifactV2Workspace::programs_anomalies_path`), deduped
by `signature = sha256(agent_id ∅ goal_id ∅ kind)[..16]`. Recurrence increments
`occurrences` + refreshes `last_seen`; a `Resolved` anomaly seen again re-opens.
`status`: `Open` → `FixDispatched` → `Resolved`/`Dismissed`. Atomic writes; a
missing dir reads as empty.

## Surface

- **API:** `GET /api/magician/v2/harness/anomalies[?status=open|fix_dispatched|resolved|dismissed]`
  (`magician-api/src/learning_api.rs::list_harness_anomalies_handler`) — mirrors `/harness/cycles`;
  returns `{scope, count, anomalies}`.
- **UI:** `/harness` (`ui/unified-ui/src/routes/(app)/harness/+page.svelte`) — a
  read-only "Harness caught N issues" list. Scope is auto-injected by the `(app)`
  layout's patched `fetch`.
- **Prompt:** `append_harness_program_context` appends an `### Anomalies caught (open)`
  block (this agent's own open anomalies, ×5) to its next cycle prompt, so the
  agent sees the issues it hit.

## Autofix (default ON, owner-review-gated)

A 15-minute sweep (`run_harness_autofix_tick`, sibling of the steward loop):

1. Repairs stale dispatches: a `FixDispatched` anomaly whose task is still
   `pending`/`ready`/`paused`/`deferred` is started; a missing or
   failed/cancelled fix task reopens the anomaly.
2. Per scope, picks the top **open + uncooled** anomaly
   (`harness::autofix::select_autofix_targets`, `AUTOFIX_MAX_PER_SWEEP = 1`,
   6h per-anomaly cooldown).
3. Creates a coding task (`ArtifactV2Service::create_task`, `approved: true`)
   for a coding-capable **engineer** agent, asking for a **minimal fix drafted
   as a diff proposal, not applied**, and starts it with
   `ArtifactV2Service::start_execution`. Only after the start succeeds is the
   anomaly marked `FixDispatched` with the task id.

The engineer's `run_coding_task` stages a `Pending` `CodeChangeProposal` behind
the `diff_approval` HITL; **the owner approves or rejects every diff**
(`web_api.rs` diff-approval resume). The harness cannot call `run_coding_task`
itself (a compiled pack tool, not a harness action tool), hence the
`create_task` → engineer bridge. No code is applied without owner approval.

## Config

Typed config (`config::HarnessConfig`, `harness:` section of the magician config;
see the repo-root `magician-config.yaml`):

| Field | Default | Env override | Meaning |
|---|---|---|---|
| `harness.autofix_enabled` | `true` (opt-out) | `MAGICIAN_HARNESS_AUTOFIX` (`0/1/on/off/…`) | Whether the autofix sweep dispatches fixes. `0/off` disables. |
| `harness.autofix_agent` | `"senior-software-developer"` | `MAGICIAN_HARNESS_AUTOFIX_AGENT` | The engineer the fix task is assigned to. **Must directly grant `run_coding_task`** (a coding engineer, not a router like `cto` whose persona forbids coding — that only works via a delegation hop). |
| `harness.paused` | `false` when absent; repo seed `true` | `MAGICIAN_HARNESS_PAUSED` (`1/on` = pause, `0/off` = resume) | **Global company-loop switch:** when `true`, scheduled and manual harness starts plus steward/autofix dispatches are blocked. The control API also clears queued harness triggers and requests cancellation of active harness cycles. Config read failures fail closed. |
| `harness.shared_program_docs` | `[]` | — | Playbooks (bare `*.md` names in each scope's `programs/`) appended to every program-context cycle as "Shared Context". A focus area's `shared_programs` replaces the list for that area (`[]` = none). Read per cycle; a read failure means no shared block. See [autonomous agents](autonomous-agents.md#shared-context). |

An env var set to a recognized value **overrides** the config; treat it as an
emergency override. The Crew control clears it from both runtime env files
before making the live config authoritative. Config is reloaded at every
admission/tick, so changes take effect without a restart. To pause autofix, set
`harness.autofix_enabled: false` (or `MAGICIAN_HARNESS_AUTOFIX=0`).

## Key files

- `harness/anomaly.rs` — `HarnessAnomaly`, `AnomalyKind`/`AnomalyStatus`, `classify_cycle_anomaly`.
- `harness/anomaly_store.rs` — `AnomalyStore` (`upsert`/`list`/`mark`).
- `harness/autofix.rs` — `autofix_env_override`, `select_autofix_targets`, consts.
- `magician-api/src/web_api.rs` — detector hook in `run_post_cycle_pipeline`; prompt block in
  `append_harness_program_context`; `run_harness_autofix_tick` + its loop.
- `magician-api/src/learning_api.rs` + `magician-bin/src/main.rs` — `/harness/anomalies` endpoint + route.
- `magician/src/config.rs` — `HarnessConfig`.

## Record writes are serialised per file

`BacklogStore` and `AnomalyStore` mutate a record by read `{id}.json` → change a
field → write the whole file, and both are constructed per request. Without a
lock, two handlers on one item (a promote racing a review, a resolve racing a
re-report) would lose one field while both return success.

`harness::harness_record_lock` keys a `std::sync::Mutex` by **record path**, not
by scope, so per-item files never contend and
`AnomalyStore::resolve_open_for_agent_goal` (list then mutate N records) is not
serialised against every other harness write. It is `std::sync` because nothing
awaits while it is held, and in-process because one process owns a data root.

A regression test must race two *different* mutators touching *different*
fields (owner-request vs. title-refreshing `upsert`); racing `set_owner_request`
against itself writes both fields from one read and passes even without the lock.
