# Changelog

All notable changes to the canonical Magician event taxonomy are documented in
this file.

## [Unreleased]

- Expose memory decision completion and deferral events to clients.

_Current development version: `0.1.5`._

- `chat.engine.updated` (`RuntimeAgentEventType::ChatEngineUpdated`): the process-wide chat engine changed.

### 2026-09-06 — 0.1.3 — Typed Task Recipe lifecycle events

- Register all ten recipe replay lifecycle rows and shared
  `RuntimeAgentEventType` identifiers: start, step completion/failure, auth
  healing, transport downgrade, approval request/resolution, fallback,
  completion, and recompilation.
- Route the recipe observer through the typed identifiers without changing
  wire names. Synchronize the generated Unified UI taxonomy and pin exhaustive
  observer mapping plus private-value exclusion in regression coverage.

### 2026-08-29 — media and local-transcript telemetry (0.1.2)

- Register media configuration and voice-session mint, rotation, surface,
  reconnect, and compaction events.
- Register local-transcript state, queue, turn, and fallback events. These are
  content-free operator telemetry; queue pressure, provider fallback, and
  exhausted reconnects carry warning severity.
- Synchronize the generated Unified UI taxonomy mirror and its source stamp.
