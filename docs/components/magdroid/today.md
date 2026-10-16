# Today — Morning Edition (Android)

Android's Today tab renders the web `/today` Morning Edition
(`ui/unified-ui/src/lib/today/MorningEdition.svelte`) as one `LazyColumn`.
Each section and each broadsheet card is its own lazy row, so attention
impressions start only for cards actually on screen.

Code: `magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/`
(`TodayScreen.kt` entry, routing and sheets; `TodayMorningEdition.kt`,
`TodayRealtimeWire.kt`, `TodayLedger.kt`, `TodayTriageDeck.kt`,
`TodayBroadsheet.kt`, `TodayNewspaper.kt`). Pure rules live in
`magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/today/TodayMorningEdition.kt`;
state lives in `TodayViewModel` (same package).

## Layout (top to bottom)

1. **Masthead**: the Newsreader `Today's` title, the full date and
   `VOL. {roman(year − 2022)} · NO. {day of year}`, then a double rule. The
   clock is reread every minute, so the date and issue change at midnight. The top bar
   greets ("Good morning" 5–11, "Good afternoon" 12–17, "Good evening"
   otherwise) and carries refresh. Web's WEATHER item and greeting kicker are
   left out.
2. **Realtime wire**: a live dot, a ticker that changes every 4.5 s while
   collapsed, and a `formatCompact24h` count chip (`35K/24H`). Tapping opens
   the latest five lines with All/Events/Insights/Activity filters; a line
   with a task or thread opens it; other lines do nothing. **Activity** opens the Activity sheet
   (search, filters, remove, clear).
3. **Lead story**: the first Needs You item with `Take action now →` (opens
   Attention) and `+{n−1} more urgent →` when there are more. With nothing urgent:
   `Slate is Clear · Nothing needs attention.`
4. **Operations carousel** (`TodayLedger.kt`): one card whose title changes
   with the slide, with tappable dots. It auto-advances every 8 s, holds 15 s
   after a swipe or dot tap, and does not auto-advance with animations off.
   One chevron collapses or expands every slide (62dp / 206dp, so the page
   never jumps); only the task list scrolls. Each slide is one `OpsSlide`
   entry.
   - **Economics of Operations**: collapsed shows today's spend, `{n} calls`
     and a 24-bar hourly chart (tap or drag a bar for `{3p}: $0.42 (12 calls)`).
     Expanded adds the top provider, the delta against yesterday (more spend
     red, less green), `{n} model calls today`, and Avg / call, Peak hour,
     Coding runs and Memories (or Evals passed/cases when present).
   - **State of Operations**: collapsed shows Active / Succeeded / Failed and
     a mini pie. Expanded adds a pie with legend (a dashed `IDLE` disc with no
     tasks), agent Enabled/Active/Total, `Tasks →`, and the 20 most recently
     updated `/v3/tasks` rows (status dot · title · `4m`/`2h`/`3d`), which
     open the task. `Agent Crew →` is left out (no phone destination).
   - **State of the Crew** (last 24 hours): collapsed shows active now/total,
     cost, tasks done and reliability. Expanded adds one row per agent
     (`definition.name` · cost · tasks done · success · reliability), listing
     only agents active in the window or running now, active first. Cost and
     calls come from one grouped `llm_calls` query; tasks from
     `/v3/tasks?sort=updated_at&order=desc`, walking `next_cursor` (up to 5
     pages) while inside the window. **Success** = done ÷ (done + failed);
     **Reliability** = successful model calls ÷ calls. ≥95% green, 80–94%
     amber, below 80% red, `—` with nothing to measure. Refreshed on the 60 s
     pulse.
5. **Reading Room**: the title and a switch (`🃏 Brief {n}` |
   `📰 Broadsheet`), saved in SharedPreferences (`reading_room_mode`), then a
   hidden-items drawer (`{n} hidden · Show`) with Restore/Undo.
6. App widget slots (`/` primary, secondary).
7. **§ 3 Special Reports & Briefings**: up to 6 cards (all after
   `View all →`); a tap opens the briefing render sheet.
8. **§ 4 Completed Deliverables**: only when `delivered` is non-empty; 6 at a
   time with `Load more`. `Inspect →` opens the item; `Acknowledge` hides it
   (undoable).
9. **§ 5 The Chronicle & Digest**: only when the digest has bullets; 6 per
   page (`digest_limit=6`), Newer/Older and `↻ Refresh digest`.
