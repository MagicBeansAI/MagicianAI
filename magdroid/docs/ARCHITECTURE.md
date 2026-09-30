[← Back to README](../README.md)

---

# 🏗️ Architecture Overview

> This document began as the vendored NeuralBridge transport description. The
> current Magdroid application also contains a native Magician client. The
> client and automation bridge share identity but not responsibilities.

## Native client path

The Compose shell lives in the `:app` module. Network contracts and state live
in `:bridge`, alongside the authenticated connection layer, so UI code cannot
silently invent a different principal, workspace, host, or credential path.

Attention HITL is the same envelope as web and Magios: one
`POST /hitl/{correlation_id}/respond`. `input_type=form` is stacked fields on
the Attention sheet; chat form cards open that sheet instead of flattening to
a text box. Grants (`tool_authorization`, `sandbox_override`), chained
clarifications, destructive confirmations, file-path multiplicity, and
external-action instructions render on the same sheet. Chat composer modes
are `ask` (omitted), `accept_in_scope`, and `plan`.

For Tasks, `TaskRepository` is the only HTTP/WebSocket boundary. It exposes the
existing Magician V3 task and monitor APIs, V2 execution controls and Notes
publication, always using `MagicianAccess`. `TasksViewModel` owns accumulated
pagination, filters, mutation serialization, completion grace, realtime
refresh, partial detail reads, and the task/monitor navigation state. Compose
renders that state through `TasksScreen`, `TaskDetailScreen`, and
`MonitorScreens`; events are refresh hints, never the source of truth.

```text
Compose Tasks / Task Detail / Monitors
                 │
          TasksViewModel
                 │
          TasksDataSource
                 │
          TaskRepository
        ┌────────┴────────┐
 Magician V3 tasks     V2 controls / Notes
 and monitors          + scoped realtime WS
```

Task detail deliberately reads task, execution-panel, outputs, details, and plan
in parallel and retains successful sections. A transient failure in Outputs must
not turn a readable task and run timeline into a full-screen error. Mutations,
in contrast, fail closed and are single-flight because two overlapping status or
plan changes cannot be reconciled safely by the phone.

List availability is also lane-scoped. Tasks, Internal, and Monitors retain
independent load failures, so an Internal refresh cannot display an error over
the Tasks lane. A failed refresh keeps the last authoritative rows and renders
an inline retry banner; an empty failed lane renders the same structured error
as its placeholder. Transport failures are classified into actionable offline,
configuration, authentication, timeout, conflict, capacity, service, and wire
response messages. Unknown exception details remain in diagnostics rather than
being projected into user-visible copy.

Today follows the same ownership rule but has a different consistency shape.
`TodayRepository` owns the scoped Today projection plus visibility, channel
follow-ups, resurfacing, feed, published-surface, analytics, canonical-delivery,
impression, and realtime contracts. `TodayViewModel` is the sole client state
machine and Compose renders it through `TodayScreen`.

```text
Compose Today
     │
TodayViewModel ─── visible-only 30s / 20s / 60s polling
     │
TodayDataSource
     │
TodayRepository
 ┌───┼────────────┬──────────────┬─────────────┐
Today projection  Channel assist  Feed/briefings Analytics pulse
 + visibility     + attention     + render      + V3 task counts
```

Briefing detail prefers `render.muij_document` and passes it through the pure
`MuijDocument` validator before Compose sees a component. Validation uses an
iterative walk, unique ids, the server's 32-level depth limit, a 500-component
top-level limit, and a mobile total-work limit. `MuijRenderer` then projects the
tree as read-only native UI; it does not evaluate queries, execute ActionBus, or
treat the document as authority. Static props and `static_snapshot` supply table,
metric, and chart values. An unknown display type renders a labeled fallback
with its valid children, while malformed documents disclose an inline error.
`json_content` is a distinct bounded key/value fallback and is never guessed to
be MUIJ without the versioned envelope.

