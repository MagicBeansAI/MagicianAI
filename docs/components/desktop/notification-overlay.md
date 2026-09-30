# Notification Overlay

## Purpose

The notification overlay surfaces backend agent events as push-style cards on
an always-on-top transparent Tauri window, so the user acts on them from the
desktop instead of leaving them unseen in the web UI.

It carries two card families:

- **Actionable** — agent HITL approvals (`HitlRequested`). Whole-card click
  opens the canonical `/attention` page in a dedicated native Attention
  window (`magician-attention`) with `?attention_item=<correlationId>`. The
  overlay renders no response controls. Close × persistently suppresses only
  that native correlation id without resolving the underlying HITL; a
  resurfaced request with a new id remains visible.
- **Informational** — errors / failures and successful completions, rendered
  as transient toast cards that auto-dismiss. Clicking one calls
  `open_app_at` with the card's `deepLink` (non-Attention paths open in the
  system browser).

**Layout.** The overlay anchors **bottom-right** in a
narrow **300pt** column that grows **upward**:

- **Persistent approval queue (anchored at the bottom).** Actionable HITL
  cards are grouped **oldest first** at the very bottom. They never
  auto-dismiss — only `HitlResolved` (any surface) or expiry removes them.
- **Transient zone (stacked above the queue).** Error / completion toasts
  stack above the approvals, **newest on top**, and auto-dismiss after
  their TTL.

At most **4 cards are visible**; overflow folds into a **"+N more pending"**
indicator. Each transient card's auto-dismiss timer starts **only once the
card is actually visible**.

The companion web/UI side (the `/notify-overlay` Svelte route, the
`lib/notify` data layer, and the card components) is documented in
[`docs/components/unified-ui/notification-overlay.md`](../unified-ui/notification-overlay.md).

## Architecture

```
backend (:3002)
  └─ GET  /api/magician/v3/events?category=hitl,pipeline,execution,agentic&user_relevant=true
           (NDJSON: backfill + live tail)
        ▲
        │ subscribe (fetch stream)
  Tauri desktop process
   ├─ notify-overlay  (WebviewWindow, transparent, always-on-top, NSStatusWindowLevel 25)
   │     hosts the /notify-overlay Svelte route — owns the stream + render + launch
   └─ tray slice (Rust)  → menu-bar icon counter (set_title) + Show/Hide Notifications toggle
```

The window subscribes to the backend's server-side-filtered V3 event stream
and maps each event to a card. The Rust side owns window lifecycle, the
native Attention window for `/attention` paths, and a thin tray badge for
unresolved actionable items.

## The Overlay Window

A dedicated `WebviewWindow` (label `notify-overlay`), created in
`desktop/src-tauri/src/overlay.rs` (`initialize_notify_overlay`), modeled on
the contextual-assist window: `decorations(false)`, `transparent(true)`,
`always_on_top(true)`, `skip_taskbar(true)`, no shadow. Spawned hidden at
app start and shown on the first card.

- **Window level above the draw/tutor overlay.** The draw overlay sits at
  `NSFloatingWindowLevel` (3). The notify overlay raises its NSWindow level
  to `NSStatusWindowLevel` (25) so cards paint **above** that dim.
- **Content-hugging, bottom-right — not full-screen.** Fixed **300pt**
  width, pinned to the **bottom-right** of the **primary** monitor
  (`NOTIFY_OVERLAY_SCREEN_MARGIN` 12pt, `NOTIFY_OVERLAY_BOTTOM_MARGIN`
  24pt). It resizes to the measured height of the card stack
  (`resize_notify_overlay`) and hides at zero cards. The overlay does
  **not** `set_ignore_cursor_events(true)` — the cards must remain
  clickable.
- **Passive focus.** Created non-focusable. The route flips
  `set_notify_overlay_focusable(true)` while a visible card is actionable.
- **Main-thread safety.** macOS window creation + NSWindow setters are
  main-thread-only, so `initialize_notify_overlay` hops to the main thread
  when called from a worker.

## Tauri Commands

Commands live in `desktop/src-tauri/src/overlay.rs` except
`open_app_at`, `set_pending_approval_count`, and
`get_notifications_enabled`, which live in
`desktop/src-tauri/src/tray.rs`. There is no `open_attention_target`,
`get_pending_attention_intent`, or `acknowledge_attention_intent`.

| Command | Args | Purpose |
|---|---|---|
| `show_notify_overlay` | — | Ensure the window exists, then show it. Shared helper: `show_notify_overlay_window`. |
| `hide_notify_overlay` | — | Hide the window (it is never destroyed). |
| `set_notify_overlay_focusable` | `focusable: bool` | Toggle keyboard focus. Passive by default. |
| `resize_notify_overlay` | `height: f64` | Hug the card stack (300pt × reported height) and re-pin bottom-right. |
| `set_pending_approval_count` | `count: u32` | Menu-bar icon pending-approval counter (`set_title`). |
| `get_notifications_enabled` | — | Seed the overlay's Show/Hide mirror. Live flips arrive as `notifications-enabled-changed`. |
| `open_app_at` | `path: String` | Route a UI path. `/attention` and legacy `/approvals` (optional `attention_item` / `approval_id` / `correlation_id`) open the bounded native Attention window at `/attention?native_attention=1&attention_item=…`. Every other path opens as a browser URL. Completion is reported only after launch succeeds, so the overlay can retain a card on failure. |

## Tray Counter + Show/Hide Toggle

