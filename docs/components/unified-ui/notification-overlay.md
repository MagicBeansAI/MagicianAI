# Notification Overlay (Notify Layer)

The `/notify-overlay` route and the `$lib/notify` data layer are the web/UI
side of the desktop notification overlay — a transparent, always-on-top Tauri
window (bottom-right, content-hugging, grows upward) that surfaces agent HITL
approvals (actionable) and errors / completions (informational) as push-style
cards. Window lifecycle, tray badge, and host commands live in
[`docs/components/desktop/notification-overlay.md`](../desktop/notification-overlay.md);
this page covers the Svelte route and the pure data layer.

## Layout

Two lifecycle zones:

- A **persistent approval queue** at the bottom, **earliest at the very
  bottom**. Approvals never auto-dismiss.
- **Transient toasts** (error / completion) stacked **above** it, **newest on
  top**, each auto-dismissing after its TTL.

At most **4 cards are visible**; overflow folds into **"+N more pending"**. A
transient card's dismiss timer starts **only once it is visible**.

## The Route

`/notify-overlay` (`src/routes/notify-overlay/+page.svelte`) is a chrome-less
view layer: a sibling of `(app)`, inheriting only the root layout, with global
CSS forcing a transparent background. It renders a keyed `{#each}` of
`NotifyCard` from the `cards` store and:

- **Launches Attention.** Clicking an actionable card invokes `open_app_at` with
  `/attention?attention_item=<correlationId>`. The host classifies `/attention`
  (and legacy `/approvals`) as the native Attention window
  (`magician-attention`), rewrites to
  `/attention?native_attention=1&attention_item=…`, and `AttentionCenter`'s item
  resolver opens the prompt; `(app)` renders a chrome-less
  `native-attention-pane` for `native_attention=1`. The overlay never renders a
  response form or POSTs a HITL reply, and there is no
  `open_attention_target` / `hitlTarget` payload (`openPathsContract.test.ts`
  forbids them). A successful invoke removes the card locally.
- **Shows/hides and focuses** via `show_notify_overlay` / `hide_notify_overlay`
  as the stack fills and empties; `set_notify_overlay_focusable` is true only
  while a visible card is actionable.
- **Reports height** through a `ResizeObserver` to `resize_notify_overlay`.
- **Drives the tray badge** via `set_pending_approval_count` (actionable cards
  including overflow). Self-healing: `lastPendingCount` latches only after a
  confirmed invoke (single last-write-wins flush loop), and a 15 s reconcile
  re-asserts the count.
- **Deep-links** informational cards with `open_app_at({ path })`; non-Attention
  paths open in the system browser.
- **Mirrors the theme, listen-only.** `startThemeMirror` reads `get_app_theme`
  and applies `app-theme-changed` via `applyAppTheme` (`<html data-theme>` +
  inline tokens). It must never publish back: the route is excluded from
  `themeStore.init()` (whose `MutationObserver` re-publishes `data-theme` via
  `reportThemeToDesktop`), which would otherwise close a
  broker → setAttribute → observer → re-publish loop. `applyAppTheme` no-ops when
  already on target.

All Tauri `invoke` calls are behind a dynamic-import try/catch, so the route
renders in a plain browser.

## The Data Layer (`$lib/notify`)

Pure (no I/O) except the stream transport.

### `notifyStream.ts`

Owns the V3 subscription and the `cards` store: the NDJSON tail
`GET /api/magician/v3/events?category=hitl,pipeline,execution,agentic&user_relevant=true`
for the active scope, line-buffered and folded through `applyEvent`.

- Transport (reader loop, jittered reconnect backoff, scope resolution, abort)
  is copied from `pendingHitlStore.ts`; the live tail uses the 10-minute
  `LONG_FETCH_TIMEOUT_MS`.
- Reads only the flat `data.*` layer of `RuntimeTransportEvent`
  (`#[serde(tag = "event_type", content = "data")]`).
- `category` is one comma-separated value. Severity is **not** pinned —
  `HitlResolved` is `info`, and without it actionable cards would never dismiss.
- The store has no hard cap; growth is bounded by `HitlResolved` removal, reset
  on (re)connect and scope change, and the stale-card reconcile. The 4-card cap is
  render-time.
