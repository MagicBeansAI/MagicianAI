# Coding-engine review guardrails

Proposal-aware gating that stops the coding engine from running verification or
follow-up / parent-continuation work while a code review / diff approval is still
open, and threads structured `CodeChangeProposal` state through continuations
instead of inferring it from freeform prose.

## Enforced invariants

### 1. Verification runs against the authoritative working tree
`run_project_checks` resolves its target dir in order: explicit `real_working_dir`
→ the latest matching `CodeChangeProposal.apply_root` (exact `execution_id` match;
without one, only when a single unique `apply_root` exists — an ambiguity guard)
→ legacy `repo_path`. It returns `real_working_dir` (+ `proposal_id` /
`proposal_status`) in success responses.

### 2. Continuation preflights unresolved review and stops early
- `run_coding_task` short-circuits a continuation turn with a structured
  `blocked_on_review` payload — instead of launching another turn — when this
  execution's latest proposal is still `Pending`.
- `run_project_checks` short-circuits with `blocked_on_review` instead of running
  stale verification when the proposal is `Pending`.
- `v2_orchestrator` keeps a parent parked in `WaitingChildren` when a delegated
  child still has an unresolved diff approval (a `Pending` proposal for that
  child's execution).

### 3. Compact structured continuation handoff
- `run_coding_task` emits a compact `latest_code_change` continuation block
  (`proposal_id`, `proposal_status`, `real_working_dir`, `review_open`,
  `terminal_success`, bounded touched-file preview). `terminal_success` is
  derived from proposal status (`Applied` only), so pending or partially applied
  reviews cannot be described as complete.
- `v2_orchestrator` persists a compact JSON delegation handoff
  (`schema: magician.delegation_continuation_handoff.v1`) per child result,
  replacing verbose freeform bullets. Delegation handoffs expose review status
  but do not carry a synthetic terminal-success flag.

### 4. Review state is scoped to the execution, and cannot deadlock
- `latest_task_proposal_handoff` scopes "the latest proposal" by `execution_id`,
  not `task_id`: children on one VibeDev task share the task dir, so a sibling's
  open review must not block an unrelated continuation.
- `force_child_failure` (timeout / no-progress / forced fail) calls
  `reject_orphaned_proposals_for_execution` **before** the terminal transition.
  A force-terminated child has no live approval channel, so its `Pending` diff
  would otherwise park the parent in `WaitingChildren` forever; a run that failed
  should not leave an approvable change.
- `review_open` is `Pending` only: a `PartiallyApplied` proposal does not block
  the parent. The `blocked_on_review` payload is success-shaped (`status: ok`,
  `all_ok: true`, `results: []`); callers must check `blocked_on_review`.

Regression tests: `latest_task_proposal_handoff_*`,
`reject_orphaned_proposals_unblocks_parent_from_review_gate`.

## Key symbols

- `run_coding_task.rs`: `latest_task_proposal_handoff`, `derive_coding_continuation_context`, `CodingContinuationProposalState`
- `run_project_checks.rs`: `authoritative_proposal_verification_context`, `ProposalVerificationContext`
- `v2_orchestrator.rs`: `latest_code_change_handoff_for_execution`, `delegation_results_have_open_review`, `reject_orphaned_proposals_for_execution`, `reconcile_waiting_children`
- `agents/runtime.rs`: `force_child_failure`
