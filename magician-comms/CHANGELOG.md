# Changelog

All notable changes to `magician-comms` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

- Gate memory connection publication through reviewed Decision Model classifications.
- Recheck live connection sources before and after bounded background reference replay.

_Current development version: `0.1.7`._

- Add automatic Channel Assist compaction with coordinated reads/writes, connection resource budgets and batched annotation reads. Start optional workers after HTTP readiness.

- Preserve account purposes in channel observation and tighten the
  authenticity and timestamp checks used when ingesting verification mail.

### 2026-09-15 — A deleted workspace stays deleted (0.1.5)

- Historical attention bootstrap discovery admits a scope only while its
  directory still exists. Its three sources — the mail store, the resurfacing
  store and the attention learning store — are all databases, and a row there
  records what a scope once did rather than proving it still exists. A deleted
  workspace was therefore rediscovered on the next boot, `initialize` wrote its
  checkpoints, the scope directory was materialized again, and every later pass
  that scans `scopes/` then treated it as live. Measured: 44 disposable
  fixture-eval workspaces returned after being removed, kept alive by 225
  checkpoint rows and 134 resurfacing candidates. The default scope is still
  admitted without a directory, because on a fresh install this pass is what
  prepares it.

### 2026-09-13 — 0.1.3 — Memory lifecycle review and repeatable journeys

- Background review and owner answers reconcile revisioned memories through shared ingress. Clarifications retain source versions and survive restart/replay.
- Frozen lifecycle fixtures exercise natural capture, correction, recall and durable free-text answers through production services. The [lifecycle lane](../docs/components/magician/memory-lifecycle.md#focused-qualification) now runs from Evals with report history.

### 2026-09-11 — 0.1.2 — Attention recovery, lane corrections, distillation retirement

- **Attention:** canonical-history pruning is scheduled and batched off the page path with a 60 s stale-serve window; rank recompute keeps its retry causes, reconstructs missing decisions and re-evaluates legacy dead jobs once; rows carry `due_text`/`due_at` and the new `OwnerWork` / `WrongLane` corrections.
- **Semantics:** backfill uses JSON-schema constrained output, skips already-covered rows without a model call, ignores Task and Memory sources, and keeps a good envelope over a same-producer invalid refresh.
- **Distillation:** work older than the rolling history window is retired in bounded batches independent of model availability, pending/retryable queries take a date floor, and health exposes `distill_queue` counts.
- **Storage:** attention maintenance no longer gates on a storage snapshot, so `/storage` inventory is never blocked by it.

### 2026-09-06 — 0.1.1 — Checkpoint deferral is not a warning

- The mail-assist store's throttled `CHECKPOINT` is refused by DuckDB while
  another write transaction is active. The WAL already holds the committed
  bytes and the next throttled attempt compacts, so the deferral is logged at
  debug level instead of surfacing as a warning on every boot.
