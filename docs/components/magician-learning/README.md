# Magician Learning

**Current development version:** `0.1.5` (off-plan attribution in the execution panel).

The learning plane as a satellite crate (about 30k lines of Rust):

* `outcome_learning` — OPC outcome learning, all five phases: recording, the
  maturity policy, sweep, feeders, aggregates.
* `data_room` — the OPC deal-close container and its audit log
  (access/disclosure/custody stores).
* `bots` — the scoped chat-bot runtime, bot config store, and the auth HITL
  broker.
* `execution_panel` — the projector, runtime store and v3 adapter, plus
  `plan_attribution` (which approved-plan step each run activity row belongs
  to, read back from the run's `step.started` / step-outcome events; see
  `docs/archive/plans/2026-10-01-plan-step-progress.md`).
* `retraction` — when a claim is retracted, raises a replacement or
  revocation event against every data room still serving a revision that
  carries it (the served half; `corrections_api` covers what was sent).

Production dependencies keep `magician/test-fixtures` disabled. This crate's
`test-fixtures` feature forwards it and development dependencies enable it for
tests, so a test helper does not pull the monolith's fixture modules into
debug runtime builds.

Outcome-learning cohorts consume the shared outward-act disclosure contract.
Email disclosures keep `resource_effect: None`; calendar resource metadata is
optional and does not change the email fixtures or their maturity policy. The
feeder regressions cover that contract in the all-targets check.

## Bot spawning stays on `posix_spawn`

`ManagedBot` builds its child command in a pure step (`build_bot_command`) and
spawns it separately. The program, often a bare interpreter name, is resolved
to an absolute path with `runtime_core::process::resolve_program` against the
scoped PATH the child receives, because a bare name plus a `PATH` override
makes Rust `fork` instead of `posix_spawn`, and a forked copy of the server can
hang in macOS atfork handlers before exec. A per-bot `PATH` in `config.env` is
the last env layer and wins, so the program is resolved against it when set.
The `gws auth login`/`auth status` probes resolve the same way. Unit tests pin
the contract without starting a process.

## Managed account setup

The scoped bot supervisor owns interactive account state used by Desktop
setup. WhatsApp writes a private PNG QR plus a connected marker. Telegram runs
`tgcli auth --qr`, captures its login URL into a private scope artifact,
accepts optional 2FA over a bounded stdin seam, and verifies
`tgcli auth status` before reporting ready. Google Workspace profiles reuse
the managed `gws` status/login and expected-identity checks; before a login,
the supervisor copies the validated runtime-root OAuth client into the scope's
profile with mode 0600. Auth subprocesses are serialized, timed out, drained
and reaped; a successful login restarts a bot that was running before setup.

## Lib-side seam

`magician_v2::execution_panel` keeps only `types.rs` (the serde state
vocabulary), because `realtime_events` embeds `ExecutionPanelState` in runtime
events; the crate re-exports those types from
`magician_learning::execution_panel` beside the engines. `outcome_learning`,
`data_room` and `bots` had no lib consumers and moved wholesale.

Consumers: `magician-api` (bot_api, corrections_api, data_room_api,
data_room_reader_api, execution_panel_api, outcome_learning_api, web_api,
websocket_handler, work_modules_api) and `magician-bin` depend on
`magician-learning`.

## Stalled waits in the panel

`run.waiting_without_question` is `true` when `run.needs_attention` carries a
`waiting_without_question` escalation: the run has waited on the user for at
least 60 s and no question reached them. The row's `metadata.actions`
(`cancel`, `open_task`) are the only ways forward; cancel is
`POST /api/magician/v2/executions/{execution_id}/cancel`. A pending question
prevents a false stalled-wait flag. A delegated child's pending ask appears in
`run.needs_attention` with `metadata.delegation_child`.

Pause targets keep the canonical revision-bound HITL correlation; the storage
key stays the pause locator. Legacy pauses without a correlation keep their
original key.

## Planning activity in the panel

`debug.planning_timeline` is the task's planning activity, oldest first: the
`planexec_` rows of the `task:{task_id}` progress log (planning milestones and
each planning LLM call), read 400 rows deep (`PLANNING_TIMELINE_WINDOW`) apart
from `debug.timeline`'s newest 60, so a run does not push planning out.
Planning runs on its own V2 execution, which has no V3 event journal for
`run.activity_log` to read.

Each entry carries `cost_usd`, `tokens` (input + output) and `planning_stage`
when its metadata has them, and `llm_calls` when the row summarizes several
calls (as backfilled planning-usage rows do; see `plan-flow.md`). Readers count
`llm_calls ?? 1` calls per priced row.

## Plan steps of a run

`debug.selected_execution.step_statuses` is the run's approved plan, in plan
order (`ExecutionState.plan_id`, else the latest plan when approved). Each step
is `failed`, then `completed` (`ExecutionState`), then `in_progress`
(`current_step_id`), else `pending`. The run's `plan.step.finished` events on
the task progress log overlay `skipped` (only on a pending or in-progress step)
and the agent's `note`. A run without an approved plan gets none; nothing is
guessed.

## Delegated work in the panel feed

`run.activity_log` carries the selected execution's events **and those of its
delegated children**, each attributed to the agent that produced it.
`run.delegations` describes one group per child (`execution_id`, `agent_id`,
the child's own `status`, `entry_count`), so a client folds each delegation
into one collapsible block. Group entries by `metadata.execution_id`, never by
`agent_id`, because one agent can be delegated to more than once.

Children come from `collect_related_execution_ids`, a recursive walk of the
execution tree (`child_execution_ids` plus `active_child_execution_ids`), so
nested, repeated and finished delegations are covered. Filtering to
`selected_execution_id` alone would drop child events, and a single `agent_id`
for the whole log would attribute child rows to the parent.
