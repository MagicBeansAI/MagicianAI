# Changelog

## [Unreleased]

- 0.1.5: Pin task-output synthesis 1.2.0 to preserve supported findings during grounding repairs.

### 2026-10-24 — 0.1.4

- Prompt pins `CHAT_OUTER_LOOP_SYSTEM` 0.0.8, `AGENTIC_DECISION` 1.3.12, `AGENTIC_DECISION_SYSTEM` 1.0.10, `TASK_SUMMARY_SYSTEM` 1.0.1 and `TASK_USER_CLAIM_JUDGE` 1.2.1: ask only for essential missing inputs, findings before gaps, and requester targets kept apart from verified facts.

### 2026-10-20 — 0.1.3

- `versions::EXECUTION_OUTPUT_SYNTHESIZE` 1.2.0 and `versions::TASK_USER_OUTPUT_SYNTHESIZE` 1.1.0: the synthesis prompts report the bundle's `run_effects` (the run's recorded changes, runs and readbacks across all segments) as done. `TASK_USER_CLAIM_JUDGE` stays 1.2.0.
- `versions::EXECUTION_OUTPUT_SYNTHESIZE` 1.1.0: the execution-output synthesis prompt reports the bundle's tracked `micro_goals` (completed ones, and each blocked one with its `blocked_reason`).

### 2026-10-20 — 0.1.2

- HITL respond handle: `hitl::HitlRespondHandle` and its one builder `hitl_respond_handle` name the exact respond call (URL, source, input type, ids, options, soft-success statuses) for every answerable HITL item; `HitlOpenTarget` carries it as `respond`.

### 2026-10-04 — 0.1.1

- Version generic material-input clarification and claim-review prompt contracts.
