# Today (iOS / Magios)

Backend contract: [today-feed](../magician/today-feed.md) ·
web parity reference: [unified-ui/today-attention-lanes](../unified-ui/today-attention-lanes.md)

## The date the request carries

`TodayViewModel.get(_:query:)` attaches `today=YYYY-MM-DD` to every
`/api/magician/v2/today` read — the **reader's** own calendar date, in their
own zone. It is attached in the transport helper, beside the principal and
workspace it already injects, because three call sites read that endpoint (the
preview `fetch()`, `loadRemaining(_:)` section paging, and `loadDigest(offset:)`)
and a lane paged against a different day than the preview above it is a wrong
answer that looks entirely right.

The value comes from `TodayViewModel.readerLocalDate`, which delegates to
`TasksViewModel.localDateISO(_:in:)` — the derivation the native tasks list
already uses. There is deliberately no second copy: clients that each derive
"today" for themselves drift apart (a local midnight rendered in UTC names
yesterday all day east of Greenwich).

It matters because every Today predicate is a date comparison — due today,
changed since, overdue — and the server cannot know where the reader is. Left
to its own clock it answers from the UTC date, which is the wrong day for part
of every day in any positive-offset zone: five and a half hours of every day in
IST.

`readerLocalDate` is a closure evaluated per request rather than a value cached
at init, for two reasons. Today polls (30s/20s/60s timers), so a session that
crosses local midnight must start asking about the new day without being
restarted. And a test can pin a reader east of UTC at an hour where their date
and the UTC date are different days — the only fixture that can tell a local
date apart from a UTC-rendered one
(`TodayViewModelTests.testTodayRequestCarriesTheReaderLocalDateNotItsUTCRendering`).

The parameter is optional on the wire; a client that omits it gets an answer
computed from the server's UTC date.

## Morning Edition layout

Today renders the web "Morning Edition" (`ui/unified-ui/src/lib/today/MorningEdition.svelte`)
as one phone column. Views: `TodayView.swift` (composition, broadsheet cards,
sheets), `TodayMorningEditionViews.swift` (masthead, wire, ledger, banners,
the Newsreader type ramp), `TodayMorningBriefDeck.swift` (swipe deck). Pure
helpers live in `TodayMorningEdition.swift` and are covered by
`MagiosTests/TodayMorningEditionTests`.

Page order:

1. **Masthead** — compact: the heavy serif `Today's`, one row with the full
   date (left) and `VOL. {roman(year − 2022)} · NO. {day of year}` (right),
   then a double rule. A minute `TimelineView` re-renders it so a page left
   open over midnight turns over. The greeting ("Good morning" 05–11,
   "Good afternoon" 12–17, else "Good evening") is the navigation-bar title,
   re-evaluated every minute; the tab label stays "Today" and the only refresh
   control is the toolbar button.
2. **Realtime Wire** — collapsed ticker (cycles every 4.5 s) plus the
   `formatCompact24h` event count; the drawer shows All/Events/Insights/Activity
   chips, the five newest lines (task → task, thread → thread) and an
   **Activity** button that opens the former Activity section (search, filters,
   remove, clear all) as a sheet.
3. **Lead story** — first `needs_you` item (→ Attention), `+n more urgent →`;
   otherwise a one-line "Slate is Clear · Nothing needs attention." row.
