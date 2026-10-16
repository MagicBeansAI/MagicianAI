# Notification Overlay

## Purpose

The notification overlay surfaces backend agent events as push-style cards on
an always-on-top transparent Tauri window, so the user can act on them from the
desktop instead of missing them in the web UI.

It carries two card families:

- **Actionable** — agent HITL approvals (`HitlRequested`). These do not render
  in the overlay column. The overlay keeps the request and publishes a notch
  item (`publish_notch_inbox`); the side notch
  ([mac-notch-orb.md](mac-notch-orb.md)) shows the question and, for a choice,
  confirmation or short text, the answer controls, and the host posts an
  inline answer (`notch://answer`). Secrets, forms, diffs and file picks stay
  **Open in Attention** (`open_app_at` with
  `/attention?attention_item=<correlationId>`). Suppression of a resurfaced id
  still lives in the overlay store.
- **Informational** — errors, failures and completions, rendered as transient
  toasts that auto-dismiss. Clicking one calls `open_app_at` with the card's
  `deepLink` (non-Attention paths open in the system browser).

**Layout.** A **300pt** column anchored **bottom-right** that grows
**upward**. Toasts stack **newest on top**; at most **4 are visible**. Each
toast's auto-dismiss timer starts **only once the card is visible**. The
window is shown only when a toast is waiting. The tray badge still counts
unresolved actionable items.

The web side (the `/notify-overlay` Svelte route, the `lib/notify` data layer
and the card components) is documented in
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
   └─ tray slice (Rust)  → menu-bar icon counter (set_title)
```

The window subscribes to the server-filtered V3 event stream and maps each
event to a card. Rust owns window lifecycle, the native Attention window for
`/attention` paths, and the tray badge.

## The Overlay Window

A `WebviewWindow` (label `notify-overlay`) created in
`desktop/src-tauri/src/overlay.rs` (`initialize_notify_overlay`), like the
contextual-assist window: `decorations(false)`, `transparent(true)`,
`always_on_top(true)`, `skip_taskbar(true)`, no shadow. It is spawned hidden at
app start.

- **Above the draw overlay.** The draw/tutor overlay sits at
  `NSFloatingWindowLevel` (3); the notify overlay uses `NSStatusWindowLevel`
  (25) so cards paint above that dim.
- **Content-hugging, not full-screen.** 300pt wide, pinned bottom-right of
  the **primary** monitor (`NOTIFY_OVERLAY_SCREEN_MARGIN` 12pt,
  `NOTIFY_OVERLAY_BOTTOM_MARGIN` 24pt), resized to the card stack height
  (`resize_notify_overlay`). With no toasts the window stays ordered in at
  alpha 0 and ignores clicks; ordering it out would suspend the page that
  keeps the side notch's questions live. With toasts it is opaque and
  clickable.
- **Passive focus.** Created non-focusable. The route flips
  `set_notify_overlay_focusable(true)` while a visible toast has a deep link.
- **Main thread.** macOS window creation and NSWindow setters are
  main-thread-only, so `initialize_notify_overlay` hops to the main thread.

## Tauri Commands

Commands live in `desktop/src-tauri/src/overlay.rs` except `open_app_at`,
`set_pending_approval_count` and `get_notifications_enabled`, which live in
`desktop/src-tauri/src/tray.rs`. There is no `open_attention_target`,
`get_pending_attention_intent` or `acknowledge_attention_intent`.

| Command | Args | Purpose |
|---|---|---|
| `show_notify_overlay` | — | Ensure the window exists, then show toasts (alpha 1, clicks on) via `show_notify_overlay_window`. On macOS it orders front without becoming key. |
| `hide_notify_overlay` | — | Hide toasts without ordering the window out, so the page keeps running. Never destroys the window. |
| `set_notify_overlay_focusable` | `focusable: bool` | Toggle keyboard focus. Passive by default. |
| `resize_notify_overlay` | `height: f64` | Hug the card stack (300pt × height) and re-pin bottom-right. |
| `set_pending_approval_count` | `count: u32` | Menu-bar pending-approval counter (`set_title`). |
| `get_notifications_enabled` | — | Seed the route's notifications-enabled mirror (see below). |
| `open_app_at` | `path: String` | `/attention` and legacy `/approvals` (optional `attention_item` / `approval_id` / `correlation_id`) open the native Attention window at `/attention?native_attention=1&attention_item=…`; other paths open in the browser. Completion is reported only after launch succeeds, so the overlay can keep a card on failure. |
| `publish_notch_inbox` | `items` | Replace the side-notch Attention list. A new id opens the panel; the same ids do not reopen a panel the user closed. |
| `notch_answer` | `answer` | Post an inline answer from the host, report the result to the notch, then queue it. `posted: true` means the host already posted. Dismiss and Open are queued without a host post. |
| `notch_report` | `id`, `ok`, `message?` | Tell the notch whether the answer landed. Dismiss and unpostable questions report success so the field unlocks. Success drops the question. |
| `drain_notch_answers` | — | Take answers queued by `notch_answer`, drained on the `notch://answer` event and a short timer. A host-posted answer only drops the card. |

