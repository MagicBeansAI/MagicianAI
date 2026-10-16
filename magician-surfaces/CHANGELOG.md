# Changelog

All notable changes to `magician-surfaces` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-10-04 — 0.1.3

- Keep task runtime recursion compatible and canonicalize thinking maps to parent trees.

### 2026-09-13 — 0.1.2 — Lifecycle Evals with immutable report history

- Lifecycle runs validate options, get a unique run ID and keep options in backward-compatible history; reports resolve only to their run.
- Standalone evaluator costs stay separate from task-ledger costs.

### 2026-09-06 — 0.1.1 — Thinking-map replay breaks its tie with the snapshot

- Ambiguous thinking-map replay prefers the snapshot's candidate; logs that stay ambiguous are quarantined instead of failing every boot.
