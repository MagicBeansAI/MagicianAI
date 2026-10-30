# Magicutor Docs

Landing page for `magicutor` documentation.

## Canonical References

- [Magicutor Overview](../../../magicutor/README.md)
- Retired Canvas Mode Execution Board
- Popup to Side Panel Migration Checklist (archived 2026-05-20)
- [Side Panel Live Preview](sidepanel-live-preview.md)
- [Trusted credential delivery CDP scope alias](jit-browser-scope-alias.md)
- Archived Dashboard Live Previews Checklist
- Archived Chromium Executor Enhancement Plan
- Session Tab Tracking
- iPhone Automation Research
- [Extension README](../../../magicutor/extension/README.md)
- [Extension Architecture](../../../magicutor/EXTENSION_ARCHITECTURE.md)
- [Quick Start](../../quickstart.md), [Deployment](../../DEPLOYMENT.md)

## Browser Sidecar Runtime

Browser automation is selected through Magician's `browser` skill and executed
by the pinned `agent-browser` CLI. Magicutor is the sidecar:

- CDP proxy routes for agent-browser CDP mode, with per-thread owned-tab
  isolation.
- Extension bridge and debugger event forwarding for the user's normal Chrome
  profile.
- Per-thread network trace buffering and drain for API mining.
- Per-thread page-signal buffer and drain for ambient page identity (the route
  exists; the extension has no emitter).
- Session/tab bookkeeping, activity state, overlays, and live preview support.

Not owned by Magicutor (agent-browser and the browser skill own them): the
high-level browser action runtime, spatial/canvas actions, iframe-scoped
click/type/observe, selector retry, native file dialogs
(use CDP/file-input uploads; host automation belongs to the macOS host bridge),
and download interception (the extension never cancels, rewrites or registers
page downloads; see
Retired Download Architecture).
There is no auth-capture or auth-refresh action: auth capture is passive CDP
observation (below).

**Endpoint discovery.** On service-worker startup and periodic bridge health
checks the extension reads the versioned contract from the desktop host gateway,
`http://127.0.0.1:3017/host/runtime/endpoints` (loopback HTTP/WebSocket URLs
only). The last valid contract is cached across desktop startup gaps, compiled
defaults (`3002`/`3003`) are the final fallback, and a port change retires and
reconnects the bridge.

**Several browsers.** Up to `bridge.max_connections` browsers (default 4, e.g.
two Chrome profiles, Comet and Dia) hold the bridge at once, each with its own
connection, requests and CDP events. The handshake carries `browser_id` (random,
per install), `browser_key` (a per-install secret, never echoed; Magicutor
keeps a keyed hash) and `browser_label` (the brand, or Settings → Browser name).

- A browser beyond the limit gets HTTP 409 `bridge_full`.
- A reconnect with the same `browser_id` replaces only its own socket, and only
  with the same `browser_key` after 20 s without inbound traffic; anything else
  reusing an id (no key, wrong key, a copied profile) gets 409
  `browser_id_in_use`. No browser can answer or cancel another's requests.
- A silent connection closes after 90 s. An extension without an id connects as
  `legacy-browser-<n>`: never pinned, never replacing another connection.

