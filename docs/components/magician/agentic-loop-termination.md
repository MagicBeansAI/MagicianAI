# Agentic Loop Termination & Refinement Bounding

Bounds "refinement overdone" in the flat loop without retiring useful
refinement.

**Related:** [`YIELD_DECISION.md`](execution/YIELD_DECISION.md) (the sole
LLM-facing terminal), [`FLAT_LOOP.md`](execution/FLAT_LOOP.md) (the
observe-decide-execute loop this bounds),
[`execution/TASK_STATE_DESIGN.md`](execution/TASK_STATE_DESIGN.md) (the
durable task-state schema).

## What this is

A run concludes when the LLM emits `yield` (see
[Yield Decision](execution/YIELD_DECISION.md)). This doc covers the
orchestrator-side **backstops** for a verification pass that keeps
re-inspecting known state instead of emitting a terminal. **Progress always
resets the streak; only egregiously redundant inspection trips the
backstops.**

1. **No-progress cutoff** (`decision.rs::try_synthesize_stuck_auto_yield`):
   fires after `NO_PROGRESS_AUTO_YIELD_THRESHOLD` consecutive read-only
   iterations.
2. **Definition-of-done auto-conclude**
   (`executor.rs::all_durable_micro_goals_resolved`): concludes as soon as all
   tracked requirements in the durable task state close.
3. **Prerequisite: durable-task-state Patch UPSERT**
   (`executor.rs::apply_outer_task_state_action`): keeps durable state
   maintained so definition-of-done can see micro-goals close.

Definition-of-done is checked each iteration, before the streak can build; the
no-progress cutoff is the backstop for runs that do not maintain durable state.

## The generic principle

> A refinement runs while it **advances or verifies** a goal, and stops when it
> produces **no new information or state**. *Progress* = a mutation, OR a
> newly-produced artifact, OR a newly-verified success criterion / closed
> micro-goal, OR a terminal/yield. Sustained absence of all of these is the
> signal to conclude.

Definition-of-done is the precise reading of "no new verified criterion"; the
no-progress cutoff is the generic reading of "no new information or state".
Neither touches the LLM's own `yield`.

## Caller-selected active-work budgets

Delegation's `timeout_secs` is a **soft active-work budget**, not a hard
cancellation timer. `depth` names the tier: `normal`, `deep`, `thorough` →
900, 2700, 5400 s (`DELEGATION_WORK_BUDGET_*` in `execution/actions.rs`; the
`delegate_to_agent` tool's `depth` description is built from the same
constants). An explicit `timeout_secs` only **extends** its tier (the
named depth, or `normal`); a lower value is raised to it
(`resolve_delegation_work_budget_secs`), because the value is model-authored
and models pick budgets too short for the work. Zero, malformed and unknown
values fail delegation preflight. With neither supplied, no budget is
invented. The legacy `constraints.coordination.delegation_timeout_secs` does
not clamp the result.

The flat loop checks the budget at every safe boundary: before requesting a
model decision; after a decision returns, before dispatching any proposed
tool, sub-goal, handover or delegation; and between calls in a multi-tool
response. A running operation finishes under its own safeguards. At the next
boundary the executor saves findings as `work_budget_partial.md`, records
`work_budget_reached`, returns a successful `goal_achieved_partial` with
time-budget metadata and enters normal result synthesis. There is no grace or
finalization timer. Local-resource queue wait and paused time are excluded;
consumed time survives pause/resume; nested frames restore the parent budget
without double-counting; a budget-stopped outcome cannot commission a
refinement pass.

Per-run token/cost grants are a separate owner control. Their
`budget_grants.json` sidecar uses the shared durable byte writer (unique
staging, parent-directory sync) and keeps the process lock and cache
invalidation. The run-budget plan
describes grant and resume semantics.

## The durable-task-state micro-goal model

Each task may carry a small JSON durable task state
(`execution/durable_task_state.rs`):

- `status`: `open` / `in_progress` / `blocked` / `completed` / `abandoned` for
  the whole task;
- `micro_goals[]`: `id`, `description`, `status` (`pending` | `in_progress` |
  `completed` | `blocked`), `evidence_refs` (non-empty when `completed`);
- `active_micro_goal_id`.

