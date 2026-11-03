# Changelog

All notable changes to `magician-learning` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

### 2026-11-02 — 0.1.9

- The execution panel's failed tool rows say why: `summary` is the error without the runtime's dispatch prefixes and `metadata.error` carries it as journaled.
- `BotManager::google_workspace_account` returns a Google Workspace bot's profile directory, expected email and project-id override.

### 2026-10-23 — 0.1.8

- Execution activity pairs context-preparation rows by phase and shows their duration and partial or cancelled outcome.
- Bots run in their own process groups: stopping a bot ends what it forked, a group left by a previous run is reaped, all bots stop together under one grace period, and an unfinished Google sign-in times out after 10 minutes.

### 2026-10-20 — 0.1.7

- Execution panel: an unsettled `llm.requested` / `tool.started` row is `status: "running"` until its call settles or the run moves past it; LLM rows carry `llm_call_id`, `trace_id`, `profile`, tool rows `call_id`, `started_at_ms`.

### 2026-10-20 — 0.1.6

- Execution-panel clarification and pause targets carry the server's HITL respond handle.

_Current development version: `0.1.7`._

- A deleted workspace's bots are stopped and never respawned or issued tokens.

_Current development version: `0.1.4`._

- Show planning milestones, metered model calls and plan step states in the execution panel; distinguish pending owner questions from undelivered waits.

### 2026-09-20 — 0.1.2 — Supervisor-owned account pairing

- The bot supervisor owns Telegram QR/2FA, WhatsApp pairing and scoped Google OAuth client setup used by Desktop.

### 2026-09-11 — 0.1.1 — Execution panel follows delegated children

- The execution panel merges events from delegated children into one chronological feed.
- The `test-fixtures` feature no longer leaks into production builds.
