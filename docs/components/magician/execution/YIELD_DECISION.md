# Yield Decision

`yield` is the sole LLM-emitted terminal outcome-report tool for the agentic
loop. `goal_reached` / `cannot_proceed` are not catalog tools (emitting them is
an unknown capability); native lowering aliases them to `Decision::Yield` only
for persisted-history replay.

**Design:** `2026-05-27-yield-decision-migration.md`.
**Deferred:** `2026-05-28-decision-yield-unification.md`
(`Decision::Completed` / `Decision::Failed` still exist as synthetic variants).

There is no inner-loop yield catalog or inner-loop system prompt
(`primitive_dispatch` replaces the nested loop).

## What it is

The LLM emits structured fields — `summary`, `completed`, `open`, `blockers`,
`artifacts`, `next_step_hint` — and `dispose_yield` computes Completed /
PartialSuccess / Failed / RetryTransient.

The `Decision::Yield` handler maps Completed and PartialSuccess to
`AgenticOutcome::Success`, Failed to `AgenticOutcome::Failed`. `Success`
carries `completion: CompletionKind` (`Full` | `Partial`) and the yield's
`open[]` verbatim, so the kind survives every layer above the yield: the
reducer writes `completion_kind` / `open_items` onto `ExecutionState` and
`TaskState`, an explicit delegation root copies its child's, and any
aggregate takes the weakest kind among its parts (`min()`). PartialSuccess
also builds `partial_findings.md` and persists
`outcome_type = "goal_achieved_partial"` so memory classifies the episode
correctly.

`need_user_input` is a separate primitive. Yield is strictly terminal. For
interactive asks where the conversation continues, use `need_user_input`.

## YieldDecision shape

Required `summary: String`. Lists: `completed`, `open`, `blockers`
(`YieldBlocker { kind, description }`), `artifacts`. Optional:
`next_step_hint`, `self_classification` (advisory only). Browser handoff:
`keep_browser_cdp_connection_alive` (alias `keep_browser_session_alive`) and
`keep_browser_window_open`.

`YieldBlockerKind`: `Auth`, `DataMissing`, `Permission`, `Transient`,
`External`, `Other`. Only `Transient` is `is_transient()`.

## Disposition logic

`dispose_yield` first-match precedence:

1. `completed` **and** `open` both non-empty → `PartialSuccess`, retaining any
   blockers as caveats rather than discarding completed work.
2. Non-empty `blockers`, all transient → `RetryTransient`.
3. Non-empty `blockers`, any non-transient → `Failed`.
4. `open` empty AND (`artifacts` or `completed`) non-empty → `Completed`.
5. Everything empty → `Failed { reason: "yield with no progress, no blockers, no artifacts" }`.
6. `open` non-empty, `completed` empty, no blockers → `PartialSuccess` (the
   evidence gate can still reject a hollow success).

`self_classification` is **not consulted**.

## Success Guards

`terminal_success_rejection` is shared by the `Decision::Completed` and
`Decision::Yield` success arms (one soft-reject source of truth):

- task-backed success needs material evidence, a material produced artifact, or
  a substantive successful iteration with narrative evidence. Raw
  `tool_inline_result` / `*_inline_*` captures do not satisfy the durable
  deliverable contract;
- evidence that the deliverable was not produced, remains blocked, has
  insufficient evidence, or is not audit-ready is rejected as success;
- durable micro-goals, when present, must be completed or explicitly blocked.

The synthetic no-progress auto-yield follows the same contract. Ad-hoc read-only
research may conclude after repeated inspection. Task-backed work with a
read-only streak and no material artifact becomes `PartialSuccess` with an open
deliverable instead of `goal_achieved`.

## What writers of prompts and personas should do

1. **Use `yield` exclusively** for terminal reports.
2. **Do not put user-question fields in yield** — use `need_user_input`.
3. **Populate `completed[]` for any work that landed.** Partial-success depends
   on it, and completed+open wins over blockers.
