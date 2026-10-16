# Changelog

All notable changes to `magician-media` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-10-15 — 0.1.8

- Keep confirmed offer bindings and reply Work provenance across negotiation restarts and rescheduling.

### 2026-10-04 — 0.1.7

- Fixed a stack-size issue in the voice orchestrator.
- Bound cancellable local speech evidence and mark unsupported figures in voice transcripts.

### 2026-09-18 — 0.1.2 — Realtime profile fields

- The hands-free realtime voice profile carries the new thinking-level, tool-result scheduling and display-order fields, unset.

### 2026-09-11 — 0.1.1 — GPT Live 1, tool-using voice delegation, Gemini Transcribe

- Voice runs GPT Live 1 alongside GPT Realtime and Gemini Live, delegates to the full tool-using chat turn, and honours a pinned voice from Settings.
- Fixed: personal voice no longer fails closed on an unattested owner credential.
- Add Gemini file and streaming speech-to-text providers.