`set_pending_approval_count` stores the count in
`AppState::pending_approval_count` and writes it onto the **menu-bar icon**
via `TrayIcon::set_title`. At `count == 0` the title is cleared with an
**empty string** (`set_title(Some(""))`), not `None`: on macOS
`NSStatusItem`, `set_title(None)` is a silent no-op that leaves the
previously-painted number on the icon.

The tray menu item is a **Show/Hide toggle** (`toggle_notifications`):
label flips between **"Show Notifications"** and **"Hide Notifications"**,
with the pending count as a ` · N` suffix. **Show** surfaces the overlay
and unmutes it; **Hide** mutes it — the overlay no longer pops, but the
icon counter keeps climbing. State lives in
`AppState::notifications_enabled`, is seeded from persisted config
(`config.general.notifications_enabled`), and the overlay route honors it
(`get_notifications_enabled` + `notifications-enabled-changed`).

## Data Flow

1. **Subscribe.** The route opens the NDJSON tail
   `GET /api/magician/v3/events?category=hitl,pipeline,execution,agentic&user_relevant=true`
   for the active scope. Backfill replays still-pending items first, then
   it live-tails.
2. **Render.** Each event maps to a `NotifyCard` — actionable via
   `hitlEventToCard`, informational via `infoEventToCard` — and is
   coalesced so a re-emitting source shows one updating card.
3. **Act (actionable).** Whole-card click invokes `open_app_at` with
   `/attention?attention_item=<correlationId>`. The overlay never submits
   a HITL response. On a successful invoke the overlay removes that card
   from its store (close × is the durable suppression path).
4. **Cross-surface sync.** Both the overlay and `/attention` consume the
   same stream, so resolving on either emits `HitlResolved` → the other
   dismisses its card.
5. **Auto-dismiss.** Informational cards carry `dismissAfterMs`; the route
   schedules the timer only once the card is visible. Approval cards never
   auto-dismiss.
6. **Deep-link.** Informational click → `open_app_at({ path })` (browser
   for non-Attention paths). Actionable click → native Attention window: the
   host adds `native_attention=1` and the `attention_item` query, and the root
   layout renders a chrome-less `native-attention-pane` when that flag is
   set. Window label `magician-attention` ("Magican — Needs You"),
   820×760, min 620×560, max 1100×900; an existing window is navigated and
   re-presented rather than recreated.

## Debug Injector

To exercise the overlay without driving the real agent / HITL path:

- `POST /api/magician/v3/events/debug-emit` (always on) emits a synthetic
  `HitlRequested` / `ExecutionFailed` / `ExecutionCompleted` onto the live
  V3 broadcaster (no durable `events.jsonl` write) for a given scope.
- The UI `/debug/notify` page drives it, reachable from the command
  palette via **"Notify debug"** (Manage).

## Scope & Delivery

- **Scope must match exactly.** The overlay subscribes at the active
  `principal` / `workspace` (desktop default `anonymous` / `default`);
  the backend filters by **strict scope equality**.
- **The live tail is a broadcast.** Late subscribers miss already-emitted
  events; there is no replay beyond the initial pending-item backfill.
- **Same-origin transport.** The overlay window reaches the backend over
  the same origin as the main app (in dev, the Vite `:5173` proxy to
  backend `:3002`).

## App Lifecycle (relied upon)

The overlay and the tray badge deliver only while the menu-bar app runs:

- Magician is a menu-bar (Accessory) app — menu-bar icon, no Dock icon by
  default.
- **Closing the window, Cmd-Q, and Dock → Quit only HIDE it**
  (`CloseRequested` → `prevent_close()`; the terminate signal is
  intercepted and demotes back to Accessory).
- **Only the tray's "Quit Magician"** truly terminates the process.

The tray icon's pending-approval counter is the ambient indicator when the
main window is hidden or notifications are muted.

## Known Limitations

- **Primary monitor only.** `resize_notify_overlay` pins to the
  bottom-right of the primary monitor.
- **Out of scope (YAGNI):** native macOS Notification Center, Focus /
  Do-Not-Disturb, a notification history center, snooze / mute,
  cross-device / mobile push, and Windows / Linux parity.
- **Tests.** `tray.rs` covers Attention path classification, URL rewrite
  (`native_attention=1` + `attention_item`) and bounded window reuse; a UI
  contract test forbids `open_attention_target` / `hitlTarget` on the overlay
  route.

## Key Files

| Concern | Path |
|---|---|
| Window builder, NSWindow level, show/hide/focus/resize | `desktop/src-tauri/src/overlay.rs` |
| Tray icon counter, Show/Hide toggle, `open_app_at`, native Attention window | `desktop/src-tauri/src/tray.rs` |
| Overlay route (view layer) | `ui/unified-ui/src/routes/notify-overlay/+page.svelte` |
| Card component | `ui/unified-ui/src/lib/notify/NotifyCard.svelte` |
| V3 stream reader + `applyEvent` reducer | `ui/unified-ui/src/lib/notify/notifyStream.ts` |
| Event → card mapping | `ui/unified-ui/src/lib/notify/{cardModel,infoModel,coalesce}.ts` |
| Durable native-only dismissal | `ui/unified-ui/src/lib/notify/suppression.ts` |
| Debug injector page | `ui/unified-ui/src/routes/(app)/debug/notify/+page.svelte` |
| Backend debug-emit endpoint | `POST /api/magician/v3/events/debug-emit` |

## Canonical References

- Design (archived)
- [Unified UI notify layer](../unified-ui/notification-overlay.md)
- [Desktop README](README.md)
