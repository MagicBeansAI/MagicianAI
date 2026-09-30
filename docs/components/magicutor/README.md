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
click/type/observe, selector retry and similar robustness, native file dialogs
(use CDP/file-input uploads; host automation belongs to the macOS host bridge),
`CaptureAuth` / `ForceAuthRefresh` actions (auth capture is passive CDP
observation), and download interception (the extension never cancels, rewrites
or registers page downloads; see
Retired Download Architecture).
The legacy `GeneratedActionSandbox` and enum-based side-effect classifier must
not be extended.

**Endpoint discovery.** On service-worker startup and periodic bridge health
checks the extension reads the versioned contract from the desktop host gateway,
`http://127.0.0.1:3017/host/runtime/endpoints` (loopback HTTP/WebSocket URLs
only). The last valid contract is cached across desktop startup gaps, compiled
defaults (`3002`/`3003`) are the final fallback, and a port change retires and
reconnects the bridge.

## Server routes

- `GET /health` — process liveness.
- `GET /json/version`, `GET /json`, `GET /json/list` — CDP discovery.
- `GET /devtools/browser/{id}`, `GET /devtools/page/{tab_id}` — CDP-compatible
  WebSocket attachment.
- `POST /cdp/threads/{thread_id}/bind-tab`, `DELETE /cdp/threads/{thread_id}` —
  thread-scoped tab ownership.
- `GET /trace/drain/{thread_id}` — API-mining traces.
- `GET /auth/drain/{thread_id}` — one-shot transient auth material into
  Magician's encrypted secret store.
- `GET /ambient/page/drain/{thread_id}` — page-signal buffer (currently unfed).

## CDP proxy owned tabs

Thread-scoped CDP sessions isolate tabs in `THREAD_OWNED_TABS`
(`magicutor/src/server/cdp_proxy.rs`):

- **Tier 1:** tabs in the thread's dedicated window (`THREAD_WINDOWS`), or one
  extension-bound existing tab (`THREAD_BOUND_TABS` via `bind-tab`).
- **Tier 2:** later tabs whose `openerTabId` chain leads to an owned tab, claimed
  on `Target.attachedToTarget`.
- **Tier 3:** the user's other tabs, other executions, and `noopener` popups —
  invisible to this thread.

`check_tab_ownership` gates `Target.attachToTarget` and `Target.closeTarget`. An
empty owned set is allowed only before `Target.getTargets` populates it; after
that, unowned attach/close is rejected. `DELETE /cdp/threads/{thread_id}` clears
the thread's state, closing dedicated windows but not bound existing tabs. With
no extension WebSocket connected, sessions return a bridge error.

Before reusing a cached dedicated window, Magicutor checks the extension's live
tab inventory: a window with no tabs is forgotten with its mappings and
recreated; an unanswered check keeps it (recreating would not repair a
disconnected bridge).

**Iframes.** The proxy records OOPIF session ids from Chrome auto-attach
`Target.attachedToTarget` events with `targetInfo.type == "iframe"`, and forwards
only those Chrome-issued ids on `chrome.debugger.sendCommand` (proxy-minted
main-tab ids would be rejected). `debugger.js` forwards `Target.*` and other
non-flood events to the bridge; it does not refresh iframe metadata on
`Target.targetInfoChanged`.

## CDP proxy debugger release

`Target.attachToTarget` is proxied as `attach_debugger` (`chrome.debugger.attach`,
which shows Chrome's "is being debugged" bar). Release is proxied too:

- When the CDP client's websocket closes (`CdpSession::stopped` — run cleanup
  closed the agent-browser session, or the CLI died), the proxy first sends
  `session_ended {sessionId}`, then `detach_debugger` for every top-level tab the
  session attached (`tabs_to_detach`; iframe children ride their parent). The
  ending goes first because `detach_debugger` clears the session's tab registry.
- An explicit `Target.detachFromTarget` releases a top-level tab no other session
  still references.
- On `session_ended`, the extension's `endAutomationSession` asks Magician for the
  execution status (2 s budget): terminal → the real ending (`overlay_status`
  success/error and the aurora animation) on every tab the execution may show on
  (session tabs, tracked tab, its dedicated window); unreachable →
  `overlay_panel_hide` + `overlay_hide`; paused or waiting-children → the panel
  stays. Then the execution is untracked and the session cleared.

Logs (info): `[cdp-proxy] session stopped thread=… detaching debugger from N
tab(s) [ids]`, `[cdp-proxy] detached debugger tab=N`, and `[cdp-proxy] automation
UI ended thread=… result={ended, executionId, status, tabs}`; a
`detach_debugger tab=N failed: …` warning names the extension's reason. To audit a
run's teardown, expect these right after Magician's `Closed agent-browser session
at execution end`. The extension's `list_tabs` `debuggerAttached` flag is cleared
even when `chrome.debugger.detach` fails, so it is not evidence on its own.

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
  pending (`extension_messages.js`); `background.js` upgrades it to the real
  automation tab/window once the browser session exists, so the launcher page
  does not inherit the running-session overlay.
- **Terminal status.** The background worker, side panel and content handlers
  share one execution-status normalizer: `completed`, `success`, `failed`,
  `cancelled`/`canceled`, `timeout`, `aborted` all clear tracking. The stale
  cleanup alarm sends a terminal overlay status or hide before clearing state.
  Pause-state polling uses a bounded abort signal.
- **Notes capture menu.** `notes_capture_menu.js` adds a selection-only "Save
  selection to Magician Notes" item beside Contextual Assist (two entries: assist
  opens an overlay; this files and gets out of the way). It posts straight to
  Magician's `/notes/capture-selection`, not via the host gateway, so capture
  works with the desktop app closed. It registers at init, `onInstalled` and
  `onStartup` (MV3 workers are ephemeral). The client is bearer-only
  (`magician_scope.js` deletes `X-Principal` / `X-Workspace`); the payload is
  `text`, `source_url`, `source_title`, `source_app` and a fresh `capture_id`
  (a resend after a lost response files once; a second click files again).

## Startup diagnostics

Magicutor initializes tracing before its startup message and records its PID at
INFO, with no unlabelled stderr banner, so healthy startup never shows as
supervisor warnings.
