# Changelog

All notable changes to the canonical Magician event taxonomy are documented in
this file.

## [Unreleased]

- 0.1.12: Register `task.answer_progress` for final-answer writing, checking and revision.

### 2026-10-23 — 0.1.11

- Registers `context.retrieval.started` / `context.retrieval.completed` (Pipeline, Info, not user-relevant).
- `AgenticProviderOutage` (agentic, warn): a decision call is waiting out a provider outage (`attempt`, `max_attempts`, `next_probe_ms`, `error_class`), with a `recovered` marker when it clears.

### 2026-10-20 — 0.1.10

- Register `chat.turn.state_changed` (`Execution`, `Info`, not rendered in chat): a chat turn started or settled.

### 2026-10-20 — 0.1.9

- Register `LLMRouted` / `llm.routed` (`Llm`, `Info`, not user-relevant): the router's provider/model choice for an in-flight call.

### 2026-10-12 — 0.1.8

- Register `chat.turn.stopped` for turns the chat watchdog stops.

### 2026-10-04 — 0.1.7

- Publish the typed agentic action-start event for client progress.

---

Older entries: `docs/archive/changelogs/magician-event-taxonomy.md`