4. **Operations carousel** (`TodayOperationsCarousel`) — one card, a generic
   list of slides (title + collapsed + expanded content). The header shows the
   current slide's one-line serif title (cross-fades) and a shared collapse
   chevron (collapsed by default).
   Fixed content heights (collapsed 62 pt, expanded 206 pt) so the page never
   jumps; one-line lists are 3.5 rows of 27 pt so the cut-off row signals
   scrolling. Pages via a `TabView(.page)` swipe or the tappable dots ("Slide 2
   of 3, State of Operations"); auto-advances every 8 s, pausing 15 s after a
   manual swipe/dot tap, never under Reduce Motion (or `--ui-test`).
   - *Economics of Operations* — collapsed: spend figure + "{n} calls" |
     hairline | hourly graph. Expanded: TOP PROVIDER kicker, figure beside the
     inverted-tone delta and "{n} model calls today", graph (tap a bar for
     `3p: $0.42 (12 calls)`), then a stats row AVG / CALL (`$0.0016` under a
     cent) · PEAK HOUR · CODING RUNS · MEMORIES (or EVALS passes/cases).
   - *State of Operations* — collapsed: ACTIVE / SUCCEEDED / FAILED counts and
     a mini pie. Expanded: solid pie + legend, agent Enabled/Active/Total,
     `Tasks →`, and the 20 most recently updated tasks, one line each (status
     dot · title · `4m`), tapping opens the task.
   - *State of the Crew* (last 24 h) — per agent: cost and calls from one
     `llm_calls` aggregate (`TodayMorningEdition.crewSQL`), tasks done/failed
     from `/v3/tasks?limit=100&sort=updated_at&order=desc` (paged by
     `pagination.next_cursor` while the page's last row is still in the
     window, max 5 pages), success % and reliability % (ok_calls/calls, "—"
     when undefined; ≥95 success colour, 80–95 warning, <80 danger), active
     flag. Collapsed: ACTIVE n/total · COST 24H · TASKS 24H · RELIABILITY.
     Expanded: those totals plus a scrolling NAME · COST · TASKS · SUCC. · REL.
     table (rows have no destination on mobile). A failed read shows an error
     with Retry and keeps the last good crew.
   Nothing is drawn until the pulse has loaded; a failed pulse shows its
   section error with Retry.
5. **Reading Room** — one header row: the serif title (shrinks to fit) and a compact right-aligned switch `🃏 Brief {n}` | `📰 Broadsheet` (rounded-rect
   segments, like the deck tabs and wire chips), persisted per
   device (`UserDefaults` key `todayReadingRoomMode`), then the hidden drawer.
6. App widgets (`PinnedAppsTodaySection`, `AppNativeSlotPageRegion`).
7. **§ 3 Special Reports & Briefings** (six cards, `View all →`, tap → render sheet).
8. **§ 4 Completed Deliverables** (only when `delivered` is non-empty; the banner
   shows the bare count, spoken as "n deliverables"; six per
   page; `Inspect →` routes like a card tap, `Acknowledge` = hide(dismiss) with Undo).
9. **§ 5 The Chronicle & Digest** (only with bullets; digest pages of 6,
   Newer/Older, `↻ Refresh digest`).
10. Footer.

`active_work` and `changed` items have no lane; changes surface through the
digest.

### Broadsheet

Own tabs **For You** / **Worth a Look** (rounded-rect chips with count badges;
before a tab's first page loads the badge uses the deck's totals), persisted as
`@AppStorage("todayBroadsheetTab")`. Below the column header a fixed-height
(460 pt) container scrolls its cards independently of the page (reset to top on
page/tab change; spinner on first load; empty-state text inside), then a
`ServerPager`-style row: `6–10 of 23` · ‹ `2 / 5` ›. Five per page, held in
view-model state separate from the deck's load-more lists:

- Worth a Look: `GET …/resurfacing/today?limit=5&offset=(page−1)·5`.
- For You: `GET …/follow-ups?limit=5&cursor=…` with a page→cursor map; page N
  is reached by walking `next_cursor` from the nearest known page
  (`TodayMorningEdition.walkFollowUpCursor`), landing on the last reachable page
  if the cursors run out. Core Today `followups` items sit at the top of page 1.
- A per-tab request generation drops late responses. Current pages re-read on
  the 30 s poll and after every follow-up/worth action (success or failure);
  an optimistic removal drops the page's total by one.

### Morning Brief deck

Cards interleave channel follow-ups and worth cards as {2 follow-ups, 1 worth}
(core `followups` items live only in the broadsheet's For You column). Tabs:
All / For You / Worth a Look with remaining counts. The top card sits over two
preview cards.

- **Gestures:** drag right past 95 pt = Useful, left = Dismiss; a flick whose
  predicted end crosses the threshold also commits. The card claims only
  **horizontal** drags — a predominantly vertical drag that starts on it
  scrolls the page and never triages (on a phone the card fills most of the
  viewport, so an upward "Seen" swipe is indistinguishable from scrolling).
  Stamps ink in with the drag; Reduce Motion skips rotation/fly-out and fades.
- **Buttons:** `Dismiss` · `Seen` (acknowledge — button only) · `Useful` ·
  `⚡ Do it` (follow-up `approve`) / `⚡ Open` (worth `open` feedback, then the
  card's `open_url`, an in-app `source_route`, or the resurfacing detail).
- **Tap** opens the existing rich sheet (follow-up detail with compose, dismiss
  reasons and writing style; resurfacing detail with actions/Reminders).
- Actions go through the existing optimistic view-model calls; a failed call
  rolls back, shows the error banner and returns the card to the stack.
  Impressions are recorded for the top card only.
- When two or fewer cards remain, `loadMoreDeckCards()` pulls the next
  follow-up / worth page through the existing cursors (only when a cursor exists).

Snooze: a channel follow-up snooze has no duration on the wire, so it is a
single **Snooze — hide from Today** action; core Today follow-ups keep their
Tonight / Tomorrow / Next week picker (`snooze_minutes`). Resurfacing has no
snooze and its dismiss reasons are only `spam`, `already_handled`,
`duplicate`, `delegated`, `not_relevant`.

### Data sources

| Section | Source |
| --- | --- |
| Wire feed lines | the Activity sheet's `GET /api/magician/v2/feed?limit=80` response (newest 15 rows) |
| Wire agent lines | `GET /api/magician/v2/agents/updates` |
| Wire 24h count | `POST /api/magician/v2/analytics/query` `SELECT COUNT(*) AS total_24h FROM events …` (kept at its last value on failure) |
| Wire live lines | the existing `/api/magician/v2/realtime/ws` socket — every non-control frame, `AgentEvent` unwrapped to its inner type/agent/timestamp, streaming deltas skipped; each bumps the count |
| Ledger spend / hourly calls | existing LLM pulse SQL |
| State of Operations | the pulse's `GET /api/magician/v3/tasks` (succeeded = max(#completed, completed today); active = running/paused/planning; the same rows feed the recent-task list) |
| Agents | `GET /api/magician/v2/agents` (`agents` only; `definition.name` for the crew), fetched with the pulse, fail-soft |
| State of the Crew | per-agent `llm_calls` SQL (last 24 h) + `/v3/tasks` newest-first pages, with the pulse (60 s) |
| Deck | existing follow-up and resurfacing load-more pages |
| Broadsheet | its own 5-per-page follow-up (cursor) and resurfacing (offset) reads |

The wire never shows placeholder lines or an invented count.
