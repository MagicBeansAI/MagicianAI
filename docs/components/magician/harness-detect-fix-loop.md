# Harness CTO Detect-and-Fix Loop (Phase 1)

Phase 1 of the Autonomous Company Loop
(implementation plan):
turn each autonomous harness cycle into a deliberate bug-catcher — detect
anomalies, surface them, and (by default) auto-draft a fix as an owner-review-gated
diff proposal.

> **Phase 2** builds on this: a shared company backlog, `propose_backlog_item` /
> `promote_backlog_item` action tools, real backlog + repo state injected into
> every cycle, and the CPO agent. See
> [Harness Company Loop — Phase 2](./harness-company-loop-phase2.md).

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

`run_post_cycle_pipeline` (`api/web_api.rs`), right after `record_harness_cycle_outcome`,
classifies the just-finished cycle via `harness::anomaly::classify_cycle_anomaly(outcome, episode_failed, detail)`.
Only `harness:`-goal cycles are considered (`is_harness_goal`). Best-effort — a
failure to persist an anomaly logs a `warn!` and never blocks the pipeline.

Anomaly kinds (`harness::anomaly::AnomalyKind`): `CycleFailed`, `CycleDropped`
(the two classified today), plus reserved `ToolUnavailable`, `StuckHitl`,
`NoProgress`, `SandboxDenied`, `RosterDrift` (Phase 1.5 — will inspect the
episode/detail text). Intentional drops (`dropped_disabled`/`_paused`/`_duplicate`/
`_reservation_changed`) are NOT anomalies.

## Store

`harness::AnomalyStore` persists one `programs/anomalies/<signature>.json` per
anomaly under the scope (`ArtifactV2Workspace::programs_anomalies_path`), deduped
by `signature = sha256(agent_id ∅ goal_id ∅ kind)[..16]`. Recurrence increments
`occurrences` + refreshes `last_seen`; a `Resolved` anomaly seen again re-opens.
`status`: `Open` → `FixDispatched` → `Resolved`/`Dismissed`. Atomic writes; a
missing dir reads as empty.

## Surface

- **API:** `GET /api/magician/v2/harness/anomalies[?status=open|fix_dispatched|resolved|dismissed]`
  (`api/learning_api.rs::list_harness_anomalies_handler`) — mirrors `/harness/cycles`;
  returns `{scope, count, anomalies}`.
- **UI:** `/harness` (`ui/unified-ui/src/routes/(app)/harness/+page.svelte`) — a
  read-only "Harness caught N issues" list. Scope is auto-injected by the `(app)`
  layout's patched `fetch`.
- **Prompt:** `append_harness_program_context` appends an `### Anomalies caught (open)`
  block (this agent's own open anomalies, ×5) to its next cycle prompt, so the
  agent sees the issues it hit.

## Autofix (default ON, owner-review-gated)

A 15-minute sweep (`run_harness_autofix_tick`, sibling of the steward loop) first
repairs stale dispatches from prior sweeps: a `FixDispatched` anomaly whose task
is still `pending`/`ready`/`paused`/`deferred` is started, while a missing or
failed/cancelled fix task reopens the anomaly for another attempt. It then reads
the queue and, per scope, picks the top **open + uncooled** anomaly
(`harness::autofix::select_autofix_targets`, rate-limited to
`AUTOFIX_MAX_PER_SWEEP = 1`, 6h per-anomaly cooldown). For that anomaly it creates
a coding task (`ArtifactV2Service::create_task`, `approved: true`) assigned to a
coding-capable **engineer** agent, whose description asks for a **minimal fix
drafted as a diff proposal — not applied**. The sweep immediately starts the task
with `ArtifactV2Service::start_execution`; only after that start succeeds is the
anomaly marked `FixDispatched` with the task id. The engineer's `run_coding_task`
stages a `Pending` `CodeChangeProposal` behind the existing `diff_approval` HITL;
**the owner approves/rejects every diff** (`web_api.rs` diff-approval resume).

The harness itself cannot call `run_coding_task` (it's a compiled pack tool, not
one of the harness action tools) — hence the `create_task` → engineer bridge. No
code is ever applied without owner approval.

## Config

Typed config (`config::HarnessConfig`, `harness:` section of the magician config;
see the repo-root `magician-config.yaml`):

| Field | Default | Env override | Meaning |
|---|---|---|---|
| `harness.autofix_enabled` | `true` (opt-out) | `MAGICIAN_HARNESS_AUTOFIX` (`0/1/on/off/…`) | Whether the autofix sweep dispatches fixes. `0/off` disables. |
| `harness.autofix_agent` | `"senior-software-developer"` | `MAGICIAN_HARNESS_AUTOFIX_AGENT` | The engineer the fix task is assigned to. **Must directly grant `run_coding_task`** (a coding engineer, not a router like `cto` whose persona forbids coding — that only works via a delegation hop). |
| `harness.paused` | `true` | `MAGICIAN_HARNESS_PAUSED` (`1/on` = pause, `0/off` = resume) | **Global company-loop switch:** when `true`, scheduled and manual harness starts plus steward/autofix dispatches are blocked. The control API also clears queued harness triggers and requests cancellation of active harness cycles. Config read failures fail closed. |

The env var, when set to a recognized value, **overrides** the config; otherwise
the config value applies (default ON). Treat it as an emergency override. The
Crew control clears it from both runtime env files before making the live config
authoritative. Config is reloaded at every admission/tick, so changes take
effect without a restart.

Autofix is ON by default; set `harness.autofix_enabled: false` (or
`MAGICIAN_HARNESS_AUTOFIX=0`) to pause it. The sweep is bounded (1 dispatch per
15-min tick, 6h cooldown per anomaly) and every diff is owner-approved.

## Key files

- `harness/anomaly.rs` — `HarnessAnomaly`, `AnomalyKind`/`AnomalyStatus`, `classify_cycle_anomaly`.
- `harness/anomaly_store.rs` — `AnomalyStore` (`upsert`/`list`/`mark`).
- `harness/autofix.rs` — `autofix_env_override`, `select_autofix_targets`, consts.
- `api/web_api.rs` — detector hook in `run_post_cycle_pipeline`; prompt block in
  `append_harness_program_context`; `run_harness_autofix_tick` + its loop.
- `api/learning_api.rs` + `bin/magician.rs` — `/harness/anomalies` endpoint + route.
- `config.rs` — `HarnessConfig`.

## Record writes are serialised per file

`BacklogStore` and `AnomalyStore` mutate a record by read `{id}.json` → change a
field → write the whole file. Both are constructed per request, so without a lock
two handlers on one item (a promote racing a review, a resolve racing a
re-report) lose one handler's field while both return success.

`harness::harness_record_lock` keys a `std::sync::Mutex` by **record path**, not
by scope: per-item files should never contend, and a scope-wide lock would
serialise `AnomalyStore::resolve_open_for_agent_goal` (list then mutate N
records) against every other harness write. `std::sync` because nothing awaits
while it is held; in-process is sufficient because one process owns a data root.

A regression test must race two *different* mutators touching *different*
fields (owner-request vs. title-refreshing `upsert`); racing `set_owner_request`
against itself writes both fields from one read and passes even without the lock.
