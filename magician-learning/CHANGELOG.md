# Changelog

All notable changes to `magician-learning` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

_Current development version: `0.1.2`._

### 2026-09-20 — 0.1.2 — Supervisor-owned account pairing

- The bot supervisor owns Telegram QR/2FA, WhatsApp pairing readiness, and scoped Google OAuth-client placement used by Desktop setup.

### 2026-09-11 — 0.1.1 — Execution panel follows delegated children

- **Execution panel:** the V3 adapter merges events from every delegated child, settled ones included, into one chronological feed instead of stopping at the delegation.
- **Build:** the `test-fixtures` feature forwards to `magician/test-fixtures` instead of pulling it into production dependency graphs.
