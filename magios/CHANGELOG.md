# Changelog

## Unreleased — Observe Command Deck

- Observe is the web Command Deck: a status + Refresh row, four KPI cards that switch between Now, Sources, Audio and Notes, and live captures above every view.
- Now has a capture launchpad (Listen, Join as agent, Share screen, Brainstorm), Upcoming with loading and error states, widget slots, and a Recent captures list.
- Sources shows this phone's microphone, notification and Live Activity state, plus read-only web sources; Audio picks the meeting and listening profiles.
- `magican://observe?pane=…` opens Observe on a view; a prepared screen share no longer starts the in-app mic on return and resets when the broadcast ends.
- Upcoming "Send bot" failures now show an error, and Share screen has its own title field.
- Today swipe cards (Morning Brief deck, For You / Worth a look) no longer stop the page scrolling on iOS 18+: a vertical drag that starts on a card scrolls the page, sideways still swipes.
- App widget slots no longer flash "temporarily unavailable": a 304 renews every item's deadline, a near-now deadline retries after the refresh floor, the last good cards survive refreshes and transient errors for up to 5 minutes, and a hidden workspace default shows the add slot instead.

## Unreleased — Tasks web parity

- Task cards show **Result** (opens Output → Result), **Publish to Notes**, Reset only for tasks that ran, and a ↻ Recurring chip with a readable cadence instead of the raw cron.
- Task detail Output splits Deliverables from a collapsed "Intermediate artifacts & evidence" group; Run activity groups delegated children (Grouped | Chronological); History shows run durations; the duplicate Reset menu item is gone.

## 2026-09-28 — v0.3.0 (build 202): Today is the Morning Edition

- Today matches web's Morning Edition:
  - The navigation bar shows the greeting, and the masthead is compact.
  - A realtime wire, whose AgentEvent frames show their real type.
  - A one-line lead story or "Slate is Clear".
  - A collapsible operations carousel: Economics, State of Operations and State of the Crew.
  - A one-row Reading Room with a sideways-only Morning Brief swipe deck and a tabbed, server-paged Broadsheet.
  - § 3 briefings, § 4 deliverables and § 5 digest, set in Newsreader.

## 2026-09-27 — v0.2.8 (build 201): service-health attention

- Recognize service-health HITL source labels alongside existing task attention sources.

## Unreleased — app contract fixtures

- Regenerated `AppContractFixtures.generated.swift` for the new `app_memory_read_v1` manifest feature (no app-side behaviour change).

---

Older entries: `docs/archive/changelogs/magios.md`