A micro-goal is **resolved** when `completed` or `blocked`. Transitions only
move forward (`ensure_forward_micro_goal_transition`).
`synthesize_durable_task_state(task_id, goal, success_criteria, agent_id)`
mints the baseline: one active `mg_initial` (`in_progress`, description =
goal), deliberately not a definition-of-done. The decider emits
`task_state_action` envelopes (`Create` / `Patch` / `Close`); `Patch` is an
UPSERT that synthesizes the same baseline when no state is loaded.

## No-progress cutoff

`try_synthesize_stuck_auto_yield` stops repeated-same-error loops before
another LLM call and bounds the successful-but-redundant case. It counts the
trailing run of read-only iterations (`iteration_is_read_only`):

- At `NO_PROGRESS_AUTO_YIELD_THRESHOLD` (10), non-task-backed runs get a
  synthesized yield. Task-backed runs enter the adversarial progress-review
  cadence (`REVIEWER_CADENCE` = 3: streak 10, 13, 16, 19; the reviewer can
  continue, redirect, converge or escalate) under a stateless hard cap.
- At the hard cap (20 read-only steps, `REVIEWER_HARD_CAP_MULT=2`) the runtime
  reserves one deterministic final-synthesis turn that forbids inspection and
  asks for the deliverable as structured yield output. If step 21 is still
  read-only, the runtime hard-concludes via `Decision::Yield` /
  `dispose_yield`.

When a task-backed completed yield has substantive text but no deliverable
artifact, the executor writes it as `task_deliverable.md` with the same
durable completion summary the Artifact V2 finalizer uses (runtime-owned, so
no authorization pause).

### What counts as read-only vs progress

`iteration_is_read_only(record)` is `true` only when the step **succeeded**
and was an inspection:

- **Known inspection tools**: `read_file`, `grep`, `glob`, `find`, `ls`,
  `cat`, `head`, `tail`, `tool_search`, `help`, `read`, plus leaves ending
  `_get` / `_list` / `_describe` / `_help` / `_status`. `*_search` and `*__run`
  are not inspection. This table belongs to the agent loop and is deliberately
  not the app bind classifier in [app-tool-bind](app-tool-bind.md).
- **Browser reads**: `browser__snapshot` / `state` / `find` / `network` /
  `console` / `help` / `eval` / `screenshot`; any other browser primitive is
  progress.
- **Shell / `bash` / `sh` / `exec` / `http` / `duckdb`**: read-only unless the
  signature carries a mutation marker (`sig_has_mutation_marker`):
  `requests.post/put/patch/delete`, `.post(` / `.put(` / `.patch(`, `urlopen(`,
  `curl -X` / `--request` / `-d` / `--data`, `wget`, `method=post|put|delete|patch`,
  `card_update/create/delete/archive`, SQL `insert into` / `delete from` /
  `create table` / `drop table` / `alter table` / `create or replace`, fs ops
  `rm` / `mv` / `mkdir` / `rmdir` / `sed -i` / `tee`, redirects (` > ` / ` >> `),
  `.write(` / write-mode opens, `git commit/push/add`, `pip install`,
  `npm install`.

Anything else resets the streak: a failure (handled by the same-class-error
detector), any typed mutating, control or terminal action (`PROGRESS_KINDS`, matched as a
substring of the kind: `yield`, `goal_reached`, `need_user_input`, `handover`,
`delegate`, `create_task`, `request_thinking`, `orchestrator`,
`spawn_sub_goal`, `write_file`, `edit_file`, `card_update/create/delete/archive`),
an unknown typed action, or a shell/http/duckdb step with a mutation marker.

### Repeated same-class failures

A failure is neither read-only nor churn (`iteration_is_repeat_churn` needs a
success), so failures keep their own trailing streak
(`trailing_repeated_failure_streak`, `StuckKind::RepeatFailure`): consecutive
failed iterations of one step with one error class. The step
(`failure_step_key`) is a pack call's capability (arguments may vary), or else
the whole action signature (a shell command, truncated to 47 chars), so five
*different* commands failing with the shared `command failed with exit code`
template do not trip it. At `STUCK_AUTO_YIELD_THRESHOLD` (5)
`decision_preflight` synthesizes the stuck yield without a model call. Its
blocker carries the failure and, for a bare coded error such as
`StructuralActionDenied`, the reason from the output (`Action type 'tool' is
not allowed for this agent…`). A success, another step or another class resets
the streak.