The core `/today` response determines whether the page is available. Hidden
items, resurfacing, channel follow-ups, activity, briefings, and pulse are
fail-soft sections; each retains its last good value and exposes its own error.
Scope-matched realtime frames become Morning Edition wire lines; Today-relevant
ones are also debounced refresh hints. Mutations remove a card immediately,
persist through the authoritative endpoint, and restore it at its prior list
position if persistence fails. Canonical attention ordering is adopted only
when the complete response validates against the advertised projection and all
delivered ids exactly match the loaded page; malformed, stale, cross-scope, or
partial authority never reorders the screen.

`TodaySwipeActionCard` is the single gesture implementation for core Today,
message follow-up, and Worth-a-look cards. It is keyed by the durable card id,
keeps a short-swipe rail open for labeled button activation, and commits only
the first action on an edge after the same distance/velocity threshold used by
Magios. Gesture state is presentation-only; every action enters the ViewModel's
per-card mutation lock before an authoritative write. Core actions are split
between two inline controls and a lossless overflow list rather than truncated.
The Morning Brief deck (`TodayTriageDeck`) is the one other Today gesture: a
card-owned `awaitEachGesture` drag that claims only sideways drags and leaves
vertical ones to the page scroll and resolves releases through the pure `resolveDeckRelease`; it commits
through `TodayViewModel.triageDeckCard`, which enters the same mutation locks.

## Settings and native voice

Settings owns presentation and delegates every behavior to one durable owner.
`ThemeStore` owns appearance, `MagicanKeyboardStore` owns keyboard skills and
learned vocabulary, `MagicianAccess` owns endpoint/scope/credentials,
`WakeService` owns the background microphone window, and `VoicePrefs` is the
single process-wide source for device-local audio choices. Rows that Android
must authorize—Accessibility, assistant role, microphone, overlay,
notifications, battery exemption, and keyboard enablement—open the system's
authoritative surface and re-read the grant on resume; the app never remembers a
fake grant.

`PrimaryAgentWakeIdentityStore` is the arm-time addressing bridge. On Settings
entry it reads the scoped primary definition from `GET /api/magician/v2/agents`,
caches the last good canonical name and aliases, and derives the same
canonical-name-first `Hey <name>` phrases as Magios and backend voice
addressing. There is deliberately no hardcoded assistant fallback: an empty
cache cannot arm the microphone. A changed identity flows into an active
`WakeService`, which rebuilds the local Vosk grammar and its disclosure
notification without requiring a stop/start cycle. The service sends no
ambient audio to the backend until one of those exact local phrases matches.

App Pilot is the single owner-facing automation surface. The drawer starts the
standalone `MainActivity` directly; Settings contains no second accessibility,
permission, endpoint, credential, or activity projection. `MainActivity` keeps
its stable intent identity but now hosts Compose and resolves the same persisted
theme family and system/day/night mode as the rest of Magican.

The canonical surface preserves the complete legacy Status / Setup / Activity
contract without preserving three separate destinations. Status owns
enablement, live bridge/accessibility/screenshot state, device identity,
command percentiles, all Android grants, and secure pairing and Access
credentials. Permissions, connection, and device details are state-aware accordions:
missing prerequisites sort first and open automatically, while collapsing a
section cannot discard unsaved form values. Activity remains the filtered and
pausable projection of two bounded
in-memory sources: `BridgeLog` for outbound socket lifecycle and `CommandLog`
for tool executions over the production socket. Secret fields are write-only:
blank values preserve stored secrets
and stored values are never rendered back into Compose.

Device enrollment is an owner-initiated bootstrap, not a second identity
system. Web Settings creates a bounded pending record under the browser's
current principal/workspace and renders a local QR containing only the public
origin, random enrollment id, and one-time secret. The backend keeps only the
secret digest in memory, expires it after five minutes, and atomically consumes
it while minting the durable device credential. A wrong guess does not destroy
the legitimate ticket; a successful exchange, cancellation, or expiry does.

