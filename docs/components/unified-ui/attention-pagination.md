# Attention pagination (web client)

`/attention` and the Attention modal page every category tab through the shared
`ServerPager` (`src/lib/shared/components/ServerPager.svelte`). Surfaces:
`src/routes/(app)/attention/+page.svelte` (full route) and
`src/lib/attention/AttentionCenter.svelte` (the always-mounted modal).

## Why the offset pager fits here

The feed is cursor-paged across several merged sources, but every category tab
carries a true server-side total (`FeedAttentionTotals`, folded by
`attentionCategoryCountsFromTotals`). That is a valid denominator, so
`Page 2 of 7 · 21-40 of 137` is honest per tab without an offset endpoint, and
First/Last/arbitrary jumps work by aiming the existing cursor loader further out.
(Compare [monitors](./monitors.md), which moved onto `ServerPager` once its
envelope gained a `total`.)

## The derivation

`attentionPagerView` (`src/lib/attention/pagination.ts`) is the **one** place
cursor-frontier state becomes `ServerPager` props; both surfaces call it:

- **Live cursor → the server total is the denominator:**
  `totalItems = max(categoryTotal, provenRowCount)`.
- **Exhausted cursor → loaded rows are the truth** (totals can over-count after
  client dismissals).
- **An under-reported total never disables Next:** while the cursor is live,
  `pageCount` stays at least one page ahead of the current index.

`provenRowCount` = `attentionGloballyProvenRowCount(frontiers, rows)` — rows whose
global ordering every source frontier has proven. Paging never renders past it,
so a merge-ordering artifact cannot surface as a wrong row. `maxReachablePage`
clamps `pageCount` when a buffer cap makes far pages unreachable (modal only).

## Jump semantics

`gotoInboxPage(page)` (route) / `goToPage(page)` (modal):

- **Backwards is free** — rows are buffered; no fetch.
- **Forwards** runs the same `ensureInboxWindow` / `ensureRowsForWindow` frontier
  advance with `requiredRows = (targetIndex + 1) * pageSize`, costing only the
  cursor pages in between; the generation guard + scope key invalidate late
  responses.
- **Overshoot lands, it does not strand:** `attentionJumpLandingIndex` settles on
  the furthest fully-backed page, never dragging a forward jump backwards if rows
  shrank mid-flight. The end-of-category / capacity notice still renders.

## Draining the page you are working

Resolving and dismissing remove rows from the buffer; both halves of the shared
[paged-list removal](./paged-list-removal.md) policy apply:

- **Refill.** `reconcileInboxAfterRemoval()` runs after an `activate` returning
  `resolved`/`dismissed` and after Approve-all, calling
  `pageAfterRemoval(inboxPageIndex + 1, …)` whose refetch is
  `ensureInboxWindow(page * ATTENTION_PAGE_SIZE, …)`. Skipped on a still-full page
  or while a window load is in flight.
- **Step back** fires when the rendered page is empty with nothing in flight —
  not gated on `inboxHasServerMore`, since a live cursor whose remaining rows fail
  the filter would otherwise strand the reader on an empty page.
- `inboxRemovalsInFlight` covers the one frame where the store has dropped the
  row synchronously but the awaited action has not returned, so the clamp does not
  undo the refill.

No double-fetch against the 15s poll: `attentionStore` coalesces refresh and
append (`inFlightRefresh` / `inFlightLoadMore`); the refill adds at most one
cursor append.

## Attention is live-only

Attention (route and modal) covers live, just-in-time notifications: HITL
requests/approvals/escalations and failed executions. Channel follow-ups are not
here (web or iOS); they live on Today and Town Square, which fetch
`/channel-assist/follow-ups` themselves. `all` means exactly what the badge
counts (`needs_action + failed`); `attentionCategoryCountsFromTotals` takes only
`FeedAttentionTotals`.

Client dismissals decrement `totals` and per-lane page totals
(`applyDismissedFailed`), so tab counts, pager denominator and header agree with
the badge and rows. A locally dismissed item the server still returns is
re-POSTed to `/feed/attention/dismiss` on the next payload
(`reconcileServerDismissals`), converging both dismissal stores without a
reconciliation endpoint.

## Per-surface differences

| | `/attention` route | Attention modal |
| --- | --- | --- |
| Page size | `ATTENTION_PAGE_SIZE` (20) | `ATTENTION_CENTER_PAGE_SIZE` (6) |
| Buffer cap | none | `MAX_FEED_ITEMS` (200) |
| `maxReachablePage` | omitted | `nextPageBlockedByCap ? pageIndex + 1 : undefined` |
| Search box | yes — filters client-side | no |
| Pager placement | above **and** below each list | pinned footer only |

**Search invalidates the total:** with a non-empty search the route passes
`categoryTotal: 0` and the pager uses the loaded filtered count; Next stays live
while the cursor is. **The modal's cap is deliberate:** beyond it
`capacityNotice` points to full Attention and `pageCount` is clamped so Last
never lands on a nonexistent page.

## Placement

The route puts a pager above and below the list (house style of `AutoTable`,
`EntityGrid`, thinking-maps), the top copy gated on `pageCount > 1` and sitting
under the category tabs as a toolbar row (`--top` cancels most of the column
gap). The modal keeps **one** pager in its `flex: 0 0 auto` footer inside a
fixed-height dialog, always on screen. Top/bottom pagers take distinct
`ariaLabel`s (`… (top)` / `… (bottom)`) so landmarks are unambiguous.

## Recently resolved (history)

The `All` view's resolved list is an in-memory snapshot (one `backfill_only`
fetch of `/api/magician/v3/events?category=hitl`) paged client-side over the
filtered array with its own `ServerPager` (`RESOLVED_PAGE_SIZE` 20, hidden at one
page). The index resets on refetch and clamps when a filter shortens the list.

## Accessibility

`ServerPager` keeps the `Previous page` / `Next page` labels (so the modal's
`focusPaginationControl` restores focus after an async load) and adds `First
page` / `Last page`; the modal's `aria-live` page announcement is unchanged. In
the 560px dialog the pager may shrink; under 640px the range text drops first.

## Tests

`src/lib/attention/pagination.test.ts` (`attentionPagerView`,
`attentionJumpLandingIndex`), `src/lib/attention/AttentionCenter.component.test.ts`.