The typical case is an owner whose every decision the policy refuses: its
grants resolve to nothing (a granted skill that failed to compile), so
`derive_allowed_action_types` yields an empty list. Without this streak such a
run would repeat the refused turn, one model call each, until
`max_iterations` (4000).

**The engine's stop is not policy-checked as a model action.** For a
synthesized `Yield` flagged `decision_is_synthetic_stuck`, resolve's
structural control check skips its deny: the owner allowlist (including
`allowed_action_types`, explicit `excluded_tools: [orchestrator]` or a denied
`yield`), the policy-snapshot gate, the trust deny on `orchestrator.yield` and
the retention check. Otherwise the policy refusing the run would refuse its
stop too. Only engine code sets the flag: `decide.rs` for the
`stuck_auto_yield` operation, and the decision rail for any preflight `Yield`
(stuck stop, reviewer hard cap, reviewer escalate). An approval rule on
finishing still applies (a pause is a stop, not a spin). A model-chosen
terminal is policy-checked as usual; a model's own yield is still refused for
an owner with no allowed action types (a yield lowers to `Pack{"yield"}`,
typed `tool`).

### Disposition of a synthesized no-progress yield

The synthesized `YieldDecision`'s `completed[]` decides whether
`dispose_yield` routes to `Completed` (rule 4) or `Failed` (rule 6). Effect
wins over method, so a run that did real work is never false-failed:

- **Produced output artifacts** → listed in `completed[]` → `Completed`.
- **Read-only streak, no artifact, non-task-backed** → conclusion summary in
  `completed[]` → `Completed` (Discuss/plan runs may legitimately end
  read-only; see `pi-coding-engine-contract.md` → `plan_only`).
- **Read-only streak, no material output, task-backed** → after the final
  synthesis turn, empty `completed[]`, mined next steps kept, deliverable left
  open → `PartialSuccess`.
- **Identical-action churn, no artifact** → empty `completed[]` → `Failed`.

A `no_change` coding-engine build (`no_change:true && status:ok &&
proposal_id.is_none()`) is a terminal "already satisfied" success, not a
re-delegation trigger: the payload carries `already_satisfied:true` and a
`change-summary.md` note becomes `output_path`, so the engineer yields rule 4
Completed.

## Cross-execution no-progress guard (B14)

The in-execution cutoff cannot see a coordinator re-delegation loop (history
resets each resume; `delegate_to_agent` is progress-bearing).
`orchestrator/v2_orchestrator.rs::enforce_delegation_no_progress_guard`:

- Keyed by `root_execution_id` in an in-memory
  `DelegationRoundProgressRecord` / `DelegationRoundProgressEnvelope` that
  survives resume cycles (not a process restart).
- Each resume round signs its children: `agent_signature` (sorted
  `active_owner_agent_id`) + `goal_signature` (sorted canonicalized child
  titles, `[Delegation]` prefix stripped). Progress = either changed. After
  `MAX_NO_PROGRESS_DELEGATION_ROUNDS` (3) no-progress rounds the parent goes
  `Failed` (`Runnable → ExecutionFailed`). A fresh artifact id is not progress.
- Runs on both resume paths: `reconcile_waiting_children_and_continue` and
  `continue_parent_after_v3_child_results`.
- `no_new_proposal_rounds` resets only on a content-distinct proposal
  (`sha256(patch) + sorted(touched_files)`); after
  `MAX_NO_NEW_PROPOSAL_DELEGATION_ROUNDS` (3) it ORs into the trip. It arms
  only once the run has staged ≥ 1 proposal.

**Delegated child pauses.** A child pausing for `DiffApproval` or any
owner-answerable input (`Text`, `Password`, `Choice`, `MultiChoice`,
`Confirmation`, `ExternalAction` such as a 2FA code or CAPTCHA, `Guidance`,
`FilePath`, `Form`) stays suspended
(`delegated_waiting_for_user_should_suspend`): its HITL card is on the task,
the answer resumes the child by execution id, and the parent stays in
`WaitingChildren`. Coordinator-only decisions (`ToolAuthorization`,
`SandboxOverride`, a `DiffApproval` with no staged id) force-fail. Three sites
share the predicate: the live disposition
(`classify_delegated_child_outcome`), the receipt stamped when the pause
commits (`TerminalSettlementDescriptor::waiting_for_user_diff_approval`, a
name older than the wider set), and terminal-outbox settlement
(`accept_terminal_loop_settlement` via
`stateless_terminal_receipt_is_delegated_diff_approval`). They must agree,
because the outbox projector publishes the pause's `input.requested` later
than the live arm; a disagreement would force-fail the child before its card
exists.

