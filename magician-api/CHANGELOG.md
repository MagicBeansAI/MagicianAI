# Changelog

All notable changes to the Magician API project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-10-15 — 0.3.44

- `GET /work/live`, `POST /work/follow` and `DELETE /work/follow/{item_id}`; `POST /v3/tasks` takes an optional `source`.
- Authenticated `GET /runtime/client-config` returns only the Clarity project id.
- `GET/PUT /settings/super-god-mode` (debug): on for 1 hour, until turned off, or off; config reload applies `execution.super_god_mode` live.

### 2026-10-15 — 0.3.43

- Add scoped delivery correlation, confirmed calendar lifecycle guards, inbound reply Work routing and paged follow-through reads.

### 2026-10-12 — 0.3.42

- Work budget grants can add active time, extend exhausted rounds and keep cumulative usage.
- Config reload applies the execution network policy live.
- God mode endpoints (owner only): global setting, grant reset and per-chat override.
- Deleting a workspace also stops its bots, cancels its wakes, revokes its sessions, tokens and grants, and unpairs its devices; nothing can recreate a deleted workspace afterwards.
- Boot admits system packages for live workspaces only; readiness waits only for each principal's default workspace, with the rest admitted in the background. Deterministic boot-review failures are not retried until inputs change.

### 2026-10-12 — 0.3.41

- App authoring catalogs prewarm before readiness.
- New `archived=exclude|include|only` filter on chat sessions, UI threads, tasks and VibeDev projects and runs (with pagination where noted); unknown values return 400.
- Detached task and HITL resumes run with bounded stack headroom.

### 2026-10-12 — 0.3.40

- LLM call queries build only the tables they name and use bounded DuckDB threads.

---

Older entries: `docs/archive/changelogs/magician-api.md`
