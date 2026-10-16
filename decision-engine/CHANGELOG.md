# Changelog

## [Unreleased]

_Current development version: `0.4.5`._

- Ship magician-decision 0.4.9: the unused harness-chat planner mode is removed.

_Version `0.4.4`._

- Ship the planner scope check that sticky agent plans rely on.
- Ship magician-decision 0.4.8: chat-native planner rounds keep chat's own system prompt instead of the JSON-plan prompt.

_Version `0.4.3`._

- Ship magician-decision 0.4.7: chat-native planner prompts stay flat across calls.
- Bounded shared-context chunk execution with per-item recovery, bound to canonical per-item states; unqualified destructive memory outputs are withheld.
- Revision-guarded routing settings APIs: atomic local/cloud primary and backup mappings, separate saved and active state, and rejection of conflicting thresholds for the same adapter/model.
- Background classifications get their own queue and inference budgets; classification deadlines reach local fallback routing.
- Serve contract-v4 bounded memory batches with partial results, per-output qualification and an explicit operator override.
- Serve compact planner catalogs, capacity-aware model fallback, provider-health results, and native and harness chat proposals on contract v3.

_Current development version: `0.4.2`._

- **Breaking:** serve tool selection and validated planner fallback on contract v2; `/v1/step` is removed. MLX kernels are optimized in debug builds.
