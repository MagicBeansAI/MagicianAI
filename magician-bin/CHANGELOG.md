# Changelog

## [Unreleased]

### 2026-10-15 — 0.2.29

- Routes `/work/live` and `/work/follow`; a daily job deletes failure rows older than a week.

### 2026-10-15 — 0.2.28

- Start the scoped Work bridge and task service with delivery and scheduling.

### 2026-10-12 — 0.2.27

- Prewarm app authoring catalogs for default workspaces before HTTP readiness.
- God mode: boot wires its approver and routes its settings and per-chat endpoints.
- Release idle workspace database owners periodically; shutdown waits for this before storage teardown.

### 2026-10-09 — 0.2.26

- Start attention-learning storage maintenance after HTTP readiness.

### 2026-10-09 — 0.2.25

- Retire deleted workspaces' remaining state (host files, HITL rows and tokens, then attention data in the background after readiness).
- Faster readiness: only each principal's default workspace is admitted before HTTP readiness; others follow in the background (previously several minutes of `service_starting`).
- The terminal-outbox projector wakes when a run is released instead of waiting for its sweep.
- **Removed:** `seal-stateless-loop-cutover` is accepted and does nothing.

---

Older entries: `docs/archive/changelogs/magician-bin.md`