Android accepts the QR scanner result or the same `magican://connect` app link.
The older `magican://pair` shape remains parser-only compatibility for already
issued Android links. Camera
access is requested just in time for QR capture and is deliberately excluded
from operational permission readiness. Scanner cancellation and camera denial
return to the themed App Pilot surface with explicit recovery, while the pending
enrollment URI is retained in memory through Activity recreation. Android
rejects extra or duplicated fields and unsafe origins, and requires confirmation
of the normalized host. `DeviceEnrollmentClient` reaches the exact one-time
exchange without any pre-existing credential. The response supplies the
deployment's current outer Cloudflare credential plus Android's mobile-client
and device-automation credential. The client proves both on `/devices/me`
before `MagicianAccess` atomically persists the returned endpoint, scope, and
tokens in encrypted preferences backed by Android Keystore. Every later HTTP
and WebSocket request carries that device proof. Magician derives scope from
the roster and rejects a shared service-token identity without a paired device.

```text
Owner in web Settings                    Android App Pilot
        │ create five-minute enrollment         │
        │◄─── local SVG QR / magican://connect ────┤ scan + show host
        │                                        │ owner confirms
        └──────── one-time exchange ────────────►│
                   durable scoped token          │ encrypted save
                                                 └─ reconnect MCP bridge
```

```text
Settings / Voice sheet / Active call / Audio Notes
                 │
      VoicePrefs + ChatViewModel
        ┌────────┼───────────────┐
  Dictation   RealtimeVoice   AudioNoteOutbox
  + Speech       Session            │
        │           │          WorkManager retry
        └──── Magician media/notes APIs ────┘
```

Live voice registers one call-scoped media session and opens a backend-proxied
control WebSocket. Binary frames are bounded PCM16 chunks; control frames carry
captions, readiness, interruption, PTT, and terminal state. Translation profiles
force open mic. Other profiles honor the persisted open-mic/hold-to-talk choice,
with a local audio gate preventing bytes from leaving after mute or release.
Bootstrap and mid-call transport losses use separate bounded recovery budgets,
and reconnect reuses the same registration. A stop cancels capture immediately,
then performs time-bounded registry cleanup.

Audio Notes use a write-before-send outbox. WAV and metadata exist durably before
the first request, so an app death or offline phone cannot turn a consented
recording into silent loss. Server paging and search remain authoritative;
recording bytes are fetched only for playback. Dictation archiving is off by
default and never changes ordinary ephemeral dictation into storage without the
owner's explicit switch.

## Automation path

Magdroid opens one authenticated WebSocket to Magician and serves a bounded MCP
`2026-07-28` tools surface on it. The phone remains the MCP server even though
it initiated the connection. It opens no listening socket, which keeps the
automation surface usable behind NAT and removes same-network access entirely.

Magician adapts the accepted socket to `magician-mcp-client` through a bounded
duplex-JSON transport. rmcp owns discovery, correlation, tool identifiers,
result validation, cancellation, and the request-bound tool-roster subscription.
The device hub owns pairing, scope, one generated connection revision, timeout,
and reconnect. On the handset, `MagdroidMcpServer` routes a validated
`tools/call` into `McpToolHandler`, which executes through the live
`AccessibilityService` engines without IPC or ADB transport overhead.

<p align="center">
  <img src="diagrams/architecture.svg" alt="NeuralBridge Architecture" width="800" />
</p>

---

# ⚡ Data Flow

### What happens when you say "tap Login"

The journey from natural language to a physical tap on glass takes four steps — and completes in roughly 60ms:

1. **Agent chooses a public Android verb** — Magician resolves its internal
   action to a tool identifier from the handset's latest MCP discovery.
2. **Companion App resolves selector** — The Semantic Engine walks the accessibility tree, finds the "Login" button, resolves it to coordinates `(540, 820)`, and dispatches the gesture through the AccessibilityService.
3. **Android OS processes the tap** — The system injects the touch event into the target app's window.
4. **Agent receives response** — The companion returns an MCP tool result over
   the outbound socket; Magician preserves device failure separately from
   timeout, disconnect, and protocol failure.

