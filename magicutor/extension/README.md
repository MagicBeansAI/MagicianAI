# Magicutor Chrome Extension

**Current extension version:** `0.2.2` (Chrome shows it as **Magican**).

This Chrome extension lets Magicutor's CDP proxy automate the user's normal Chrome profile through the WebSocket bridge.

## Features

- **Window Management**: Create and close CDP-owned Chrome windows
- **Tab Inventory**: Close and list tabs for CDP target ownership
- **Debugger API**: Execute CDP commands for advanced automation
- **WebSocket Bridge**: Bidirectional communication with the local Magicutor CDP proxy
- **Runtime Endpoint Discovery**: extension `0.1.266` resolves configured
  Magician and Magicutor ports through the stable desktop host gateway, caches
  the last valid loopback-only contract, and reconnects when it changes.
- **Single-Session Bridge Lifecycle**: extension `0.1.265` keeps connection
  establishment single-flight, deduplicates reconnect timers, applies bounded
  backoff/jitter, and ignores stale socket callbacks after a replacement bridge
  generation becomes active.
- **Event Streaming**: Real-time browser events (tab/window creation, navigation, etc.)
- **Side Panel Controls**: Persistent control surface for running, monitoring, and cancelling automation through Magician's active execution cancellation APIs
- **Popup Fallback**: Full popup UI remains available if side panel action wiring is unavailable
- **Live Previews**: Side-panel global live monitor for active automation sessions. Legacy dashboard bridge plumbing remains in the extension, but the current Magician ExecutionPanel no longer mounts a dashboard live-preview tab.
- **Agent-Browser CDP Runtime**: agent-browser sends raw CDP traffic through Magicutor; the extension no longer hosts the old high-level click/type/observe/scroll action runtime
- **CDP Action Summaries**: popup and side-panel activity rows show the exact CDP method and compact payload tooltip for raw debugger commands
- **API Mining Hooks**: debugger attach enables Network/Runtime/DOM/Page domains, OOPIF auto-attach, trace capture plumbing, and page-context `api_replay` fetches for browser cookie/TLS parity
- **Automation Page Signals**: Magicutor mirrors foreground automation CDP command completions and selected lifecycle events into the thread-owned page-signal drain without issuing extra extension-side debugger commands
- **Contextual Assist Probe**: Magicutor can ask the extension, on demand, whether
  the active tab has selected text or a focused writable DOM field. The content
  script returns metadata-only eligibility and never streams selected text into a
  realtime buffer.
- **Contextual Assist Context Menu**: Chrome shows one `Magician` context-menu
  item for selected text and editable fields. Clicking it calls the local desktop
  host gateway to open the native Contextual Assist menu.
- **Dialog Handling**: JavaScript alert/confirm/prompt events are forwarded through the CDP proxy; agent-browser owns `dialog status`, `dialog accept`, and `dialog dismiss`
- **Browser-Owned Downloads**: the extension no longer intercepts or cancels downloads; download artifacts should come from agent-browser/tool outputs or a future passive ambient-capture feature
- **Terminal Status Cleanup**: background, side-panel, and message handlers share one terminal-status normalizer so completed/failed/cancelled executions hide stale automation overlays and clear paused state consistently
- **CDP Overlay Mirroring**: raw agent-browser CDP commands are still forwarded as-is, but mouse/key/navigation/screenshot commands are mirrored into the visual overlay so CDP-proxy automation shows the cursor sprite, status panel, and Stop control. The cursor is a filled black pointer with a wide base, a notch in that base, rounded corners, a white edge, and a light blue-violet tint behind it, and it floats while it waits instead of sitting in the corner. The page-sized spotlight is not drawn.
- **Overlay Click Isolation**: the floating status panel accepts pointer input only while its visible class is active; its hidden state is click-through to the underlying page.
- **Gmail Render Budget Boundary**: `gmail_mail_assist_latency.js` is loaded
  before content scripts and gives the future Gmail annotation renderer a
  synchronous one-frame (16.7 ms) measurement boundary with bounded samples,
  p50/p95, violation counts, warnings, and a DOM telemetry event. Gmail chips
  themselves remain part of Mail Assist Phase 4 and are not enabled yet. This
  Phase 8 boundary ships in extension `0.1.264`.

## UI Surfaces

The extension now exposes three related surfaces:

- **Side panel**: Primary control surface for automation commands and the global `Live` monitor tab
- **Popup**: Full fallback control surface when side-panel action-click behavior is unavailable
- **Dashboard bridge**: legacy page-to-extension preview plumbing retained for future reuse; not an active mounted UI surface in the current Magician ExecutionPanel

Live previews use CDP screenshot polling for automation tabs that are already
debugger-backed by the agent-browser CDP proxy. The `tabCapture`/offscreen path
remains available for non-debugger preview targets.

## Installation

### 1. Install Extension in Chrome

1. Open Chrome and navigate to `chrome://extensions/`
2. Enable "Developer mode" (toggle in top-right corner)
3. Click "Load unpacked"
4. Select the `extension` directory: `/path/to/magicutor/extension`
5. Note the Extension ID (e.g., `abcdefghijklmnopqrstuvwxyz123456`)

### 2. Start Magician And Magicutor

The extension discovers the configured Magician and Magicutor HTTP/WebSocket
ports from the desktop host gateway at
`127.0.0.1:3017/host/runtime/endpoints`. It accepts loopback URLs only, caches
the last valid contract across service-worker restarts, and falls back to
Magician `:3002` / Magicutor `:3003` when the gateway and cache are unavailable.
Magician and Magicutor are expected to already be running before using the
extension controls.

