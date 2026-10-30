# Changelog

All notable changes to `magician-media` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

- 0.1.11: GPT-Live calls get ready without waiting for the chat prompt's memory lookup.

- GPT-Live calls get ready without waiting for the chat prompt's memory lookup: Live installs its own prompt and only needs the catalog snapshot that authorizes `delegate_to_chat`, which used to hold `session.ready` for up to 8 seconds.

### 2026-10-24 — 0.1.10

- Realtime calls fix their chat before the provider session starts and keep it through rotation, so voice is charged to the chat it began in; a chat created for a call that fails to start is deleted again.

### 2026-10-23 — 0.1.9

- The voice orchestrator reports token usage so voice turns count toward the owner spend envelope.

### 2026-10-15 — 0.1.8

- Keep confirmed offer bindings and reply Work provenance across negotiation restarts and rescheduling.

### 2026-10-04 — 0.1.7

- Fixed a stack-size issue in the voice orchestrator.
- Bound cancellable local speech evidence and mark unsupported figures in voice transcripts.

### 2026-09-18 — 0.1.2 — Realtime profile fields

- The hands-free realtime voice profile carries the new thinking-level, tool-result scheduling and display-order fields, unset.

---

Older entries: `docs/archive/changelogs/magician-media.md`