**Pause publication latency.** A receipt-backed terminal (an approval or input
pause, a non-resumable end) is never projected by the run itself, since
publishing inside the run would let a fast HITL response race an unavailable
pause. The terminal-outbox projector (`magician-bin`'s lifecycle loop) is its
only publisher, and the pause is answerable only after it activates. The
projector sweeps every 30 s. `advance_once` / `advance_in_session` also wake it
(`wake_terminal_outbox_projector`, a stored `Notify` permit) after releasing
the lease of a run that ended, and the HITL respond path wakes it when it
finds the pause still staged (answering `terminal_pause_settlement_pending`,
retry 1 s). A failed pass keeps cadence-only backoff; a wake changes when a
pass starts, never what it may do.

**Delegation checkpoint supersession.** A resumed parent may delegate again;
it re-parks in `WaitingChildren` before the round's checkpoint settles
(`resume_delegation_checkpoint` accepts that), and the new round's checkpoint
is written under the same pause key.
`FullPauseStore::retire_verified_delegation_checkpoint` returns `Ok` when the
replacement is a Delegation checkpoint of the same execution whose parked
segment is exactly the segment the source resumes as
(`DurableDelegationCheckpointAuthority::resume_segment`); any other
replacement is refused. A resumed parent that asks the owner likewise shares
the storage key (`<execution>:<plan>:<step>`) with its retained checkpoint:
the `StatelessTerminalPreparation` precondition retires a same-execution
Delegation envelope whose `stateless_resume_segment` equals the incoming
pause's `stateless_parked_segment`. Otherwise the resume errors, never
consumes its handoff descriptor, and the reconcilers retry forever.

On every V3 parent reconcile, `persist_parent_delegation_summary_from_v3`
re-reads the `CodeChangeProposal` store by `task_id` and injects an
authoritative line (`applied` / `partially applied` / `pending` / `rejected`)
plus a `code_change_proposal:<id>` artifact, so the coordinator resumes
against the proposal store rather than a frozen report. There is no
"proposal = success" gate.

`continue_execution_with_orchestrator` returns an already-terminal
execution's outcome without re-running (`status ∈ {completed,failed,cancelled}`
&& `completed_at` set). `reduce_execution_terminal` finalizes the task when
`active_root == this` OR (`active_root` is None AND this is the latest root),
never when a newer execution holds the active root.

## Definition-of-done auto-conclude

`all_durable_micro_goals_resolved(ctx, executors)` is `true` when durable
state tracks **at least one** micro-goal AND every one is resolved. It is
stricter than `open_durable_micro_goals(..).is_empty()` (also true with no
state), so a task without durable tracking is never falsely concluded and a
fresh baseline never reads as done.

It is computed once per iteration and passed to `decide_next_action` as
`definition_of_done_met`. When set, the decider short-circuits before any
prompt or LLM call and returns `Decision::Completed { evidence, artifacts: [] }`
with `NativeDecisionMetadata::synthetic("definition_of_done", ...)`. The
synthetic completion is not trusted: the executor re-runs
`terminal_success_rejection` (`goal_reached_rejection_reason`, then
`open_durable_micro_goals`). On rejection it is dropped and the rejection
recorded, and `definition_of_done_met` is computed only while
`loop_protective.consecutive_goal_reached_rejections == 0`, so the next turn
goes to the model (re-issuing a bare synthetic terminal would hit the
three-strike abort).

**Resumed parents.** The task-backed evidence gate
(`goal_reached_has_sufficient_evidence`: an artifact, a
`COMPLETED:`/`BLOCKED:` partial claim, or a ≥ 40-char narrative backed by a
successful iteration) counts what the parent was resumed with.
`terminal_success_rejection` reads the children-ready summary (rendered as
`## RECENT DELEGATION RESULTS`) and adds each child's readable deliverable as a
`delegation_deliverable` artifact
(`executor.rs::resumed_child_deliverable_artifacts`), so "the child did it"
can complete without inventing an artifact.