### 3. Verify Installation

1. Click the Magicutor extension icon in Chrome toolbar
2. On supported Chrome versions, the side panel should open and show connection state, extension version, and automation controls
3. If side-panel action wiring is unavailable, the popup should open instead and show the same control surface
4. If disconnected, check:
   - The desktop host gateway is running on `127.0.0.1:3017`
   - Magician and Magicutor are running on the ports selected in Desktop Settings
   - The extension is loaded in the Chrome profile you want to automate

## Usage

Once installed in a Chrome profile, the extension automatically:

- Connects to the Magicutor WebSocket bridge on startup
- Listens for commands from Magicutor
- Streams browser events back to Magicutor
- Manages CDP-owned windows and tab inventory as requested
- Keeps tab inventory lightweight: default `list_tabs` does not call
  `chrome.debugger.getTargets()`, so CDP `Target.getTargets` discovery does not
  depend on a debugger API call that may hang in Chrome.
- Exposes live previews in the extension side panel and in supported dashboard pages
- Attaches the workspace-bound bearer saved in the popup or side panel to extension → Magician API calls. Legacy scope headers are stripped before dispatch.
- Clears execution overlays from automation tabs when polling or explicit status messages observe a terminal execution state, including stale completed runs restored from extension storage.
- Answers Magicutor `probe_contextual_assist` bridge requests by inspecting the
  active tab/frame only at request time for Contextual Assist eligibility.
- Registers one `Magician` context-menu item for Chrome `selection` and
  `editable` contexts and opens the native Contextual Assist menu through the
  desktop host gateway when clicked. The click payload includes selected text
  when Chrome provides it, or non-empty focused editable-field text from the
  content script on normal pages. Empty editable fields send no context text;
  password/token/API-key/credential-like fields and explicitly marked sensitive
  DOM regions are treated as secure and are not forwarded.
- Answers `capture_contextual_assist_tab` bridge requests with a visible-tab PNG
  when Chrome allows `captureVisibleTab`; Chrome capture failures return
  `captured: false` plus the browser error so Desktop can fall back to native
  focused-window screenshot capture.

### Ambient browsing capture (work-evidence graph, Phase 2)

Independent of the CDP/bridge automation above, the extension also does **opt-in
ambient capture** of normal browsing for the work-evidence graph:

- `ambient_browsing.js` (content script, top frame only) computes a
  **metadata-first** signal from the live DOM on each page load / SPA route
  change — sanitized URL (secret query params redacted), title, top headings, and
  element-count structure (incl. a password-field flag). No full HTML, no
  screenshots, no CDP/debugger.
- `ambient_collector.js` (background) queues signals in `chrome.storage.local`,
  drops incognito-tab signals, and batch-uploads (~30s alarm) to Magician's
  `POST /api/magician/v2/ambient/signals/batch` via `magicianFetch` (scoped).
- Capture only **persists when the user enables it** on the `/observe` page's
  "Observe tabs" card (a server-side consent flag); the server also applies a
  denylist + secret-field redaction before storing. Private/incognito windows are
  always excluded.

## Architecture

### Gmail Mail Assist latency contract

Every Gmail chip/panel DOM commit must run through
`MagicianGmailLatency.measureRender(render, options)`. The default surface is
`gmail_chip`; callers may provide a narrower budget or surface name. A miss
emits `magician:gmail-chip-latency`, records a bounded sample, and warns without
blocking Gmail or throwing. Inspect aggregate in-session results with
`MagicianGmailLatency.stats()`.

The helper is content-script safe and directly testable in Node:

```bash
node --test magicutor/extension/gmail_mail_assist_latency.test.cjs
```

```
Magician execution API
       ↓
Magicutor CDP proxy + WebSocket bridge
       ↓
Chrome Extension (JavaScript)
       ↓
Chrome Browser (windows/tabs/debugger)
```

### `Runtime.evaluate` IIFE handling

`sendDebuggerCommand` in [`debugger_actions.js`](./debugger_actions.js) auto-wraps top-level `const`/`let`/`class`/`await` expressions in an async IIFE so callers can pass either expression-shaped or statement-shaped JS. Existing IIFE wrappers (`(()=>…)()`, `(() =>…)()`, `(async()=>…)()`, `(function…)()`) are detected whitespace-tolerantly and left alone — they're patched sync→async only when the body uses `await`. Without this, compact arrow IIFE forms like `(()=>{const x=5;return x})()` would be re-wrapped without a `return` and silently resolve to `undefined`.

## Development

### Testing the Extension

```bash
# Open Chrome DevTools for the background service worker
chrome://extensions/ → Magicutor → "service worker" link

# View console logs
Console tab shows all extension logs

# Test bridge connectivity from the popup/side panel status area
```

## Permissions

The extension requires these permissions:

- `tabs` - CDP target inventory and owned-tab cleanup
- `windows` - CDP-owned automation window lifecycle
- `debugger` - CDP access for advanced automation (click, type, screenshot, etc.)
- `scripting` - Execute page-context API replay fetches for API mining
- `webNavigation` - Navigation events
- `storage` - Local state persistence (including profile name tracking)
- `<all_urls>` - Content script injection for site-specific extraction

## Security

- Extension only talks to local Magician/Magicutor HTTP/WebSocket endpoints
- Debugger API requires user consent (one-time per Chrome profile)
- All browser actions are logged and auditable
- Secret values (`[REDACTED:...]` placeholders) are substituted server-side; never sent to the extension
