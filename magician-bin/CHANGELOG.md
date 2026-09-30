# Changelog

## [Unreleased]

- Register the Decision Model routing settings proxy in the host API.

- Package shared-decision reliability fixes and scoped provider-health notifications.
- Serve the Decision Engine mode setting with contract-v3 chat integration.

_Current development version: `0.2.20`._

- Connect the HTTP authentication gate to the app host's shared live-session verifier for embedded page assets.

- Register the `memory_data` app memory reader at boot.

- Register the `tasks_data` and `notes_data` app read binders at boot.

- Relay chat-engine changes to open clients as `chat.engine.updated`.

- Serve startup status and a loading page before full initialization; preserve request authentication, WebSockets and streaming when the application becomes ready.
- Defer optional startup jobs and join owned work during shutdown, including stops during initialization.

- Package the Pi chat and agentic harness wiring and its Plane extension with
  the server runtime.

### 2026-09-20 — 0.2.12 — Setup routes and public OAuth callback origin

- Mount component planning/install and bot setup routes, and initialize MCP OAuth callbacks from the reviewed public origin.

### 2026-09-18 — Preflights answered by the CORS layer

- Both API scopes wrap `magician_v2::cors::api_cors_middleware`; the local
  header middleware, `cors_preflight_handler` and the two `/{tail:.*}`
  `OPTIONS` catch-alls are gone. A catch-all route never reached a nested
  scope, so its preflight 404ed and every cross-origin bearer call into it
  failed as an opaque network error.

### 2026-09-13 — Local-generation settings routes

- Mount `GET`/`PUT /settings/local-generation` so Settings can pin the
  kitty model and reload Ollama without a process restart.

### 2026-09-12 — 0.2.11 — Package the recurring App task runtime

- Coordinate Magician `0.7.22` and API `0.3.18` in the server distribution: stable scheduled task identities, exact-occurrence restart recovery, completion-based scheduling and scoped legacy-task maintenance.
- The implementation passed the targeted recurring tests, SSD1 Make debug build and live restart/round verification. The version metadata update follows that deployment; see the completion record.

### 2026-09-11 — 0.2.10 — Gemini Transcribe at boot, contract tests for Plane and app recipes

- **Speech:** register `gemini_transcribe` recording STT and `gemini_live_transcribe` streaming STT when `GEMINI_API_KEY` is set. Selectable realtime profiles advertise a speakable voice catalog on `GET /media/providers`.
- **Tests:** `tests/plane_elicitation_contract.rs` and `tests/app_reconciliation_contract.rs` pin the Plane typed-input flow and the shipped system-app recipes against frozen `tests/fixtures/app-runtime-v1` packages.
- **Roster:** the process-wide `agent_roster_data` copy reports `busy: null`; the per-scope registries the app path reads attach the artifact service and answer it.

---

Older entries: `docs/archive/changelogs/magician-bin.md`