Total: **~60ms end-to-end** vs ~1500ms with Appium.

<p align="center">
  <img src="diagrams/data-flow.svg" alt="Data Flow — tap Login" width="800" />
</p>

---

# 🔀 The Two Command Paths

Not all operations are equal. NeuralBridge intelligently routes commands through the fastest available path.

### Fast Path — AccessibilityService (<10ms)

The fast path handles **95% of operations** by executing directly within the companion app's process. No IPC, no ADB, no process spawning.

| Operations | Latency |
|---|---|
| `tap`, `swipe`, `pinch`, `drag` | ~2ms |
| `input_text` | ~1.4ms |
| `press_key`, `global_action` | <5ms |
| `get_ui_tree` | 18–33ms |
| `find_elements` | <10ms |
| `screenshot` | ~60ms |
| `notifications`, `events` | <10ms |

### Slow Path — ADB Shell (200ms+)

The remaining **5% of operations** require ADB shell access for capabilities that Android restricts from accessibility services.

| Operations | Latency | Why ADB? |
|---|---|---|
| `close_app` (force-stop) | ~200ms | `am force-stop` requires shell |
| `list_apps` | ~200ms | Package manager query via shell |
| `set_clipboard` | <10ms | Background clipboard access restricted on Android 10+ |

<p align="center">
  <img src="diagrams/command-paths.svg" alt="Fast Path vs Slow Path" width="800" />
</p>

---

# 🎯 Selector System

Most tools accept **selectors** instead of raw coordinates. This means your AI agent can say `tap(selector: "Login")` instead of `tap(x: 540, y: 820)` — making automation scripts readable, resilient to layout changes, and resolution-independent.

### The Resolution Priority Chain

When a selector arrives, the Semantic Engine walks the accessibility tree and resolves it through a six-step priority chain:

| Priority | Strategy | Example |
|---|---|---|
| 1 | **Exact text match** | `"Login"` == `"Login"` |
| 2 | **Partial text match** | `"Login"` found in `"Login Now"` |
| 3 | **Content description** | `contentDesc` == `"Login"` |
| 4 | **Resource ID (suffix)** | ID ends with `"login_button"` |
| 5 | **Combined (AND logic)** | `text="Login"` AND `class="Button"` |
| 6 | **Fuzzy match** | Levenshtein distance: `"Login"` vs `"Logn"` < 3 |

### Multiple Matches?

When more than one element matches, the engine applies a tiebreaker preference:

**Visible** → **Interactive** → **Center-positioned**

This ensures taps land on the most likely intended target — a visible, tappable button near the center of the screen beats a hidden or non-interactive element every time.

<p align="center">
  <img src="diagrams/selector-system.svg" alt="Selector Resolution Chain" width="800" />
</p>

---

# ✅ What Works and What Doesn't

### Works Great (95% of use cases)

- **Native Android apps** — Settings, Calculator, Clock, Contacts, Files
- **Popular apps** — Chrome, YouTube, Gmail, Maps, social media, e-commerce
- **System UI** — Notifications, Quick Settings, Recent Apps, Launcher
- **Multi-step workflows** — Form filling, navigation, app switching
- **Accessibility testing** — Touch target audits, content description checks

### Limitations

| Limitation | Reason |
|---|---|
| Games (OpenGL/Unity/Unreal) | Canvas rendering — no accessibility tree |
| Banking apps with FLAG_SECURE | Screenshot blocked by the app |
| Biometric authentication | Cannot simulate fingerprint/face |
| CI/CD headless screenshots (Android 14+) | MediaProjection requires user consent |
| Google Play distribution | AccessibilityService policy restrictions |

---

<p align="center">
  <a href="../README.md">← Back to README</a>
</p>
