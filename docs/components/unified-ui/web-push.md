# Browser notifications (web push for asks)

While a tab is open, asks already show in the composer dock
(`ComposerAskDock`), the task panel (`TaskPanelDrawer`) and Attention. Web push
covers the closed or backgrounded tab: the browser shows a notification for a
new ask, and a plain two-way decision can be answered from the notification.
The server half (VAPID, encryption, the subscription store) is in
[HITL attention → Push → Browsers](../magician/hitl-attention.md).

## Turning it on

**Settings → Browser notifications** (`/settings`, the card above "Critical
alerts"; `src/lib/settings/BrowserNotificationsPanel.svelte`). The card shows
one state:

| State | Meaning |
|---|---|
| On / Off | This browser is / is not subscribed. **Turn on browser notifications** / **Turn off**. |
| Blocked | The site's notification permission is denied; allow it in the browser's site settings. |
| Off on this server | `GET /web-push/config` answered `enabled: false` (no VAPID keys) or 404. **Set up browser notifications** (owner) calls `POST /web-push/vapid`: the server generates a P-256 pair, writes it to its runtime env files (the private key is never returned) and turns web push on without a restart; it refuses with 409 once keys exist. |
| Not supported | No service worker / Push API, or not a secure context (HTTPS or localhost; iOS needs the Home Screen app). |
| Handled by the desktop app | The Tauri webview: the desktop app has its own notch; web push is never used there. |

Turning on (`enableWebPush`, `src/lib/webPush/client.ts`):

1. `Notification.requestPermission()` — the first thing awaited in the click
   handler, so the prompt follows the user gesture.
2. Registers `/service-worker.js` (scope `/`; `type: module` in dev).
3. `pushManager.subscribe({userVisibleOnly: true, applicationServerKey})` with
   the config's `vapid_public_key`. An existing subscription made with a
   different key is unsubscribed first.
4. `PUT /api/magician/v2/web-push/subscription`
   `{subscription: {endpoint, keys: {p256dh, auth}}, label}` (`label` such as
   "Chrome on macOS").
5. Sets `localStorage['magician:web-push:enabled'] = '1'` and mirrors the
   bearer (below).

Turning off sends `DELETE /web-push/subscription {endpoint}`, unsubscribes,
clears the flag, and deletes the mirrored bearer.

**Re-sync on load.** The `(app)` layout calls `startWebPushClient` on mount
(skipped in the native Attention window and in Tauri). If the flag is set and
permission is still granted, it re-registers the worker, reuses or recreates
the subscription, and PUTs it again (push services rotate endpoints). If
permission was revoked, it drops the flag and the mirrored bearer. The worker
also handles `pushsubscriptionchange` (PUT the new endpoint, DELETE the old).

The worker is registered only by this code. `kit.serviceWorker.register` is
`false` in `svelte.config.js`, so a browser that never opts in, and the
marketing landing, never get a service worker. The worker has no `fetch`
handler and caches nothing, so page loads and API calls behave as they did
without it.

## Auth: bearer mirrored into IndexedDB

The web app has no cookie session. Every `/api/magician/**` call carries the
bearer from `localStorage` (`magician:auth:scoped-bearer`), injected by
`installScopedApiFetch` ([auth session gate](auth-session-gate.md)). A service
worker cannot read `localStorage`. While web push is on, the page therefore
copies the bearer into IndexedDB (`magician-web-push` / `credentials` /
`scoped-bearer`, `src/lib/webPush/credentialMirror.ts`). That is the same
origin and the same exposure as `localStorage`.

- `startWebPushClient` subscribes to `scopeIdentityStore`. A new bearer from
  sign-in or a workspace switch is written (only when opted in). An empty one
  (sign-out, or a session-loss reset) deletes the copy.
- Turning off, or a revoked permission, deletes it too.
- The worker sends it as `Authorization: Bearer …` on exactly two calls: the
  ask fetch and the respond POST.

Without a mirrored bearer (or with a stale one), the fetch fails and the
generic notification stays. Clicking it opens the app.

## Service worker (`src/service-worker.ts`)

Handlers live in `src/lib/webPush/workerHandlers.ts`, with the payload and
eligibility logic in `src/lib/webPush/protocol.ts`. Both are pure, so vitest
drives them.

Everything the worker imports, including the HITL adapters it shares with the
app, must stay free of `$app/*` modules other than `$app/env/public`; SvelteKit
refuses them in service-worker code. Only a production build (`npm run build`)
compiles the worker, so dev mode does not catch a violation. That is why the
owner-private alignment read, which needs the scope store, lives in
`src/lib/hitl/alignmentDetail.ts` and not in `alignmentAsk.ts`.

**`push`, `attention_requested`:**

1. `showNotification("Magican", {body: "Magican needs your input", tag:
   correlation_id})` at once. Every push shows a notification
   (`userVisibleOnly`). An unparseable payload gets the same generic
   notification with tag `magician-web-push`.
2. `GET /feed/attention/{correlation_id}`, which may return a bare FeedItem or
   `{item}`. The ask is parsed with the app's own `hitlRequestFromFeedItem`.
   An ask nested under `metadata.hitl_request` is lifted first.
   - 404 means the ask was already answered or closed, so the notification is
     closed.
   - Any other failure keeps the generic notification.
3. The notification is shown again with the same tag (no second alert), with
   `title` = the question. It is not shown again if the reader dismissed it,
   or a resolution closed it, while the fetch was in flight.
4. Two action buttons are added only when all of these hold:
   - The push carries the server's `answer_mode` (`push_answer_mode` is the
     allow-list; no mode means the ask must be opened in the app).
   - The fetched ask matches that mode:
     - `confirm`: a `confirmation`. Buttons use `confirm_label` /
       `deny_label`, else Approve / Reject.
     - `choice`: a single-select `choice` with exactly two options, neither
       `requires_input`. Buttons use the two option labels.
   - The ask is not `sensitive`, not `destructive`, and not a form
     (`questions`).
   - `Notification.maxActions` ≥ 2. With fewer than two, one button would bias
     the answer.

   `text` asks, wider choices and everything else open the app.

