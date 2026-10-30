# Changelog

All notable changes to `magician-comms` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

- 0.1.13: Give memory connection reviews their full budget with clean deferral, rate-limit repeated failure rows, and hold router-dropped resurfacing candidates.

- Memory connection reviews get their full 60 s budget (the 20 s outer timeout cut gate decisions and still charged them); a review that ends before any decision is `deferred`, released from the 3/hour budget and capped separately (3/hour, one per pass); the memory lifecycle pass is recorded as its own `memory_lifecycle` run kind instead of inflating `memory_connections` durations.
- Repeated `memory_lifecycle`/`memory_connections` failures write one run row per 15 min per scope unless the message changes; each deferred memory connection review logs its root cause (debug, warn on a repeat within the hour).
- Rollback note: earlier binaries cannot parse the new `deferred` connection state; its connection pass fails each time the cursor reaches a candidate holding such a row, until that row is removed.
- Fixed: the resurfacing curator holds router-dropped candidates (handled, dismissed, suppressed, stale, owned by a Follow-up) out of its review window until their content changes or the hold expires, and writes each pass's funnel events in one batch keyed on content, so repeated drops no longer starve Worth a look or flood the funnel.

### 2026-10-21 — 0.1.12

- Resurfacing note items open by the canonical relative link `/notes?path=…&workspace=…`.

### 2026-10-20 — 0.1.11

- `POST /api/magician/v2/chat/memory/duplicates/fold` (owner): plans, and with `{"apply": true}` applies, folding near-duplicate user knowledge records through the memory lifecycle (dry run by default).

### 2026-10-18 — 0.1.10

- The evidence, feedback and pattern bridges write user memory once per pass instead of once per group; under the user-memory bounds' `enforce` mode their rows go to lane files, where a day's slices are combined.

### 2026-10-04 — 0.1.9

- Channel Assist releases idle databases safely and reopens them on demand.
- The mail assist store never recreates a deleted workspace.
- Relentless run and guidance journals are excluded from generic retention.

### 2026-09-15 — A deleted workspace stays deleted (0.1.5)

- Fixed: deleted workspaces were rediscovered from historical attention data on the next boot; only workspaces whose directory still exists (plus the default) are admitted.

---

Older entries: `docs/archive/changelogs/magician-comms.md`