- **`reconcileStaleCards` (20 s)** pulls `/v3/events` with `backfill_only=true`
  (~24 h, row-capped) and (1) drops any actionable card whose `correlationId` has
  a `HitlResolved` in the backfill; (2) if the backfill leads the live stream by
  more than 5 s (`latestBackfillTs` vs `lastSeenEventTs`), aborts the live
  stream so it reconnects. Fail-open: any failure prunes nothing.
- **Durable native-only dismissal.** Close × stores the correlation id in the
  overlay webview's `localStorage` (`magician.notify.dismissed-actionable.v1`)
  so stream replay cannot restore it; the HITL stays in Attention, and a
  resurfaced request with a new id appears normally.

`applyEvent(current, event, opts?)`:

- `HitlResolved` (any outcome) → remove the matching actionable card.
- otherwise `hitlEventToCard`, then `infoEventToCard`, add-or-replace via
  `coalesceCard`.
- **Informational cards are live-only:** with `opts.nowMs`, events older than
  30 s are skipped. Actionable approvals always backfill.

### `cardModel.ts`, `infoModel.ts`, `coalesce.ts`

- `kind: 'actionable'` — `HitlRequested` with a correlation / pause-state /
  approval id: `correlationId` (the complete deep-link handoff), `source`,
  `inputType`, `prompt`, optional `hint`. Historical events without a parsed
  request still surface on the id path; Attention hydrates the rest.
- `kind: 'info' | 'success' | 'error'` — `title`, optional `message`,
  `deepLink`, `dismissAfterMs`.
- `infoEventToCard`: `ExecutionFailed` / `ProcessingError` / `AgenticStepFailed` /
  `AgenticMaxIterationsReached` → `error` (~15 s); `ExecutionCompleted` with
  `success === true` → `success` (~5 s). `ChatMessageReceived` is not surfaced;
  guest requests arrive as `HitlRequested { source: 'user_request' }`.
- Keys: actionable on `correlationId`; informational on
  `${kind}:${deepLink ?? id}` — disjoint key spaces, and a completion never
  overwrites an earlier failure.

## Card presentation (`NotifyCard.svelte`)

Compact themed card on a transparent window: kind accent as two L-brackets
(`data-kind` → `--notify-accent`), no type subtitle; long body clamps to 2 lines
with inline **Show more** (a hover popover would clip) and a native tooltip;
close × is durable suppression on actionable cards and local dismiss on
informational ones.

## Resolution

The overlay does not resolve HITL; the canonical prompt on `/attention` responds
through `respondToHitl` → `postHitlResponse`. Web `/attention` rows come from
`pendingHitlStore` (live) + `attentionStore`; a successful POST drops the row from
both, and `pendingHitlStore`'s `HitlResolved` branch calls
`attentionStore.dropResolved`, so resolution from any surface (including the
native Attention window) drops the web copy on the same event.

## Debug injector

`/debug/notify` drives always-on `POST /api/magician/v3/events/debug-emit`,
injecting a synthetic `HitlRequested` / `ExecutionFailed` / `ExecutionCompleted`
onto the live V3 broadcaster (no durable `events.jsonl` write). Command palette:
**Notify debug** (Manage). The injected scope must match the overlay's
(default `anonymous` / `default`). The page also shows a live `NotifyCard`
gallery.

## Testing

`cd ui/unified-ui && npx vitest run src/lib/notify` (`cardModel`, `infoModel`,
`coalesce`, `dismiss`, `suppression`). The launch contract is in
`src/lib/attention/openPathsContract.test.ts`. Window and tray behavior are
manually verified (desktop doc).

## Key Files

| Concern | Path |
|---|---|
| Route (view layer) | `src/routes/notify-overlay/+page.svelte` |
| Card component | `src/lib/notify/NotifyCard.svelte` |
| V3 stream + `applyEvent` | `src/lib/notify/notifyStream.ts` |
| Card model + mappings | `src/lib/notify/{cardModel,infoModel,coalesce}.ts` |
| Durable native-only dismissal | `src/lib/notify/suppression.ts` |
| Debug injector page | `src/routes/(app)/debug/notify/+page.svelte` |

## Canonical References

- [Desktop notification overlay](../desktop/notification-overlay.md)
- Design (archived)
- Implementation Plan (archived)
