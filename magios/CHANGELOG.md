# Changelog

## [Unreleased]

### 2026-10-15 — 0.4.3 (build 207)

- "Magician at work" Live Activity for coding runs, crews and Work tasks, with Approve from the lock screen and Follow for a per-item activity.
- An archived task's chat card settles as "Archived" instead of reading "Running".
- **VibeDev** companion in the left menu: Now (crew questions, approvals, crews, live runs with Steer/Stop) and Projects, with searchable runs, turn timelines, changed files and folding diffs, and dictation in the New run, Follow up, crew and Steer sheets. Destructive actions offer a two-second Undo; screens refresh live and show stale data offline. The coding-runs Live Activity opens VibeDev (`magican://vibe`).
- Coding-runs Live Activity on the lock screen with push updates and push-to-start (iOS 17.2+); diff approvals can be answered directly. VoiceOver labels and actions, wrapping at accessibility text sizes, 44 pt targets, and an offline cache.
- VibeDev fixes: undo-window actions are sent even after leaving the screen; only one Live Activity runs and failed starts retry; stale reads no longer overwrite newer turns; refresh keeps paged runs; failed approvals are reported; offline caches are per machine, account and workspace.
- More VibeDev fixes: the Live Activity and its push routes follow the machine in use and are cleaned up on switch or forget; route removals cannot remove a newer activity's route; run screens show turns while events keep arriving over a slow connection; Forget removes the machine at once and sends its route removals in the background, giving up after 10 seconds.

### 2026-10-12 — 0.4.2 (build 206)

- Filled buttons pick a readable text colour; Internal work refreshes on navigation; a selected tag keeps the Today filter.
- Asks are answerable in place in the composer and on task detail; Relentless reviews show Approve/Reject and Review draft.
- Fixed: development-signed Release builds register the correct APNs environment, so the server no longer drops their tokens.
- Answer simple asks from the notification (Approve/Reject, up to four options, or a text reply); answering needs an unlocked phone. Questions stay hidden until unlock unless the new "Show questions on lock screen" setting is on.
- Settings explains how iOS Show Previews affects push questions; push answering logs content-free diagnostics.

### 2026-10-04 — 0.4.1

- Paired-machine selection, refreshed theme, task and attention surfaces, tree-layout thinking maps and new icon/launch assets (build 205); speech/timer and remote reliability fixes.

## 2026-09-30 — v0.4.0 (build 203): Observe Command Deck, Tasks parity, voice

### Voice notes and voice settings

- Dictation in an idle chat is an ordinary chat turn again.
- Voice settings choices open a scrollable list, so every entry is reachable.

### Observe Command Deck

- Observe matches the web Command Deck: status row, KPI cards switching Now, Sources, Audio and Notes, and live captures.
- Now has a capture launchpad, Upcoming, widget slots and Recent captures; Sources shows device and web sources; Audio picks meeting and listening profiles.
- `magican://observe?pane=…` deep links; screen share no longer starts the mic on return.
- Fixed: Send bot failures show an error; Share screen has a title field; Today swipe cards no longer block page scrolling on iOS 18+; app widget slots no longer flash "temporarily unavailable".

### Tasks web parity

- Task cards show Result, Publish to Notes, Reset only for tasks that ran, and a readable recurring cadence.
- Task detail separates deliverables from intermediate artifacts, groups delegated children, and shows run durations.

## 2026-09-28 — v0.3.0 (build 202): Today is the Morning Edition

- Today matches the web Morning Edition: greeting, realtime wire, lead story, operations carousel, Reading Room with Morning Brief deck and Broadsheet, and briefing, deliverable and digest sections.

---

Older entries: `docs/archive/changelogs/magios.md`
