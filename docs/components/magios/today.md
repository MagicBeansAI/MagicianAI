# Today (iOS / Magios)

Backend contract: [today-feed](../magician/today-feed.md) ·
web parity reference: [unified-ui/today-attention-lanes](../unified-ui/today-attention-lanes.md)

## The date the request carries

`TodayViewModel.get(_:query:)` attaches `today=YYYY-MM-DD`, the **reader's**
own calendar date in their own zone, to every `/api/magician/v2/today` read. It
lives in the transport helper beside the principal and workspace, so all three
call sites (the preview `fetch()`, `loadRemaining(_:)` section paging, and
`loadDigest(offset:)`) ask about the same day.

The value comes from `TodayViewModel.readerLocalDate`, which delegates to
`TasksViewModel.localDateISO(_:in:)`, the derivation the native tasks list
uses; there is no second copy. Every Today predicate is a date comparison (due
today, changed since, overdue), and on its own the server answers from the UTC
date, which is wrong for part of every day east of UTC (five and a half hours a
day in IST).

`readerLocalDate` is a closure evaluated per request, not cached at init, so a
polling session (30s/20s/60s timers) that crosses local midnight moves to the
new day, and a test can pin a reader east of UTC whose date differs from the
UTC date
(`TodayViewModelTests.testTodayRequestCarriesTheReaderLocalDateNotItsUTCRendering`).

The parameter is optional on the wire; a client that omits it gets the
server's UTC date.

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
   then a double rule, re-rendered each minute by a `TimelineView`. The
   greeting ("Good morning" 05–11, "Good afternoon" 12–17, else "Good
   evening") is the navigation-bar title; the tab label stays "Today" and the
   only refresh control is the toolbar button.
2. **Realtime Wire** — collapsed ticker (cycles every 4.5 s) plus the
   `formatCompact24h` event count; the drawer shows All/Events/Insights/Activity
   chips, the five newest lines (task → task, thread → thread) and an
   **Activity** button that opens the former Activity section (search, filters,
   remove, clear all) as a sheet.
3. **Lead story** — first `needs_you` item (→ Attention), `+n more urgent →`;
   otherwise a one-line "Slate is Clear · Nothing needs attention." row.
4. **Operations carousel** (`TodayOperationsCarousel`): one card of slides
   (title + collapsed + expanded content) with the current slide's serif title
   and a shared collapse chevron (collapsed by default). Fixed content heights
   (collapsed 62 pt, expanded 206 pt) keep the page still; one-line lists show
   3.5 rows of 27 pt. Pages via a `TabView(.page)` swipe or tappable dots
   ("Slide 2 of 3, State of Operations"); auto-advances every 8 s, pausing 15 s
   after a manual change, never under Reduce Motion (or `--ui-test`).
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
- Row buttons: the deck's **⚡ Act** menu (same entries and handlers, see
  below) beside **Useful** / **Acknowledge** / **Dismiss** + reason (follow-up)
  or **Dismiss ▾** (worth). Snooze and the channel/resurfacing actions live in
  Act; there is no separate Actions sheet.

Card reads omit full-universe diagnostic scans. Pulse slices and page sections publish independently as requests finish, keeping first-load placeholders until each settles and existing content during refresh.
Both Reading Room layouts show absolute local receipt/surfacing dates and `For You · source` / `Worth a Look · source` labels; older servers fall back to a labelled reference date when available.

### Morning Brief deck

Cards interleave channel follow-ups and worth cards as {2 follow-ups, 1 worth}
(core `followups` items live only in the broadsheet's For You column). Tabs:
All / For You / Worth a Look with server totals and a separate loaded/total caption. The top card sits over two
preview cards.

- **Gestures:** drag right past 95 pt = Useful, left = Dismiss; a flick whose
  predicted end crosses the threshold also commits. The card claims only
  **horizontal** drags; a mostly vertical drag scrolls the page and never
  triages (so there is no "Seen" swipe). Reduce Motion skips rotation/fly-out
  and fades.
- **Buttons:** `Dismiss` · `Seen` (acknowledge — button only) · `Useful` ·
  `⚡ Act`, a menu of the card's real actions (`TodayDeckCard.actEntries`):
  - follow-up: each server `available_actions` entry (opens the detail sheet
    on that action's composer / confirmation), **Open in <Provider>** when
    there is an `open_url`, **Start follow-up task…** (opens the detail's
    optional-instruction editor; `approve` is sent only from there, and the
    entry is hidden while `review_required`), **Snooze — hide from Today**,
    **Details**;
  - worth: **Open** (`open` feedback, then the card's `open_url`, an in-app
    `source_route`, or the resurfacing detail), each `actions` capability
    except `view_details` / `open_source` (input actions open the existing
    input form; the rest run and land on the detail), **Details**.
  Only Snooze and Open take the card off the deck (optimistic, rolled back on
  failure); every other entry leaves it on top.
- **Tap** (or the card's **Details ›** link) opens the existing rich sheet
  (follow-up detail with compose, dismiss reasons and writing style;
  resurfacing detail with actions/Reminders).
- `ask_presto` is labelled **Ask <primary agent>** everywhere in Today (Act
  menus, resurfacing detail); the wire value is unchanged.
- Actions go through the existing optimistic view-model calls; a failed call
  rolls back, shows the error banner and returns the card to the stack.
  Impressions are recorded for the top card only.
- With four or fewer cards in the selected tab, cursor paging prefetches its next pages. Loading and retry states stay distinct from a cleared deck; head refreshes retain accumulated pages.

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
| Wire live lines | the app's realtime hub (`/api/magician/v2/realtime/ws`; a hub resync refetches Today) — every non-control frame, `AgentEvent` unwrapped to its inner type/agent/timestamp, streaming deltas skipped; each bumps the count |
| Ledger spend / hourly calls | existing LLM pulse SQL |
| State of Operations | the pulse's `GET /api/magician/v3/tasks` (succeeded = max(#completed, completed today); active = running/paused/planning; the same rows feed the recent-task list) |
| Agents | `GET /api/magician/v2/agents` (`agents` only; `definition.name` for the crew), fetched with the pulse, fail-soft |
| State of the Crew | per-agent `llm_calls` SQL (last 24 h) + `/v3/tasks` newest-first pages, with the pulse (60 s) |
| Deck | existing follow-up and resurfacing load-more pages |
| Broadsheet | its own 5-per-page follow-up (cursor) and resurfacing (offset) reads |

The wire never shows placeholder lines or an invented count.
