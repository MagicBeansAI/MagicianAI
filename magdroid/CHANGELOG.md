# Changelog - Magdroid Companion

All notable changes to the Android automation companion are documented here.

This file also serves the Apache 2.0 §4(b) obligation to state significant
changes made to the vendored work.

---

## [Unreleased]

### 0.6.3 (versionCode 30) — 2026-10-15

- "Magician at work" ongoing notification with a needs-you heads-up, and Follow for per-item notifications.
- An archived task's chat card settles as "Archived" instead of reading "Running".
- **VibeDev** companion in the left drawer, matching iOS: Now, Projects, crews, searchable runs, run timelines, changed files and diffs, and sheets for new runs, follow-ups and steering with dictation. Destructive actions offer a two-second Undo.
- An ongoing "Coding runs" notification follows run state (opt-out in Settings); `magican://vibe` opens VibeDev; diff approvals can be answered directly. Accessibility labels, larger touch targets, font scaling, and an offline cache of the last view.
- VibeDev fixes: the notification can no longer stick; switching or forgetting machines cleans up routes, the card and per-machine caches; navigation and launcher shortcuts close VibeDev correctly; diffs keep their file, release memory, scroll lazily and no longer crash on very long lines; refused route removals stop retrying.

### 0.6.2 (versionCode 29) — 2026-10-12

- Filled buttons pick a readable text colour (Tasks and Plan Inspector).

### Asks answered in place; push route re-sent

- The composer and task panel answer questions and approvals in place; Relentless reviews offer Review draft, Reject and Approve. Answering anywhere clears the ask everywhere.
- The push route is re-uploaded on launch, foreground and pairing, so a dropped route comes back.

### Answer asks from the notification

- Simple asks can be answered from the notification (confirm/deny, up to three options, or a typed reply); sensitive or larger asks still open the app, and answering needs an unlock.
- New setting "Show questions on lock screen", off by default.

### 2026-10-04 — 0.6.1

- Paired-machine selection, readable night-theme accents, tree-layout thinking maps and new launcher/splash assets, plus task-progress, owner-attention, scoped-reconnect and remote-task fixes listed in the development notes.

---

Older entries: `docs/archive/changelogs/magdroid.md`