Public payloads name browsers by label and a 16-hex-digit fingerprint, never by
id. Selectors (`?browser=`) take the id, fingerprint, label or a unique label
prefix; an ambiguous one selects nothing. The longest-held accepted browser is
the default. Each CDP thread is pinned to one browser (`?browser=`,
`PUT /cdp/threads/{id}/browser`, bind-tab's `browserId`, else the default) when
its CDP session connects, and never moves while that session is open: if the
browser leaves, requests fail until it returns. An idle thread whose browser is
gone is re-pinned on next use. Tabs, windows and event subscriptions are keyed
per browser, and teardown goes only to the browser that served the session.

**Trust boundary.** The bridge token, trusted peers and the `Origin` check are
the boundary; pairing is owner UX on top.

- *Origin:* every route except `/health` and `/json/version` (the bridge
  WebSocket, devtools, `/json*`, `/cdp/threads/*`, `/bridge/browsers/*`,
  `/contextual-assist/*`, the drains) refuses a present `Origin` that is not a
  browser-extension origin, so a visited website cannot act as a browser or
  drive tabs. Requests without `Origin` (CDP clients, Magician) are allowed.
- *Trusted peers:* loopback, plus `bridge.trusted_peers` /
  `MAGICUTOR_TRUSTED_PEERS` (addresses or CIDRs; empty by default). Container
  launchers that publish Magicutor on `127.0.0.1:<port>:3003` set
  `MAGICUTOR_TRUST_GATEWAY=1` so the entrypoint trusts the gateway; never with
  `--network host`.
- *Host:* peer-based trust also needs `Host` to be `localhost`, an IP literal or
  a name in `bridge.allowed_hosts` / `MAGICUTOR_ALLOWED_HOSTS` (blocks DNS
  rebinding). A request with the configured token is accepted with any `Host`.
- *CORS:* no route sends CORS headers, so websites cannot read responses.
- *Gates:* the bridge and every automation route need the configured token or a
  trusted peer. Automatic acceptance and in-panel consent count only from a
  trusted connection.

**Pairing.** A browser not seen before connects `pending`: it holds a slot but
gets no work, and only its consent, rename and ping frames are processed. Its
extension asks “Connect this browser to Magician?” (Accept / Decline) with a
toolbar badge. Accept (from a local peer) records the browser in
`<data root>/magicutor/bridge_browsers.json` (owner-only;
`MAGICUTOR_BROWSER_STORE_PATH` overrides), so it is accepted at once after
restarts; Decline disconnects until the owner clicks Connect. Remote pairing:
`GET /bridge/browsers`, `POST /bridge/browsers/{fingerprint}/accept|revoke`.
Magician raises a “A new browser wants to connect” decision card when a run
finds a pending browser, answerable from the phone or web. Revoking returns a
browser to pending; a thread with a live session keeps its pin and is refused
rather than reaching another browser.

Automatic acceptance (marked `auto_accepted`, revocable) applies only to an
identified loopback browser, and only while the store can be saved:

- *First run:* the first browser ever is accepted, once (revoking all does not
  re-arm it).
- *Upgrade:* for 10 minutes after the store is created, up to
  `bridge.max_connections` reconnecting browsers are accepted, so browsers from
  before pairing keep working. A pairing decision or restart closes the window.

Legacy (id-less) extensions keep default routing during that window and are
pending afterwards.

**Per-run browser choice.** When an attended run (started by the owner from chat
or a device) is about to connect, more than one accepted browser is connected,
and neither the thread nor the URL names one, Magician calls
`POST /cdp/threads/{id}/choose` (`{task, timeout_secs}`). Every accepted browser
shows “Use this browser for '<task>'?” (Accept / Always use this browser), and
Magician raises the same choice as a decision card answered through
`PUT /cdp/threads/{id}/browser`. The first answer pins the thread; “Always” also
sets the persisted default. After `bridge.browser_choice_timeout_secs` (default
20, max 120) the default browser is used. Magician polls
`GET /cdp/threads/{id}/browser` until pinned (timeout + 3 s); any error falls
back to the default, so the choice never fails a run. Unattended runs
(scheduled, ambient, autonomous, public, `Plane`-harness, delegated children
without a chat) never ask; `MAGICIAN_BROWSER_CHOICE=never` turns asking off.
Entry point: `primitive_dispatch::browser::browser_choice::prepare_browser_choice`.

**Hardening note.** The seed config binds `0.0.0.0` with an empty
`bridge.auth_token`; then only trusted peers reach the bridge and automation
routes, while `/health` and `/json/version` answer anyone who can reach port
3003. The browser key protects an existing browser's connection; it does not
authenticate new ones. Where other machines share the network, bind
`127.0.0.1` and set a real token.

**Known gaps.**

- The drains (`GET /trace/drain/{id}`, `/auth/drain/{id}`,
  `/ambient/page/drain/{id}`) are GETs: a website that knows a thread id can
  empty one with a cross-site `<img>` (nothing leaks, but events are lost).
- Any browser-extension origin is accepted, not only Magican's, because
  unpacked extension ids differ per machine.

## Server routes

- `GET /health` — process liveness, `extension_connected` (any browser),
  `browsers` (fingerprint, label, `connected_at`, slot, default,
  `pinned_threads`, `state` = accepted|pending|legacy, `this_browser`) and
  `max_browsers`. `?browser_id=` marks
  the caller's own entry as `this_browser`; ids are never listed.
- `GET /bridge/native` — the extension's WebSocket bridge.
- `GET /bridge/browsers`, `POST /bridge/browsers/{fingerprint}/accept`,
  `POST /bridge/browsers/{fingerprint}/revoke` — owner pairing management
  (bridge token): label, fingerprint, state, connected, always.
- `GET /json/version`, `GET /json`, `GET /json/list` — CDP discovery; an
  optional `?browser=` picks the browser.
- `GET /devtools/browser/{id}`, `GET /devtools/page/{tab_id}` — CDP-compatible
  WebSocket attachment; an optional `?browser=` picks the browser (404
  `browser_not_found`, 409 `browser_ambiguous` or
  `thread_pinned_to_other_browser`, 503 `bridge_not_connected`).
- `POST /cdp/threads/{thread_id}/bind-tab`, `DELETE /cdp/threads/{thread_id}` —
  thread-scoped tab ownership. Clearing a thread also releases its browser pin
  when no CDP session is open on it.
- `PUT /cdp/threads/{thread_id}/browser` — pin a thread to a browser before its
  first request (`{"browserId": …}`, or `"default"`); the extension does this
  for runs started from its panel, and it settles a waiting browser choice.
- `GET /cdp/threads/{thread_id}/browser` — the thread's pin, `would_ask`, the
  connected browsers and `choice_timeout_secs`; Magician reads it before a run
  connects.
- `POST /cdp/threads/{thread_id}/choose` — start a per-run browser choice
  without waiting (`status`: `asked`, `open`, `not_asked`).
- `POST /contextual-assist/probe`, `POST /contextual-assist/capture-tab` — the
  active tab's contextual-writing eligibility and an on-demand tab capture,
  request/response through the extension. Without `?browser=` the probe asks
  every browser and the one whose window has focus answers; results carry
  `browser` (a fingerprint), and a capture with no browser goes to the browser
  that answered the last probe.
- `GET /trace/drain/{thread_id}` — API-mining traces.
- `GET /auth/drain/{thread_id}` — one-shot transient auth material into
  Magician's encrypted secret store.
- `GET /ambient/page/drain/{thread_id}` — page-signal buffer (currently unfed).

## CDP proxy owned tabs

Thread-scoped CDP sessions isolate tabs in `THREAD_OWNED_TABS`
(`magicutor/src/server/cdp_proxy.rs`):

- **Tier 1:** explicitly created tabs, or one extension-bound existing tab
  (`THREAD_BOUND_TABS` via `bind-tab`); unrelated tabs in the same window stay private.
- **Tier 2:** opener descendants, including popups in other windows; the
  extension's `chrome.storage.session` ledger survives worker reconnects. A
  popup that opens within 60 s after the thread's tabs were closed is closed
  too; after a handoff it is left to the owner.
- **Tier 3:** the user's other tabs, other executions, and `noopener` popups —
  invisible to this thread.

`check_tab_ownership` gates `Target.attachToTarget` and `Target.closeTarget`. An
empty owned set fails closed. `Target.createTarget` records the created tab;
`Browser.close` never reaches the owner's Chrome. `DELETE /cdp/threads/{thread_id}`
closes only automation-created tabs (Chrome removes a window when its last tab
closes), preserves bound owner tabs, and clears state only after confirmed cleanup.
`?keep_tabs=true` hands pages to the owner while releasing automation ownership.
A disconnected bridge or partial cleanup returns an error and retains retry state.

Before reusing a cached dedicated window, Magicutor checks the extension's live
tab list: a window with no tabs is forgotten and recreated; an unanswered check
keeps it (recreating would not fix a disconnected bridge).

**Tab groups.** A thread's first tab opens per `bridge.automation_surface` (default `auto`). That is a background tab in the owner's window, in a group named `✱ <task>`, with focus emulation so it keeps rendering. It falls back to a grouped window of its own, opened without focus, and then to a plain window. See `magicutor/EXTENSION_ARCHITECTURE.md`. Magician sends `?label=<name>` on the CDP URL: the conversation's title for a chat launch, else the task's title. A group without one falls back to the run's agent name.

**Iframes.** The proxy records OOPIF session ids from Chrome auto-attach
`Target.attachedToTarget` events with `targetInfo.type == "iframe"`, and forwards
only those Chrome-issued ids on `chrome.debugger.sendCommand` (proxy-minted
main-tab ids would be rejected). `debugger.js` forwards `Target.*` and other
non-flood events to the bridge; it does not refresh iframe metadata on
`Target.targetInfoChanged`. Each forwarded event carries `sessionId` when a
child target (an iframe) emitted it. The proxy keeps that id on the event, and
labels the tab's own events with the tab's main session, never an iframe
session on the same tab. Otherwise a page's `Page.loadEventFired` could reach
agent-browser under the iframe's session, and `open` timed out on a loaded page.

## CDP proxy debugger release

`Target.attachToTarget` is proxied as `attach_debugger` (`chrome.debugger.attach`,
which shows Chrome's "is being debugged" bar). Release is proxied too:

- When the CDP client's websocket closes (`CdpSession::stopped` — run cleanup
  closed the agent-browser session, or the CLI died), the proxy first sends
  `session_ended {sessionId}`, then `detach_debugger` for every top-level tab the
  session attached (`tabs_to_detach`; iframe children ride their parent). The
  ending goes first because `detach_debugger` clears the session's tab registry.
  Neither happens while another connection of the same thread is open (e.g. a
  secure fill's side connection sharing the scope via `cdp_scope_alias`): the
  automation UI and each tab's `chrome.debugger` attachment belong to the
  thread, and the last connection to close releases them.
- An explicit `Target.detachFromTarget` names its session in `params.sessionId`
  (or every session on `params.targetId`; a message-level `sessionId` is still
  read). It releases a top-level tab no other session still references, and
  only on the extension side when no other connection of the thread is open.
- On `session_ended`, the extension's `endAutomationSession` asks Magician for the
  execution status (2 s budget): terminal → the real ending (`overlay_status`
  success/error and the aurora animation) on every tab the execution may show on
  (session tabs, tracked tab, its dedicated window); unreachable →
  `overlay_panel_hide` + `overlay_hide`; paused or waiting-children → the panel
  stays. Then the execution is untracked and the session cleared.

Teardown logs (info): `[cdp-proxy] session stopped thread=… detaching debugger from N
tab(s) [ids]` (or `… another session of the thread is open, keeping …`), `[cdp-proxy] detached debugger tab=N`, and `[cdp-proxy] automation
UI ended thread=… result={ended, executionId, status, tabs}`; a
`detach_debugger tab=N failed: …` warning names the extension's reason. The
extension's `list_tabs` `debuggerAttached` flag is cleared even when
`chrome.debugger.detach` fails, so it is not proof on its own.

## Network trace, auth and page-signal capture

When `agent-browser` connects through
`ws://127.0.0.1:3003/devtools/browser/{thread-id}`, the proxy subscribes to
request/response/loading events for the attached tab, redacts sensitive headers,
lazily fetches bodies via `Network.getResponseBody`, caps each per-thread buffer,
and serves `GET /trace/drain/{thread_id}` for Magician to drain at browser
inner-loop boundaries. Events carry `capture_source = "cdp_proxy"`.

Auth material uses a separate bounded, process-memory-only buffer: request auth
headers, cookie headers when CDP supplies them, and auth-bearing query values,
captured before trace redaction. Magician drains it once via
`GET /auth/drain/{thread_id}` straight into its encrypted captured-auth store.
Durable trace headers, auth query values, sensitive JSON body fields and
auth-bearing initiator URLs stay redacted, and the auth payload never enters
trace artifacts or LLM inputs. No LLM is involved.

`GET /ambient/page/drain/{thread_id}` drains `AmbientPageSignal`s (redacted
URL/title/origin, structural/content hashes, element counts, change flags; no DOM
text); the bridge accepts `BridgeEnvelope::AmbientPageSignal` but no extension
file emits it. Per-thread buffers are derived runtime caches; Magician owns
draining and persisting at execution boundaries.

## Extension behavior

- **Debug-page session binding.** Debug/SOTA launches register the execution as
  pending (`extension_messages.js`); `background.js` moves it to the real
  automation tab/window once the session exists, so the launcher page never
  shows the running-session overlay.
- **Terminal status.** The background worker, side panel and content handlers
  share one execution-status normalizer: `completed`, `success`, `failed`,
  `cancelled`/`canceled`, `timeout`, `aborted` all clear tracking. The stale
  cleanup alarm sends a terminal overlay status or hide before clearing state.
  Pause-state polling uses a bounded abort signal.
- **Notes capture menu.** `notes_capture_menu.js` adds a selection-only "Save
  selection to Magician Notes" item beside Contextual Assist (assist opens an
  overlay; this one just files). It posts straight to Magician's
  `/notes/capture-selection`, not via the host gateway, so it works with the
  desktop app closed. It registers at init, `onInstalled` and `onStartup` (MV3
  workers are ephemeral). The client is bearer-only
  (`magician_scope.js` deletes `X-Principal` / `X-Workspace`); the payload is
  `text`, `source_url`, `source_title`, `source_app` and a fresh `capture_id`
  (a resend after a lost response files once; a second click files again).

## Startup diagnostics

Magicutor initializes tracing before its startup message and records its PID at
INFO, with no unlabelled stderr banner, so healthy startup never shows as
supervisor warnings.
