# Notification Overlay (Notify Layer)

The `/notify-overlay` route and the `$lib/notify` data layer are the UI side of
the desktop notification overlay: a transparent, always-on-top Tauri window
(bottom-right, content-hugging, grows upward) that shows errors and completions
as toasts. Actionable HITL questions render in the desktop side notch, not here.
Window lifecycle, tray badge and host commands live in
[`docs/components/desktop/notification-overlay.md`](../desktop/notification-overlay.md);
this page covers the Svelte route and the pure data layer.

## Layout

The column holds **transient toasts** (error / completion), **newest on top**,
each auto-dismissing after its TTL. At most **4 toasts are visible**, and a
toast's timer starts **only once it is visible**.

## The Route

`/notify-overlay` (`src/routes/notify-overlay/+page.svelte`) is a chrome-less
sibling of `(app)` that inherits only the root layout, with a forced
transparent background. It renders a keyed `{#each}` of `NotifyCard` from the
`cards` store and:

- **Publishes the side notch.** Every card-store change calls
  `publish_notch_inbox` with `notchItemsForCards`. `notchInbox.ts` decides which
  asks the notch can answer (choice, multi-choice, confirmation, short text) and
  which stay Open in Attention (secrets, forms, diffs, file picks, or a request
  the overlay could not remember). The host posts an inline answer itself; this
  route drains `drain_notch_answers` on `notch://answer` and every 400 ms.
  An answer with `posted: true` only removes the card. Any other inline answer
  posts with the same `postHitlResponse` mapping Attention uses; a question the
  route cannot post is removed and reported as done. The open kind calls
  `open_app_at`. Dismiss suppresses the correlation id and removes the card
  without posting. There is no `open_attention_target` / `hitlTarget` payload
  (`openPathsContract.test.ts` forbids them). A posted success removes the card
  and reports `notch_report`; a failed post leaves the question up. The host
  treats `/attention` (and legacy `/approvals`) as the native Attention window
  (`magician-attention`).
- **Closes the native window.** Close Attention, Escape on the inbox, and
  finishing a directly opened item call `getCurrentWindow().close()`; the host
  hides the window and drops the Dock icon. The inbox shows Close while
  `native_attention=1`. Answering one row leaves the list up. If the host cannot
  hide the window, a direct item selection is cleared so the page is not left
  inert.
- **Shows and hides** via `show_notify_overlay` / `hide_notify_overlay` from the
  toast count. Hiding leaves the window ordered in, so its event socket and the
  notch answer drain stay alive. `set_notify_overlay_focusable` is true only
  while a visible toast has a deep link.
- **Reports height** through a `ResizeObserver` to `resize_notify_overlay`.
- **Drives the tray badge** via `set_pending_approval_count` (actionable cards
  including overflow). `lastPendingCount` latches only after a confirmed invoke
  (one last-write-wins flush loop), and a 15 s reconcile re-asserts it.
- **Deep-links** informational cards with `open_app_at({ path })`;
  non-Attention paths open in the system browser.
- **Mirrors the theme, listen-only.** `startThemeMirror` reads `get_app_theme`
  and applies `app-theme-changed` via `applyAppTheme` (`<html data-theme>` and
  inline tokens). It never publishes back:
  the route is excluded from `themeStore.init()` (whose `MutationObserver`
  re-publishes `data-theme` via `reportThemeToDesktop`), which would otherwise
  loop. `applyAppTheme` no-ops when already on target.

All Tauri `invoke` calls sit behind a dynamic-import try/catch, so the route
renders in a plain browser.

## The Data Layer (`$lib/notify`)

Pure (no I/O) except the transport.

### `notifyStream.ts`

Owns the subscription and the `cards` store. Live events come from the event
hub ([event-hub.md](event-hub.md)) with
`categories: hitl,pipeline,execution,agentic, userRelevant: true`; history is
one `GET /api/magician/v3/events?category=hitl,pipeline,execution,agentic&user_relevant=true&backfill_only=true`
per scope and per hub resync. Both fold through `applyEvent`.

- The overlay is its own webview, so it loads `eventHub.ts` without
  `v2-websocket.ts` and gets its **own** hub (one socket, reconnecting forever).
  Transport and scope handling are shared with `pendingHitlStore.ts`
  (`subscribeWithBackfill`).
- Reads only the flat `data.*` layer of `RuntimeTransportEvent`
  (`#[serde(tag = "event_type", content = "data")]`).