10. Footer `TODAY'S · MORNING EDITION`.

Headings use the Newsreader serif
(`res/font/newsreader_{regular,medium,semibold,bold,extrabold}.ttf`, static
instances cut from the variable font; a single variable resource renders Bold
thin on device). Section titles shrink to one line (`FitText`); kickers use
the theme mono font; colours come from the active palette.

## Morning Brief deck

The deck interleaves two channel follow-ups, then one worth-a-look card. Tabs
All / For You / Worth a Look show how many cards are left. The top card sits
over up to two preview cards.

| Gesture / button | Follow-up | Worth a look |
|---|---|---|
| drag right past 95dp (or flick) · `✓ Useful` | `useful` | `open` feedback |
| drag left past 95dp · `✕ Dismiss` | `dismiss` (no reason) | `dismiss` |
| `Seen` button | `acknowledge` | `acknowledge` |
| `⚡ Do it` / `⚡ Open` | `approve` | `open`, then the source (route, URL, or the detail sheet) |
| tap | message sheet with every follow-up action | resurfacing detail sheet |

- Only a sideways drag moves the card; a mainly vertical drag scrolls the
  page. A release also commits when its velocity would carry the card past
  the threshold.
- Web's swipe-up Seen is a button here, because on a phone an upward swipe is
  indistinguishable from a scroll.
- USEFUL and DISMISS stamps fade in while dragging; ACKNOWLEDGED shows on
  Seen. A committed card flies out over 260 ms with a light haptic. With
  Reduce Motion it does not rotate and fades out instead.
- Each action goes through the existing `TodayViewModel` mutation: it removes
  the card at once, keeps the per-card lock, and records receipts and
  attribution. `triageDeckCard` refuses an already-locked card, which springs
  back. On failure the card is restored and the error dialog shows.
- With two or fewer cards left, the next follow-up and worth pages are
  fetched.
- Only the top card records an impression.

## Broadsheet

The broadsheet has its own **For You** / **Worth a Look** tabs (chips with
count badges; the choice is saved as `broadsheet_tab`). Each tab shows one
server page of 5 cards in a 460dp container that scrolls on its own, with a
ServerPager-style pager: `6–10 of 23 ‹ 2 / 5 ›`.

- **Worth a Look** pages by offset:
  `/channel-assist/resurfacing/today?limit=5&offset=…`.
- **For You** pages channel follow-ups by keyset cursor, remembering the
  cursor for each page and walking forward from the nearest known page (web
  `ensureFollowUpCursor`). Core `followups` items, few and unpaged, lead page
  1.
- These pages are separate from the deck's lists. A per-tab request
  generation drops late answers. Pages are reread on the 30 s poll and after
  every card action, so a resolved card is backfilled.

Cards keep their swipe rails and actions, with a mono kicker and serif
headline. Core `followups` items keep a snooze time picker. Channel
follow-ups have a single **Snooze — hide from Today** with no duration (the
wire carries none). Resurfacing cards cannot be snoozed; their dismiss reasons
are spam, already_handled, duplicate, delegated and not_relevant.

## Data

- `refresh()` (30 s): Today, hidden items, resurfacing, follow-ups,
  briefings, pulse, `/v2/feed?limit=80`, `/v2/agents/updates`, `/v2/agents`
  and the 24 h count. The feed supplies Activity (durable rows) and the wire
  (15 newest rows).
- Pulse (60 s) also refreshes agent counts and the 24 h count
  (`POST /v2/analytics/query`, `SELECT COUNT(*) … FROM events`; on failure the
  last value is kept). `/v3/tasks` supplies the pie buckets: succeeded =
  max(#completed, completed today), failed, and in flight
  (running/paused/planning).
- The app's one realtime connection (`RealtimeHub`, see
  [README](README.md#realtime-hub)) turns every scope-matched frame into an
  `event` wire line and increments the count, skipping keepalive and
  token-delta frames. Only Today-relevant frames, or a hub resync, trigger the
  debounced refresh.
- The wire shows only real records, never placeholder lines.

## Invariants

- Impression timers run only while at least half of a card is inside the
  window.
- Resurfacing detail, briefing, compose and writing-style sheets show a load
  error with Retry instead of spinning; digest page errors appear in § 5 with
  Retry. The action-error dialog shows even when the day never loaded.
