# Changelog

All notable changes to the Magician API project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 2026-11-02 — 0.3.72

- Resuming a paused run (an answered question, or `continue`) that then delegates saves the Delegation checkpoint and keeps its authority: the answered pause's cleanup no longer deletes or overwrites it, and a continued run that delegates parks instead of failing.
- `GET /health` reports `tools.ripgrep` (`status` `ok`/`missing`, `source`), whether the `grep` and `glob` tools can search.
- Saving a skill's env on the Skills page takes effect at once. The write invalidated nothing, so a changed value (a Metabase address, say) was not used until setup ran again; it now invalidates the workspace's capability, skills-folder and pack caches. Connect, status, Service accounts and the catalog resolve a `${NAME}` endpoint from the workspace's skill env; Connect answers 422 naming an unset or invalid variable, and Service accounts still lists a stored account whose contract cannot be read.

### 2026-10-31 — 0.3.71

- A realtime `session.start` with `new_chat: true`, and a voice note with the form field `new_chat=true`, get a chat of their own instead of joining the thread's open chat; a named `chat_session_id` still wins.
- `/ui-threads/{key}` accepts a thread id, its slug or a former slug. `POST /ui-threads` without an id derives a fresh unique id from the name, instead of renaming the thread that already has that id. `/chat/active`, `/chat/new` and `/chat/sessions` map a slug or former slug passed as `ui_thread_id` to the thread's id.

### 2026-10-31 — 0.3.70

- `POST /agents/{id}/trigger` rejects a `harness:{agent}:` goal that names none of the agent's focus areas, also when it has none left: retired CPO and CTO routines ran without a program from the Team routines sequence.
- Portable task export (`GET /v3/tasks/{id}/export?format=zip`) writes its archive with zip 9.

### 2026-10-30 — 0.3.69

Page chat session detail with `has_more`, bound Today and analytics windows, and expose resurfacing timestamps.

- Chat session detail (`GET /chat/sessions/{id}` and the active-session reply) returns `has_more`, so clients know when older messages can be paged with `before=`.
- Chat activity skips nested debug payloads and updates cached whole-turn usage from appended rows.
- Today card reads can omit full-universe diagnostics; analytics materialize bounded windows before aggregation.

- Resurfacing cards expose their surfacing timestamp separately from the source reference date.

### 2026-10-27 — 0.3.68

Serve immutable public MCP profiles and expose owner identity claims and owner-only chat commands.

- `POST /auth/browser-signin/bot/project-access` reports a Google Workspace account's Cloud project roles and `.../grant` adds missing ones.

- Service accounts list Google Workspace, Telegram and WhatsApp bots, connected through new `POST /auth/browser-signin/bot/{start,input,state,cancel}`.

- `/settings/god-mode` and `/chat/sessions/{id}/god-mode` read and set `auto_approve`.
- Browser-CLI sign-in follows CLIs (such as Higgsfield's) that hand their login link through a local `sign-in.html`.
- `POST /auth/browser-signin/accounts` rows add `status` and `detail`, one row per `endpoint_alias`; `/launch` accepts an optional `endpoint_alias`.
- `task_active` errors answer 409 with `Retry-After: 5`, and `DELETE /v3/tasks/{id}` waits up to 30 s for a just-finished run to settle.
- An owner session can read the resource-authority ledger without a key; `RESOURCE_AUTHORITY_API_KEY` now rides `X-Resource-Authority-Key` and is needed only for changes.
- `/budgets/defaults` and `/budgets/envelope` edit `resource_authority.model_spend`; new `GET`/`PUT`/`DELETE /budgets/limits` edit action limits (applied on restart).

### 2026-10-23 — 0.3.67

- `GET /health`: `version` is now the Magician core version (`magician::VERSION`); this crate's version moves to `api_version`, and `versions` lists magician, magician_api, magicutor and desktop. Magicutor and the desktop gateway are probed concurrently.
- Contextual writing adds the selected personality preset's voice guidance to the drafting request, so drafts follow the user's pick over the writing agent's default.
- `DELETE /chat/sessions/{id}/assets/{asset_id}` removes one chat copy (owner login or verified paired phone; 409 while the chat is busy); the asset listing adds `size`, `total_size` and `unknown_size_files`.
- Budget and spend-envelope routes accept a verified paired owner phone as well as an owner login; settings reload applies `execution.harness_turn_idle_seconds`.
- Steering a run that is not executing returns 409 with its `waiting_state`, so clients refresh their controls.
- STT and TTS accept an optional `X-Chat-Session-Id` (checked against the caller's scope), so voice notes are charged to their chat.

### 2026-10-23 — 0.3.66

- `GET /api/magician/v2/chat/sessions/{id}/assets`: a chat's uploads plus the artifacts of its tasks and their descendants, workspace-scoped (403 for another workspace).
- God mode routes accept a verified paired owner phone as well as an owner login (`owner_phone_required` otherwise).
- The daily LLM cost rollup records the root execution and per-category call counts, for chat and task usage totals.
- Relentless proposals are presented as requests with a "Review and start" link; Dismiss closes the proposal without starting a run.

### 2026-10-23 — 0.3.65

- Browser submission grants: `GET`/`DELETE /browser/submission-grants[?origin=…]` list and revoke the sites the owner approved with Approve Always (owner session only).
- Owner budgets API: `GET`/`PUT /budgets/envelope` (partial, validated, written back to the config file) and `POST /budgets/envelope/grant` (adds spend for the current window; never unlimited). Session-bearer only.

### 2026-10-22 — 0.3.64

- Expose original-folder diff approval, cross-project run paging and recorded follow-up model/work-location metadata.

### 2026-10-21 — 0.3.63

- `PATCH /notes/front-matter` (`path`, `set`/`unset`, `expected_revision`); 409 `note_revision_conflict` with `current_revision`; note reads carry `revision`.

---

Older entries: `docs/archive/changelogs/magician-api.md`