**Explicit goal calls.** On a goal line containing run / call / execute /
invoke, `extract_explicit_goal_js_calls` turns `name(args)` tokens into calls a
successful iteration must show before `goal_reached` is accepted (it exists to
check browser `window.*()` evals). A `window.*()` token counts anywhere on
such a line; a bare `name(args)` counts only when the verb acts on it
(`directly_follows_imperative_verb`: "call the `fetch_report()` helper"). So
code named in a coding request ("add `shout(name)` that returns
`greet(name)` … and run the tests") is not a required call. Staged
file-mutation tools (`STAGED_OR_TOOL_GOAL_CALLS`) are never required.

Task-backed **clean completions** also pass:

- **Contradiction guard**: terminal evidence and text-like artifacts are
  scanned for self-reported missing deliverables
  (`durable_*_materialized=false`, "not produced", "not materialized");
  partial or blocked yields may still surface open work.
- **Goal-specific materialization contracts**: e.g.
  `harness:<agent>:agent-improvement` must successfully call `create_proposal`
  or `propose_backlog_item`.

## Every way out of the iteration body

Per-iteration work lives in the labeled block `'iteration_body` in
`run_loop/driver_inproc.rs`. A `break` runs the epilogue and starts the next
iteration; **ending the run is `return Ok(..)`**.

The 21 early exits are typed `BoundaryOutcome` values (`Advance` /
`NextIteration` / `Retry` / `PopFrame`) produced by `run_loop/phases/*.rs`:
five in `resolve.rs`, sixteen in `apply.rs` (`ITERATION_EARLY_EXITS=21`;
`PHASE_SOURCES` pins the per-file counts);
Prepare, Observe, Decide and Epilogue have none. Each carries an adjacent
`// EXIT:` note naming its variant.
`every_way_out_of_the_iteration_body_is_classified` (`run_loop/outcome.rs`)
pins the population, note/variant agreement and `DRIVER_REPERFORMED_EXITS=8`:
the resident driver re-performs exits with eight annotated
`break 'iteration_body` statements, which a worker driver replaces.

Transient failures set `BoundaryOutcome::Retry(after)` instead of sleeping
inline; the driver honours `boundary_retry_after` after the boundary
(`duration_ms` still includes the wait).
`no_phase_and_no_iteration_blocks_on_a_backoff_of_its_own` (`run_loop/outcome.rs`)
asserts that neither the block nor any phase file contains `tokio::time::sleep`,
`sleep_until`, `sleep(` or `thread::sleep`.

Design (archived): `docs/archive/plans/2026-08-25-iteration-turn-boundary-contract.md`,
`docs/archive/plans/2026-08-25-stateless-loop-design.md`.

### What the boundary carries

Anything enforcement-bearing (ceilings, counters, approvals) must be
represented at the boundary, or the enforcement is advisory.

- **Wall-clock ceiling** is `LoopProtectiveState::remaining_max_duration_ms`
  (a remaining duration, not an `Instant`): paused time does not count, and a
  ceiling already passed carries zero, so the resumed run ends at once.
- **Four restrictions** (`allowed_action_types`, `denied_capability_names`,
  `denied_tool_params`, `browser_transports`) are restored on every resume
  path, gated or not, because each treats empty as unrestricted and
  withholding them would fail open. They are in `authorization_hash`, which is
  verified only for a pause that `is_elevation()`.
- `delegation_targets` is not a restriction (a catalog; losing it fails
  closed).
- A run with a wall-clock ceiling is never `is_pristine()`, so all its pauses
  carry the protective bundle.

## File references

- `execution/durable_task_state.rs`: micro-goal schema, synthesize/patch/validate, forward-only transitions.
- `execution/agentic/executor.rs`: Patch UPSERT, `all_durable_micro_goals_resolved`, `terminal_success_rejection`, clean-completion contracts.
- `execution/agentic/decision.rs`: `NO_PROGRESS_AUTO_YIELD_THRESHOLD` (= 10), `try_synthesize_stuck_auto_yield`, `iteration_is_read_only` / `sig_has_mutation_marker`.
- `execution/agentic/run_loop/driver_inproc.rs`: `'iteration_body` and eight re-performed breaks.
- `execution/agentic/run_loop/outcome.rs`: `BoundaryOutcome`, `ITERATION_EARLY_EXITS=21`, `DRIVER_REPERFORMED_EXITS=8`.
- `execution/agentic/run_loop/phases/{resolve,apply}.rs`: the 21 typed early exits.
- `orchestrator/v2_orchestrator.rs`: B14 `DelegationRoundProgressRecord` / `enforce_delegation_no_progress_guard`.
