# Changelog

All notable changes to `magician-comms` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-10-04 — 0.1.9

- Channel Assist releases idle databases safely and reopens them on demand.
- The mail assist store never recreates a deleted workspace.
- Relentless run and guidance journals are excluded from generic retention.

### 2026-09-15 — A deleted workspace stays deleted (0.1.5)

- Fixed: deleted workspaces were rediscovered from historical attention data on the next boot; only workspaces whose directory still exists (plus the default) are admitted.

### 2026-09-13 — 0.1.3 — Memory lifecycle review and repeatable journeys

- Background review and owner answers reconcile revisioned memories; clarifications survive restart.
- Lifecycle fixtures for capture, correction, recall and answers run from Evals with report history.

### 2026-09-11 — 0.1.2 — Attention recovery, lane corrections, distillation retirement

- Attention: history pruning moves off the page path; rank recompute recovers failed jobs; rows carry due dates and new owner-work and wrong-lane corrections.
- Semantics backfill uses constrained output, skips covered rows and ignores Task and Memory sources.
- Old distillation work is retired in bounded batches; health shows queue counts.
- Storage inventory is no longer blocked by attention maintenance.

### 2026-09-06 — 0.1.1 — Checkpoint deferral is not a warning

- A deferred mail-store checkpoint logs at debug instead of warning on every boot.
