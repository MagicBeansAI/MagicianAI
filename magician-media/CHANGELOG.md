# Changelog

All notable changes to `magician-media` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

_Current development version: `0.1.5`._

- A voice call's `delegate_to_chat` and hands-free chat turns think with the call's client composer engine.

- Grok Voice Transcribe 2.0 is a selectable dictation speech-to-text engine.

- Grok TTS and Gemini 3.8 Flash / Flash-Lite TTS are selectable speech voices.

### 2026-09-18 — 0.1.2 — Realtime profile fields

- The synthesized hands-free `RealtimeVoiceProfile` carries the new `thinking_level`, `tool_result_scheduling`, and `display_order` fields unset.

### 2026-09-11 — 0.1.1 — GPT Live 1, tool-using voice delegation, Gemini Transcribe

- **Voice:** the orchestrator runs GPT Live 1 (`openai_live`) beside GPT Realtime and Gemini Live, routes `delegate_to_chat` through Magician's full tool-using chat turn on the call's active-run token, overlays a Settings-pinned speakable voice on mint and rotation, and no longer fails personal voice closed on an unattested owner credential (`can_attest_selected_profile`). Live session instructions stay on `voice_live_mouth_system`; Realtime/Gemini still use the chat outer-loop; meeting-join Presto uses `voice_meeting_presto_system`.
- **Speech:** `gemini_transcribe` (Interactions API file STT) and `gemini_live_transcribe` (streaming STT) providers.