- `category` is one comma-separated value. Severity is **not** pinned:
  `HitlResolved` is `info`, and without it actionable cards would never
  dismiss.
- The store has no hard cap; `HitlResolved` removal, a reset on every backfill
  (mount, scope change, resync) and the stale-card reconcile bound it. The
  4-card cap is render-time.
- **`reconcileStaleCards` (20 s)** pulls `/v3/events` with `backfill_only=true`
  (the server's default 24 h window and 2,000-row cap) and (1) drops any actionable card whose `correlationId`
  has a `HitlResolved` in the backfill; (2) if the backfill leads the live
  stream by more than 5 s (`latestBackfillTs` vs `lastSeenEventTs`), calls
  `eventHub.reconnectNow()`. Any failure prunes nothing.
- **Durable native-only dismissal.** Close × stores the correlation id in
  `localStorage` (`magician.notify.dismissed-actionable.v1`) so a backfill
  cannot restore it; the HITL stays in Attention, and a request with a new id
  appears normally.

`applyEvent(current, event, opts?)`:

- `HitlResolved` (any outcome) removes the matching actionable card.
- Otherwise `hitlEventToCard`, then `infoEventToCard`, add-or-replace via
  `coalesceCard`.
- **Informational cards are live-only:** with `opts.nowMs`, events older than
  30 s are skipped. Actionable approvals always backfill.

### `cardModel.ts`, `infoModel.ts`, `coalesce.ts`

- `kind: 'actionable'`: `HitlRequested` with a correlation / pause-state /
  approval id: `correlationId` (the complete deep-link handoff), `source`,
  `inputType`, `prompt`, optional `hint`. Older events without a parsed request
  still surface on the id path; Attention hydrates the rest.
- `kind: 'info' | 'success' | 'error'`: `title`, optional `message`,
  `deepLink`, `dismissAfterMs`.
- `infoEventToCard`: `ExecutionFailed` / `ProcessingError` /
  `AgenticStepFailed` / `AgenticMaxIterationsReached` → `error` (15 s);
  `ExecutionCompleted` with `success === true` → `success` (5 s).
  `ChatMessageReceived` is not shown; guest requests arrive as
  `HitlRequested { source: 'user_request' }`.
- Keys: actionable on `correlationId`; informational on
  `${kind}:${deepLink ?? id}`. The key spaces are disjoint, and a completion
  never overwrites an earlier failure.

## Card presentation (`NotifyCard.svelte`)

A compact themed card: the kind accent as two L-brackets (`data-kind` →
`--notify-accent`), no type subtitle; a long body clamps to 2 lines with inline
**Show more** and a native tooltip. Close × is durable suppression on
actionable cards and a local dismiss on informational ones.

## Resolution

Inline notch answers and the canonical prompt on `/attention` both respond
through `respondToHitl` → `postHitlResponse`. Web `/attention` rows come from
`pendingHitlStore` (live) and `attentionStore`; a successful POST drops the row
from both, and `pendingHitlStore`'s `HitlResolved` branch calls
`attentionStore.dropResolved`, so resolution on any surface drops the web copy.

## Debug injector

`/debug/notify` drives `POST /api/magician/v3/events/debug-emit`, injecting a
synthetic `HitlRequested` / `ExecutionFailed` / `ExecutionCompleted` onto the
live V3 broadcaster (no durable `events.jsonl` write). Command palette:
**Notify debug** (Manage). The injected scope must match the overlay's
(default `anonymous` / `default`). The page also shows a live `NotifyCard`
gallery.

## Testing

`cd ui/unified-ui && npx vitest run src/lib/notify` (`cardModel`, `infoModel`,
`coalesce`, `dismiss`, `suppression`, `notchInbox`). The launch contract is in
`src/lib/attention/openPathsContract.test.ts`. Window and tray behavior are
verified by hand (desktop doc).

## Key Files

| Concern | Path |
|---|---|
| Route (view layer) | `src/routes/notify-overlay/+page.svelte` |
| Card component | `src/lib/notify/NotifyCard.svelte` |
| Hub subscription + backfill + `applyEvent` | `src/lib/notify/notifyStream.ts` |
| Card model + mappings | `src/lib/notify/{cardModel,infoModel,coalesce}.ts` |
| Durable native-only dismissal | `src/lib/notify/suppression.ts` |
| Debug injector page | `src/routes/(app)/debug/notify/+page.svelte` |

## Canonical References

- [Desktop notification overlay](../desktop/notification-overlay.md)
- Design (archived)
- Implementation Plan (archived)