## Tray Counter and the Notifications Flag

`set_pending_approval_count` stores the count in
`AppState::pending_approval_count` and writes it onto the menu-bar icon via
`TrayIcon::set_title`. At `count == 0` it uses `set_title(Some(""))`, not
`None`: on macOS `NSStatusItem`, `set_title(None)` is a no-op that leaves the
old number painted.

There is no tray Show/Hide item. `AppState::notifications_enabled` is seeded
at startup from `config.general.notifications_enabled` (default `true`). The
route reads it with `get_notifications_enabled` and listens for
`notifications-enabled-changed`, but the host never emits that event, so the
flag changes only by editing the config and restarting. When it is `false` the
toasts stay hidden; the stream, card stack and icon counter keep running.

## Data Flow

1. **Subscribe.** The route opens
   `GET /api/magician/v3/events?category=hitl,pipeline,execution,agentic&user_relevant=true`
   for the active scope: backfill of still-pending items, then live tail.
2. **Render.** Events map to a `NotifyCard` (`hitlEventToCard` or
   `infoEventToCard`), coalesced so a re-emitting source shows one updating
   card.
3. **Act (actionable).** The route publishes `notchItemsForCards` (Ask user
   and HITL) through `publish_notch_inbox`. The notch calls `notch_answer`;
   the host posts the inline answer to
   `POST /api/magician/v2/hitl/{id}/respond` and reports to the notch at once.
   A 404, 410 or `already_resolved` drops the question. The route drains the
   queue: a `posted` answer only removes the card; otherwise it posts with the
   remembered `HitlRequest` (the same mapping as Attention). A question it
   cannot post is removed and reported done. Open-in-Attention answers call
   `open_app_at`. A `kind: dismiss` answer suppresses that correlation id on
   this Mac, removes the card, posts no HITL response and reports success, so
   Dismiss stays usable while a submit is in flight. A successful answer or
   open removes the card; a failed post reports `notch_report` and leaves the
   question up.
4. **Cross-surface sync.** The overlay and `/attention` consume the same
   stream, so resolving on either emits `HitlResolved` and the other drops its
   card. The window stays ordered in while toasts are hidden, so this reaches
   the notch without a restart.
5. **Auto-dismiss.** Informational cards carry `dismissAfterMs`, timed only
   once visible. Approval cards never auto-dismiss.
6. **Deep-link.** Informational click → `open_app_at({ path })`. Actionable
   click → native Attention window: the host adds `native_attention=1` and
   `attention_item`, and the root layout renders a chrome-less
   `native-attention-pane`. Window label `magician-attention` ("Magican —
   Needs You"), 820×760, min 620×560, max 1100×900; an existing window is
   navigated and re-presented, not recreated.

## Debug Injector

- `POST /api/magician/v3/events/debug-emit` (always on) emits a synthetic
  `HitlRequested` / `ExecutionFailed` / `ExecutionCompleted` onto the live V3
  broadcaster (no durable `events.jsonl` write) for a scope.
- The `/debug/notify` page drives it, from the command palette **"Notify
  debug"** (Manage).

## Scope & Delivery

- **Scope must match exactly.** The overlay subscribes at the active
  `principal` / `workspace` (desktop default `anonymous` / `default`); the
  backend filters by strict equality.
- **The live tail is a broadcast.** Late subscribers miss emitted events;
  there is no replay beyond the pending-item backfill.
- **Same-origin transport.** The overlay reaches the backend over the main
  app's origin (in dev, the Vite `:5173` proxy to backend `:3002`).

## App Lifecycle (relied upon)

The overlay and tray badge deliver only while the menu-bar app runs.
Magician is a menu-bar (Accessory) app with no Dock icon by default. Closing
the window, Cmd-Q and Dock → Quit only **hide** it (`CloseRequested` →
`prevent_close()`; the terminate signal is intercepted and demotes back to
Accessory). Only the tray's **"Quit Magican"** terminates. The tray counter
is the ambient indicator when the window is hidden or notifications are off.

## Known Limitations

- **Primary monitor only.**
- **Out of scope:** native macOS Notification Center, Focus /
  Do-Not-Disturb, a notification history, snooze / mute, cross-device push,
  and Windows / Linux parity.
- **Tests.** `tray.rs` covers Attention path classification, URL rewrite
  (`native_attention=1` + `attention_item`) and window reuse; a UI contract
  test forbids `open_attention_target` / `hitlTarget` on the overlay route.

## Key Files

| Concern | Path |
|---|---|
| Window builder, NSWindow level, show/hide/focus/resize | `desktop/src-tauri/src/overlay.rs` |
| Tray icon counter, `get_notifications_enabled`, `open_app_at`, native Attention window | `desktop/src-tauri/src/tray.rs` |
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
