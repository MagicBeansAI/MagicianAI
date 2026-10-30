# Changelog

## [Unreleased]

_Current development version: `0.4.10`._

- Ground complete tool calls from coherent result rows and shared schemas; record precise selection and review diagnostics.

_Version `0.4.9`._

- **Removed:** the harness-chat planner mode and its reply schema; no host sends it.

_Version `0.4.8`._

- The planner asks for the full step sequence to the goal, with bindings, so later steps run without re-planning.
- Stored plans made under different task instructions are rejected.
- Fixed: chat-native planner rounds use chat's own system prompt instead of the JSON-plan prompt, which had caused frequent tool re-selection.

### 2026-10-04 — 0.4.7

- Fixed: the chat-native planner prompt no longer resends evidence and observations on every chained call.
- Bundle the claim-support decision pack for bounded chat claim checks.
