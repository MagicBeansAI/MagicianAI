# Changelog

All notable changes to `magician-surfaces` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

### 2026-09-13 — 0.1.2 — Lifecycle Evals with immutable report history

- Lifecycle runs validate bounded options, pass a unique run ID to Make and retain options in backward-compatible history records. Reports resolve only to that run, including failures.
- Standalone evaluator calls remain unknown in task-ledger costs; their reports carry separate usage estimates. See the [Evals contract](../docs/components/magician/eval-lanes.md#memory-lifecycle-runs).

### 2026-09-06 — 0.1.1 — Thinking-map replay breaks its tie with the snapshot

- When an event log carries two candidates for the same sequence, replay
  prefers the one the persisted snapshot came from. A log that is still
  ambiguous is quarantined beside its map (`repair-quarantine.json`) and
  stops erroring on later boots.
