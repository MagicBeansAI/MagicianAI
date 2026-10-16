# Changelog

All notable changes to the canonical Magician event taxonomy are documented in
this file.

## [Unreleased]

### 2026-10-12 — 0.1.8

- Register `chat.turn.stopped` for turns the chat watchdog stops.

### 2026-10-04 — 0.1.7

- Publish the typed agentic action-start event for client progress.

### 2026-09-06 — 0.1.3 — Typed Task Recipe lifecycle events

- Register the ten recipe replay lifecycle events with shared typed identifiers; wire names are unchanged and the Unified UI taxonomy is synced.

### 2026-08-29 — media and local-transcript telemetry (0.1.2)

- Register media configuration and voice-session lifecycle events.
- Register content-free local-transcript state, queue, turn and fallback events, with warnings for queue pressure, fallback and exhausted reconnects.
