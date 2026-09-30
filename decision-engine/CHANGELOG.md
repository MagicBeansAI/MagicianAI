# Changelog

## [Unreleased]

- Bind shared-chunk questions to canonical per-item states, including pack `item.field` paths.

- Withhold unqualified destructive memory outputs and add bounded shared-context chunk execution with per-item recovery.

- Reject conflicting thresholds for route profiles sharing the same adapter/model identity before saving.

- Persist revision-guarded local/cloud primary and backup mappings with active-route status.

- Add revision-guarded routing settings APIs with atomic persistence and separate saved/active state.

- Give background classifications separate queue and inference budgets while preserving foreground deadlines and provider health accounting.

- Pass classification deadlines into local fallback routing while preserving shadow behavior and per-model qualification.

- Serve contract-v4 bounded memory batches with partial results, shared admission, per-output qualification and an explicit operator override that retains confidence checks.

- Serve compact planner catalogs, capacity-aware model fallback, and provider-health results on contract v3.
- Serve contract-v3 native and harness chat proposals through the shared action path.

_Current development version: `0.4.2`._

- Serve shared tool selection and validated planner fallback on contract v2, remove `/v1/step`, and optimize MLX kernels in debug builds.