4. **Use the taxonomy:** `auth` (missing/expired/wrong-scope tokens);
   `permission` (HTTP 403, scope mismatch — retry won't help); `data_missing`;
   `transient` (rate limit, brief outage); `external` (upstream bug/missing
   feature); `other`.

The decision prompt version is `constants::versions::AGENTIC_DECISION_SYSTEM`.
Control-tool descriptions and retry feedback advertise only `yield`.

## Internal-only Decision variants

- **`Decision::Completed`** — `text_terminal_envelope` when the LLM returns
  text-only (and text terminals are allowed), and definition-of-done
  auto-conclude when every durable micro-goal is resolved.
- **`Decision::Failed`** — synthetic failure terminal. The apply-phase arm still
  carries owner-stack yield-back, AskUser escalation, and the partial-success
  safety net. The live model terminal is always `Decision::Yield`; evidence/success
  gates are shared with it.

## A partial is judged as a partial

`terminal_success_rejection` runs the completion-evidence gates for
`Completed` **and** `PartialSuccess`, but the deliverable-gap scan
(`task_backed_goal_evidence_has_deliverable_gap` — "was not produced",
"remains blocked", "insufficient evidence") applies **only to a clean
completion claim**.

Why: those phrases are the vocabulary of an honest `open[]`; judging a partial
by them bounces accurate partials until the three-strike cap turns them into
total failures with no output.

A partial has its own bar, already enforced by `dispose_yield`: non-empty
`completed` **and** `open`. The run must show finished work before it may
report a gap. `goal_reached_evidence_has_incomplete_markers` keeps its own
`COMPLETED:`/`BLOCKED:` escape hatch for the same reason.

## Repeating a failed call

`repeated_failed_action_rejection` bounces a `Pack` call whose capability and
arguments (minus the runtime's `__`-prefixed bookkeeping keys) already failed
in this run. `LoopDetector` does not cover this: it keys on action
fingerprints across the history, not on failure.

The guard stands down after `MAX_REPEAT_FAILED_ACTION_REJECTIONS` bounces and
lets the call through to fail normally, so it can never deadlock a model that
insists. Its message, and the `failure_means` field every failed iteration
carries into the decision payload, name the distinction the failure actually
holds: **a failed tool call is evidence about the call — its arguments, target
or transport — not about whether the information exists.** Without that, a
model reads its own failed fetch as a fact about the world.

## Related files

- `execution/agentic/yield_decision.rs` — types + `dispose_yield`
- `execution/agentic/native_lowering.rs::lower_yield` — plus `goal_reached` / `cannot_proceed` aliases
- `execution/agentic/native_catalog.rs::build_yield_tool`
- `execution/agentic/run_loop/phases/apply.rs` — Yield / Completed / Failed arms
- `execution/agentic/executor.rs::terminal_success_rejection`
- `data/magician_v2/prompts/agentic_decision_system_v*.json`
- `execution/agentic/executor.rs::repeated_failed_action_rejection`
- `execution/agentic/decision.rs` — `failure_means` on failed iteration records

### Governed app call failures

An app call that fails without an admitted result sends only a fixed host failure
code and contract-repair guidance to the next model turn. Raw error prose, output
and arbitrary fields are discarded. Deferred calls likewise carry only the exact
content-free marker. These messages do not claim a successful result or consume
a result-checkpoint sequence entry. Checkpoint-bearing results retain exact byte,
authority and replay-order validation, including after resume. This lets a model
correct an invalid store query. Generic older-history prose is omitted for
governed app turns because it can contain raw exceptions. App result data reaches the model
through the checkpoint-validated tool messages.

Shared App failed-call guidance also names the canonical store query sort
values (`ascending`, `descending`) and predicate arena (`root`, `nodes`). The
query tool itself derives these shapes from the store's wire types; error
recovery does not disclose raw exception text or infer missing data from a
failed query.
