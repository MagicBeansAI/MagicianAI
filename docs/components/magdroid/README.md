## Component docs

- [Today — Morning Edition](today.md): masthead, wire, operations carousel (economics · operations · crew), swipe deck, paged broadsheet, data sources.
- [MUIJ graph](muij-graph.md)

## Tasks parity with web

Task cards follow web `taskActions`: completed tasks lead with **Result** (when a
summary, non-failure outcome or named artifacts exist) then **Publish to Notes**;
failed/cancelled tasks offer **Reset to Ready** only once they have an execution.
Result opens task detail on the Output act at the Result card. A **↻ Recurring**
chip (cron, `recurring`/`app_recurring` tag, or app `recurring_schedule`) replaces
the raw cron pill and carries the human schedule. Output groups task-scoped
**Deliverables** above a collapsed "Intermediate artifacts & evidence" disclosure
(direct, delegated, persisted). Run activity folds delegated children into
status-accented envelopes with a per-device Grouped/Chronological toggle; History
shows each run's duration. Detail colours come from the theme palette.


## Observe — Command Deck

Observe mirrors the web `/observe` Command Deck. A command header (kicker,
title, "N captures live" / "N meetings live — nothing capturing" / "Quiet"
status, one Refresh that reloads every lane with `refresh=true`) sits over four
tappable KPI cards — **Now & Live**, **Sources on**, **Audio Profiles**,
**Notes & Recents** — which are the only view switchers. The chosen view is
remembered per device; `magican://observe?pane=now|sources|audio|notes` opens a
view directly. The **LIVE** block (in-app capture with Share/Stop screen, Stop,
live transcript, battery `powerWarning`; other server sessions with Stop / Open
transcript) sits above every view.

- **Now**: capture launchpad (Listen, Join as agent, Share screen — explains
  that sharing starts from the live card — and Brainstorm → Thinking Map),
  Upcoming (loading, failures, Join (Me)/Listen/Send bot, active match + Open),
  the `/observe` `reviews` widget slot, and Recent (`GET /meetings` → `recent`,
  newest 30, tap opens the thread).
- **Sources**: *This phone* (editable: microphone and notification permission,
  "Also capture the screen" — also in Settings — and the battery guard) and
  *Web & accounts* (view-only: channel-assist channels, calendar observation,
  enabled continuous sources, browser tabs, startup catch-up; each loads and
  retries on its own).
- **Audio**: meeting and listening profile pickers over
  `GET/PUT /media/preferences` + `/media/providers`, saved on selection
  (optimistic, rolled back on failure; clears that surface's stage overrides
  like the web control).
- **Notes**: Published Notes at iOS parity (search via `q`, 5/10/20/50 per page,
  Previous/Next with "a–b of total", Publish next 25, Promote to memory, Open in
  Notes) and an Audio notes link (the drawer entry stays).

Starting to listen also asks once for `POST_NOTIFICATIONS`; a refusal never
blocks capture and is explained instead.

## Storage maintenance visibility

Settings includes Automatic maintenance with Channel Assist/Feed state, a user
message and last completion. Its authenticated `/storage/maintenance` read runs
every ten seconds while Settings is resumed. Failed reads clear stale status;
leaving the surface cancels polling and in-flight work.

Chat reloads its session and engine/profile catalog after mobile pairing, without requiring an app restart.