**`push`, `attention_resolved` / `attention_expired`:** closes notifications
with that tag. Chrome counts a push that leaves nothing visible against the
origin's silent-push allowance; the server only sends these for asks it
announced.

**`notificationclick`:**

- **An action** posts `/hitl/{id}/respond` with `{source, input_type, value,
  channel: "web_push", execution_id?, task_id?}`. The body is built by the
  app's `resolveHitlCall`, so `task_id` rides only for clarification and plan
  approval, as on the web. The value is `{type: confirmation, confirmed}` or
  `{type: choice, selected_id}`. The notification is then replaced (silently,
  same tag):
  - "Answer sent" (2xx, accepted)
  - "Already answered" (404 / 409 / 410)
  - "Couldn't send your answer — click to open" (anything else, including a
    re-ask or `accepted: false`)

  The replacement carries no actions, so clicking it opens the ask.
- **The body** focuses an open app window and posts it
  `{type: "magician:web-push:open", path}` over a `MessageChannel`. The app
  shell opens the ask over the current page (`openResolvedAttention`) and
  acknowledges. A window that does not acknowledge within 600 ms (for example
  one on `/login` or the landing) is navigated to `web_path`. With no window,
  `clients.openWindow(web_path)` opens one. `web_path` is the server's
  `/attention?attention_item=<cid>`, and only same-origin paths are accepted.
  `?attention_item=` is the global Attention overlay param, so it opens the
  ask on any app route, `/work` included.

## Already answered on the web

`postHitlResponse` (`src/lib/hitl/adapters.ts`) still treats 404 / 409 / 410
on respond as a soft success, so the stale card drops. It now also returns
`{ok: true, alreadyClosed: true}` and shows one info toast: "This question was
already answered or closed." (`src/lib/hitl/alreadyClosedNotice.ts`). The toast
is coalesced to once per 3 s for bulk applies and double clicks. Every web
surface that answers through the adapter gets it: the dock, the task panel,
Attention, and Vibe.

## Tests

- `src/lib/webPush/protocol.test.ts`: payload parsing, eligibility, the
  respond body, and status mapping.
- `src/lib/webPush/workerHandlers.test.ts`: push → actions, `maxActions`, tag,
  close on resolve/404, click → respond body and result, and window
  focus/navigate/open.
- `src/lib/webPush/client.test.ts`: enable, disable, re-sync, key rotation,
  revoked permission, the bearer mirror, and open messages.
- `src/lib/settings/BrowserNotificationsPanel.component.test.ts`.
- `src/lib/hitl/respondToHitl.test.ts`: the already-closed flag and notice.
