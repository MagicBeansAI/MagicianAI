# Changelog

All notable changes to `magician-learning` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

_Current development version: `0.1.5`._

- A deleted workspace's bots are stopped and never respawned or issued tokens.

_Current development version: `0.1.4`._

- Show planning milestones, metered model calls and plan step states in the execution panel; distinguish pending owner questions from undelivered waits.

### 2026-09-20 — 0.1.2 — Supervisor-owned account pairing

- The bot supervisor owns Telegram QR/2FA, WhatsApp pairing and scoped Google OAuth client setup used by Desktop.

### 2026-09-11 — 0.1.1 — Execution panel follows delegated children

- The execution panel merges events from delegated children into one chronological feed.
- The `test-fixtures` feature no longer leaks into production builds.
