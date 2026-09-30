# Today — Morning Edition (Android)

Android's Today tab renders the web `/today` Morning Edition
(`ui/unified-ui/src/lib/today/MorningEdition.svelte`) as one `LazyColumn`.
Each section, and each broadsheet card, is its own lazy row, so attention
impressions start only for cards that are actually on screen.

Code: `magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/`
(`TodayScreen.kt` entry, routing and sheets; `TodayMorningEdition.kt`,
`TodayRealtimeWire.kt`, `TodayLedger.kt`, `TodayTriageDeck.kt`,
`TodayBroadsheet.kt`, `TodayNewspaper.kt`). Pure rules live in
`bridge/.../today/TodayMorningEdition.kt`; state lives in `TodayViewModel`.

## Layout (top to bottom)

1. **Masthead**: the Newsreader `Today's` title, then one row with the full
   date on the left and `VOL. {roman(year − 2022)} · NO. {day of year}` on the
   right, then a double rule. The clock is re-read every minute, so the date
   and issue number change at midnight. The top bar greets instead of
   repeating "Today" ("Good morning" 5–11, "Good afternoon" 12–17, "Good
   evening" otherwise) and carries the refresh button. Web's WEATHER item and
   greeting kicker are left out to save vertical space.
2. **Realtime wire**: a live dot, a ticker that changes every 4.5 s while
   collapsed, and a `formatCompact24h` count chip (`35K/24H`). Tapping it opens
   the latest five lines with All/Events/Insights/Activity filters. A line with
   a task opens the task, a line with a thread opens the thread, and other lines
   do nothing. The **Activity** button opens the former Activity section as a
   sheet (search, filters, remove, clear).
3. **Lead story**: the first Needs You item with `Take action now →`, which
   opens Attention, and `+{n−1} more urgent →` when there are more. When there
   are no urgent items it is one line: `Slate is Clear · Nothing needs attention.`
4. **Operations carousel** (`TodayLedger.kt`): one card whose header title
   changes with the slide, with dots under it (tap to jump). It auto-advances
   every 8 s. It holds for 15 s after a swipe or a dot tap, and does not
   auto-advance with animations off. One chevron collapses or expands every
   slide. Collapsed is 62dp high and expanded is 206dp, so the page never
   jumps; the task list is the only part that scrolls. More slides are one
   `OpsSlide` entry.
   - **Economics of Operations**: collapsed is one row, with today's spend
     and `{n} calls` beneath it and a 24-bar hourly chart to the right. Tap or
     drag a bar to see `{3p}: $0.42 (12 calls)`. Expanded cross-fades to the
     full layout: the top provider, the figure beside the delta against
     yesterday (more spend in red, less in green) and `{n} model calls
     today`, the chart, and a stats row: Avg / call, Peak hour, Coding runs,
     and Memories (or Evals passed/cases when there are any).
   - **State of Operations**: collapsed shows the Active / Succeeded / Failed
     counts and a mini pie. Expanded adds a solid pie with a legend (a dashed
     `IDLE` disc when there are no tasks), agent Enabled/Active/Total counts,
     `Tasks →`, and the 20 most recently updated `/v3/tasks` rows, one line
     each (status dot · title · `4m`/`2h`/`3d`), which open the task. Three
     and a half rows show; the rest scroll.
     `Agent Crew →` is left out because the phone has no crew destination.
   - **State of the Crew** (last 24 hours): collapsed shows the crew totals:
     active now/total, cost, tasks done and reliability. Expanded adds one row
     per agent (display name from `definition.name` · cost · tasks done ·
     success · reliability), with three and a half rows visible. Only agents
     with activity in the window, or running now, are listed, active first.
     Cost, calls and successful calls come from one grouped
     `llm_calls` query. Tasks come from `/v3/tasks?sort=updated_at&order=desc`,
     walking `next_cursor` (up to 5 pages) while a page is still inside the
     window. **Success** = done ÷ (done + failed); **Reliability** =
     successful model calls ÷ calls. Rates are ≥95% green, 80–94% amber, and
     below 80% red, and read `—` with nothing to measure. It is refreshed on
     the 60 s pulse cadence.
5. **Reading Room**: the title and a compact right-aligned switch
   (`🃏 Brief {n}` | `📰 Broadsheet`) share one row. The choice is saved in SharedPreferences
   (`reading_room_mode`). A hidden-items drawer (`{n} hidden · Show`) with
   Restore/Undo follows.
6. App widget slots (`/` primary, secondary).
7. **§ 3 Special Reports & Briefings**: up to 6 cards (all of them after
   `View all →`). Tapping a card opens the briefing render sheet.
8. **§ 4 Completed Deliverables**: shown only when `delivered` is non-empty.
   Cards appear 6 at a time with a `Load more` button. `Inspect →` opens the
   item; `Acknowledge` hides it and can be undone.
9. **§ 5 The Chronicle & Digest**: shown only when the digest has bullets.
   There are 6 bullets per page (`digest_limit=6`), with Newer/Older buttons and
   `↻ Refresh digest`.
10. Footer `TODAY'S · MORNING EDITION`.

The Newsreader serif (`res/font/newsreader_{regular,medium,semibold,bold,extrabold}.ttf`,
static instances cut from the variable font) is
used for headings and headlines. Each weight ships as its own file because a
single variable resource renders Bold as the thin default on device. Section titles shrink to fit one line (`FitText`). Kickers use
the theme mono font. All colours
come from the active palette.

## Morning Brief deck

The deck interleaves channel follow-ups and worth-a-look cards, two follow-ups
then one worth card. Its tabs are All / For You / Worth a Look, each showing
how many cards are left. The top card sits over up to two preview cards.

| Gesture / button | Follow-up | Worth a look |
|---|---|---|
| drag right past 95dp (or flick) · `✓ Useful` | `useful` | `open` feedback |
| drag left past 95dp · `✕ Dismiss` | `dismiss` (no reason) | `dismiss` |
| `Seen` button | `acknowledge` | `acknowledge` |
| `⚡ Do it` / `⚡ Open` | `approve` | `open`, then the source (route, URL, or the detail sheet) |
| tap | message sheet with every follow-up action | resurfacing detail sheet |

- Only a sideways drag moves the card. A release also commits when its
  velocity would carry the card past the threshold.
- Web's swipe-up Seen is a button here: on a phone the card fills most of the
  screen, so an upward swipe is indistinguishable from a page scroll.
- USEFUL and DISMISS stamps fade in as the card is dragged; ACKNOWLEDGED
  shows when Seen is pressed. A
  committed card flies out over 260 ms with a light haptic.
- With animations turned off (Reduce Motion), the card does not rotate and
  fades out instead of flying.
- A drag that starts on the card and moves mainly up or down scrolls the page
  instead of moving the card.
- Each action goes through the existing `TodayViewModel` mutation: it removes
  the card straight away, keeps the per-card lock, and records receipts and
  attribution. `triageDeckCard` refuses a card that is already locked, so the
  card springs back. If an action fails, the card is restored and the error
  dialog is shown.
- When two or fewer cards are left, the next follow-up and worth pages are
  fetched.
- Only the top card records an impression.

## Broadsheet

The broadsheet has its own **For You** / **Worth a Look** tabs (themed
rounded-rect chips with count badges; the chosen tab is saved as
`broadsheet_tab`). Each tab shows one server page of 5 cards in a fixed-height
(460dp) container that scrolls on its own, with a pager under it in the style
of web's ServerPager: `6–10 of 23 ‹ 2 / 5 ›`.

- **Worth a Look** pages by offset:
  `/channel-assist/resurfacing/today?limit=5&offset=…`.
- **For You** pages channel follow-ups by keyset cursor. It remembers the
  cursor that opens each page and walks forward from the nearest known page
  (web `ensureFollowUpCursor`). Core `followups` items, which are few and
  unpaged, lead page 1.
- These pages are separate from the deck's load-more lists. A per-tab request
  generation drops late answers. Current pages are re-read on the 30 s poll and
  after every card action, so a resolved card is backfilled from the server.

Core `followups` items keep a snooze time picker. Each card keeps its existing swipe rails and actions, restyled with a
mono kicker line and a serif headline. Actions differ by card type:

- Channel follow-ups snooze with a single **Snooze — hide from Today** action
  and have no duration picker, because the wire carries no duration.
- Resurfacing cards cannot be snoozed. Their dismiss reasons are limited to
  spam, already_handled, duplicate, delegated and not_relevant.

## Data

- `refresh()` (30 s): Today, hidden items, resurfacing, follow-ups, briefings,
  pulse, `/v2/feed?limit=80`, `/v2/agents/updates`, `/v2/agents` and the 24 h
  count. The feed read supplies both Activity (durable rows) and the wire (the
  15 newest rows).
- Pulse (60 s) also refreshes agent counts and the 24 h count
  (`POST /v2/analytics/query`, `SELECT COUNT(*) … FROM events`; on failure the
  last value is kept). The existing `/v3/tasks` read supplies the pie buckets:
  succeeded = max(#completed, completed today), failed, and in flight
  (running/paused/planning).
- The existing `/v2/realtime/ws` socket turns every scope-matched frame into an
  `event` wire line and increments the count. Keepalive and token-delta frames
  are skipped. Only frames that concern Today still trigger the debounced
  refresh.
- The wire shows only real records and never fills in placeholder lines.

## Invariants

- Impression timers run only while at least half of a card is inside the
  window (each card is its own lazy row).
- Resurfacing detail, briefing, compose and writing-style sheets show a load
  error with Retry instead of spinning; digest page errors appear in § 5 with
  Retry. The action-error dialog shows even when the day has never loaded.
