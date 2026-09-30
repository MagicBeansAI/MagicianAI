# Unified UI Docs

Landing page for `ui/unified-ui` documentation.

**Current development version:** `0.1.31`.

SvelteKit 2 / Svelte 5 frontend for Magician. Product surfaces present as
**Magican** (`PRODUCT_NAME` from `src/lib/presentationIdentity.ts`, generated
from `data/presentation_identity.json`). Contact is
`reach.magican@gmail.com`. `package.json` pins `packageManager` to
`npm@10.9.3` — use npm in this package, not pnpm.

`/privacy`, `/manifesto`, and `/terms` prerender as direct-entry pages for
`next.magican.ai`. Version history lives in
[CHANGELOG.md](../../../ui/unified-ui/CHANGELOG.md), not here. CSS class names
still use a `presto-*` prefix (`presto-gaui-page`); there is no `/presto/` route
tree.

## Canonical References

- [Package README](../../../ui/unified-ui/README.md)
- [Changelog](../../../ui/unified-ui/CHANGELOG.md)
- [Testing](testing.md)
- [Theme tokens](theme-tokens.md)
- [MUIJ / GAUI / native boundaries](muij-native-ui-boundaries.md)
- [Auth session gate](auth-session-gate.md)
- [Unified task panel](unified-task-panel.md)
- [Task and execution UI architecture](threads-tasks-conversion.md)
- [HITL attention](../magician/hitl-attention.md)
- [Chat mode](../magician/chat-mode.md)
- [Quick start](../../quickstart.md)

## Feature docs

### Shell, landing, overlays

- [Root landing](root-landing-native-route.md) — `/` Magican manifesto landing
- [HUD command bar](hud-command-bar.md) — Tauri `/hud` two-state summon
- [Notification overlay](notification-overlay.md) — `/notify-overlay` + `$lib/notify`
- [Personal tutor overlay](personal-tutor-overlay.md) — tutor / App Copilot draw
- [Warroom ops deck](warroom-ops-deck.md) — `/warroom` demo HUD
- [Composer dock](composer-dock.md) — chat composer dock on `/chat`, threads, HUD
- [Theme tokens](theme-tokens.md) — CSS custom properties, 22 themes
- [Auth session gate](auth-session-gate.md) — `(app)` bearer gate and workspace switch

### Tasks, execution, history

- [Unified task panel](unified-task-panel.md) — one panel for tasks and runs
- [Task and execution architecture](threads-tasks-conversion.md) — task vs execution APIs
- [Tasks and history native surfaces](tasks-history-native-surfaces.md) — `/tasks`, `/t/[name]/tasks`
- [Execution native panels](execution-native-panels.md) — native inspectors (not GAUI)
- [History native route](history-native-route.md) — `/history`
- [Monitors](monitors.md) — `/tasks?type=monitors`
- [Recurring App execution history](recurring-app-tasks.md) — one internal task per scheduled App behavior
- [Paged-list removal](paged-list-removal.md) — optimistic delete of a paged row
- [Structured response rollout](structured-response-rollout-controls.md)

### Attention, Today, square

- [Today attention lanes](today-attention-lanes.md) — `/today` Morning Edition: realtime wire, operations carousel, swipe deck / broadsheet, and the backend-backed lanes behind them
- [Attention pagination](attention-pagination.md) — `/attention` inbox paging
- [Town Square crew world](fleet-civilization-world.md) — `/square` Floor/Campus game
- [Agent update feed](agent-updates-feed.md) — `/feed` operator timeline

### Chat, voice, HUD, tutor

- [Chat turn progress](chat-turn-progress.md) — live turn card
- [Concurrent voice requests](concurrent-voice.md) — independent background questions, coordinated playback, context selection, cancellation
- [Wake word](wake-word-voice.md) — ambient Orb admission (not a composer button)
- [Voice call boundary](voice-call-boundary.md) — display-only call boundary
- [Voice resume framing](voice-resume-framing.md) — resume context is reference-only

### Operator surfaces

- [API mining](api-mining-native-route.md) — `/api-mining`
- [Claims Review](claims-review.md) — `/claims-review`, Envoy capture, native/app parity
- [Crew native forms](crew-native-forms.md) — `/crew/new`
- [Memory native route](memory-native-route.md) — `/memory`
- [Taste proposals](taste-proposals-panel.md) — `/memory` taste review
- [Notes search](notes-search.md) — `/notes`
- [Town Square posture banner](town-square-posture-banner.md) — `/town-square` status line
- [Observe channel toggles](observe-channel-toggles.md) — `/observe` Mail & chat card
- [LLM spend breakdown](llm-spend-breakdown.md) — `/llm` Spend
- [Model routing panel](model-routing-panel.md) — dedicated Settings operation/profile/model routing page and engine-affinity precedence
- [On-device generation](settings-local-generation.md) — Settings kitty pin (`runtime.ollama.local_generation.selected`) with RAM-tier warnings and an Ollama reload
- [Plane terminal grants](plane-terminal-grants.md) — Settings `plt_` grants
- [Resource Authority key](resource-authority-key.md) — `/budget` admin key
- [Runtime activity](runtime-activity-view.md) — `/runtime`
- [Storage activation](storage-activation.md) — `/storage`
- [Evals](evals-page.md) — `/evals`
- [Thinking map](thinking-map.md) — `/thinking-maps`
- [Event-taxonomy mirror](event-taxonomy-mirror.md) — generated `event-taxonomy.ts`
- [Testing](testing.md) — Vitest unit + component projects

## Key Routes

| Route | Purpose |
|-------|---------|
| `/` | Public Magican landing (outside `(app)`) |
| `/privacy`, `/manifesto`, `/terms` | Prerendered policy pages |
| `/login` | Scope session sign-in |
| `/today` | Morning Edition: masthead, realtime wire, lead story, operations carousel, Reading Room (swipe deck / broadsheet), briefings, deliverables, digest |
| `/briefing` | Published briefing canvas |
| `/apps`, `/apps/{installation}` | Installed apps and generated surfaces |
| `/vibe` | VibeDev coding cockpit; `?tab=workbench` for CLI sessions |
| `/chat` | Rich chat (attachments, grouped task progress, session history) |
| `/t/[name]/chat`, `/t/[name]/tasks`, `/t/[name]/settings` | Thread workspace; bare `/t/[name]` redirects to chat |
| `/tasks` | Task list; `?type=internal` and `?type=monitors` are views of the same route |
| `/observe` | Listen / join-as-agent / watch screen |
| `/reviews`, `/evidence` | Impact reviews and work-evidence inbox |
| `/claims-review` | Statement register review |
| `/town-square` | Agent social feed (TopBar / ⌘K "Town Square") |
| `/square` | Crew world (2D Floor / 3D Campus) with Today below |
| `/crew`, `/crew/[id]` | Agent management; live MUIJ surface on the detail page |
| `/attention` | Canonical HITL inbox (approvals, clarifications, pauses, escalations) |
| `/vault` | Provisioned secret CRUD and setup-token management |
| `/channels` | Telegram/WhatsApp bot runtime, QR pairing, logs |
| `/api-mining` | Learned APIs + noisy-origin triage |
| `/skills`, `/skills/evolution` | Skill/pack catalog and evolution |
| `/history` | Sessions and threads |
| `/memory` | User memory + taste proposals |
| `/notes` | Audio notes and search |
| `/triggers` | Trigger inventory |
| `/budget` | Resource Authority dashboard (`/budget/tokens/[id]`, `/transactions`, `/audit`) |
| `/llm`, `/llm/queue` | LLM observability and dispatch queue |
| `/runtime`, `/runtime/resources` | In-flight spans; observe-only local resource governor |
| `/storage` | Storage sizes, retention, Track B activation |
| `/settings` | Trust policy, config reload, theme, devices, plane grants |
| `/evals` | Eval-lane dashboard |
| `/thinking-maps` | Live thinking-map boards |
| `/feed` | Operator agent-update timeline |
| `/events` | Unscoped live event stream |
| `/mirror` | Execution-scoped event log |
| `/debug` | Direct browser actions and SOTA fixture launcher |
| `/warroom` | Demo ops deck |
| `/hud` | Tauri collapsed command bar |
| `/dev` | Full-screen workbench (sessions also appear under `/vibe`) |

Compatibility redirects: `/home` and `/desk` → `/today`; `/approvals` →
`/attention`; `/meetings` → `/observe`; `/internal-tasks` →
`/tasks?type=internal`; `/about` → `/#about`. Empty `dashboard/` and `v2/`
directories are not routes.

Tauri overlay routes (siblings of `(app)`, no TopBar): `/hud`,
`/contextual-assist` (native-only placeholder in a normal browser),
`/draw-overlay`, `/notify-overlay`, `/screen-region-picker`.

## Architecture

The v5 TopBar shell is the only `(app)` shell. `(app)/+layout.svelte` mounts
`TopBar`, `CommandPalette` (⌘K), `HistoryDrawer`, `AttentionCenter`,
`AtmosphereLayer`, and the attention prompt modal. Primary nav: Today, VibeDev,
Chat, Tasks, Observe, then any tab an installed app declares. Briefing, Apps,
Reviews and Square stay in the command palette (Square under Jump to; the others
under Manage).

**Command palette.** The closed palette preloads nothing (agents, tasks,
sessions, approvals, notes settings); opening it refreshes those stores
independently, keeps usable cached data, and reports failures without unhandled
rejections — keeping palette-only requests off initial navigation. Filtering
uses `commandFilter` (`src/lib/shell/commandMatch.ts`), not bits-ui's
subsequence scorer (which ranks unrelated phrases): NFKC-normalised,
case-folded, whitespace-collapsed; a command matches when the whole query is a
prefix/substring of its text or every query word is an exact or prefix word of
it; anything else is hidden.

**Auth.** The root `+layout.ts` redirects to `/login` when no workspace
bearer is present (hydration-aware for Tauri) on every route that
`isPublicRoute` does not list — the marketing pages, `/login`, and the theme
specimen sheet are the only public ones. Mount-time `refreshScopeSession()`
and the scoped-fetch 401 re-gate catch a dead token. See
[auth-session-gate.md](auth-session-gate.md).

**Desktop session restoration.** Tauri webviews restore the selected server's
native credential-store snapshot; an old `sessionStorage` bearer cannot replace
it. Native updates carry the snapshot's origin and revision, so a delayed view
cannot overwrite a newer native login. Native API requests, uploads and
voice/event WebSockets target the selected engine directly even when Vite serves
the UI. Tests: `make test-desktop-auth`.

**Task panel.** Every surface that opens a task or run mounts
`UnifiedTaskPanel` inside `TaskPanelDrawer` (`src/lib/magician/tasks/`); see
[unified-task-panel.md](unified-task-panel.md). Remaining native inspectors live
next to the `(app)` routes (`ExecutionPlanInspector`, `EventLogView`,
`PlanGraphView`, slot graph / timeline) and use
`$lib/magician/components/native/*`. `ExecutionResponsibilityPanel.svelte` is the
responsibility read model (`src/lib/types/executionResponsibility.ts`); `/debug`
still mounts it. Pause / Resume / Steer / Stop go through
`src/lib/magician/execution/controlClient.ts` against
`GET /api/magician/v2/executions/{id}/control-state` and mutate only a task's
`active_root_execution_id`. Internal tasks show one task per scheduled App
behavior with its next eligible run and latest outcome; App workflow runs appear
under Internal Tasks with controls linking to Apps (the app lifecycle owner
rejects generic Stop/Delete).

**MUIJ / GAUI.** Agent-generated and published surfaces render through
`MuijRenderer.svelte` and the catalog in `componentCatalog.ts`. Stable
product pages are native Svelte. Live agent documents (`agent.ui.delta`)
hydrate via `muijStore` on `/crew/[id]`. Contract:
[muij-native-ui-boundaries.md](muij-native-ui-boundaries.md).

**Realtime.** `src/lib/realtime/v2-websocket.ts` is the singleton WebSocket
manager. `event-taxonomy.ts` is generated from the Rust taxonomy
(`make event-taxonomy-codegen` / `make event-taxonomy-check`).
`event-filter.ts` is shared matching. Chat-turn activity uses SSE
(`GET /api/magician/v2/chat/sessions/{sid}/turns/{cid}/events[/stream]`)
with REST reconciliation in `chatTurnEventsStore.ts`: reconnect backoff
1s → 30s, 30s watchdog, 60s idle threshold. Browser upgrades offer
`magician-events-v2` (or `magician-voice-control-v1` on the voice-control
socket) before the auth-only `magician-bearer.*` protocol; the backend must
select the stable application protocol, or browsers fail an otherwise
authenticated upgrade. The shell indicator says "Realtime unavailable" — REST
history may remain healthy while this channel is down.

**Live calls.** The transcript store carries `assistantWorking` beside
`assistantSpeaking`: the `interaction.status` envelope
(`{"status": "in_progress" | "idle"}`) sets it, `session.ready` and
`response.interrupted` clear it, and the call overlay shows "Working…" between an
utterance and the answer (e.g. a non-blocking tool call). Providers without the
signal never show it. Call engine voices (GPT Realtime, Gemini Live, GPT Live 1)
are chosen in Settings and stored in media preferences; the engine picker renders
the backend catalog in arrival order (profile `display_order`, then id).

**HITL.** Canonical envelope `HitlRequested` / `HitlResolved`; respond via
`POST /api/magician/v2/hitl/{id}/respond` (`src/lib/hitl/`). `/attention`
is the inbox. TopBar Attention opens the compact center. Other surfaces
show count + CTA, not response widgets.

**Stores.** `src/lib/stores/` — `taskStore`, `agentStore`, `chatStore`,
`muijStore`, `botStore`, `approvalStore`, `attentionStore`,
`pendingHitlStore`, `scopeIdentityStore`, `secretVaultStore`,
`resourceAuthorityStore`, `feedStore`, plus chat-turn / vibe / observe
stores. Theme selection is `src/lib/shared/stores/themeStore.ts`. New pollers use
the shared poll + jittered backoff.

**Theme and layout.** `VALID_THEMES` is 22 ids (11 light/dark pairs).
Default is `longhand`. Persistence key `magican-theme` (legacy
`magician-theme` is read once). Selection hydrates from
`/api/magician/v2/ui/preferences` and follows `ui.preferences.updated`.
`--app-content-max: 1320px` in `app.css` `:root` caps primary columns;
chat uses `--chat-col: 900px`; VibeDev workbench is full-bleed. The Tauri theme
mirror publishes all four typography tokens (including `--font-brand`) and its
private bundle ships every theme's families, so desktop-only windows resolve the
same faces.

### Settings

- **Engines** exposes the server-wide Decision Engine selector: **All Engines |
  Magician Only | Off**, persisted immediately for every connected device. The
  shared engine roster lists Grok CLI model IDs; `default` follows the installed
  CLI.
- **Shared settings.** `/settings` edits notes capture and provider, canonical
  workspace storage (`/api/magician/v2/workspace-storage/settings`), FluidAudio
  and surface profiles, and shared voice preferences (auto-speak, per-surface
  profile and stage overrides). The desktop app keeps host permissions, the Orb,
  shortcuts and the local container.
- Child panels (terminal grants, device pairing, voice/audio, model routing)
  own their card, input and button tokens; parent `settings-card` classes are
  scoped away by Svelte.
- **macOS audio.** When `macos_speech` or `macos_tts` is in the catalog but
  unavailable, web shows Mac-only recovery guidance. System Voice needs the
  Desktop host gateway and staged speech helper; macOS Speech also needs the
  helper's Speech Recognition grant. A link can open the exact Privacy & Security
  pane; the signed Desktop checklist owns the request. Magician discovers these
  providers at boot, so it must restart after the gateway starts or the grant
  changes.
- **Observe account.** The verification-code permission explains the backend
  may read a code only while a verification request is open; passive observation
  alone does not grant it.

### Mobile and desktop devices

- **Mobile devices** offers separate **Connect iPhone** and **Connect Android**
  codes through the companion enrollment API; both grant ordinary mobile access.
  Before creating a QR, Settings asks whether the phone is on the same trusted
  Wi-Fi or needs the remote `connect.magican.ai` route; both addresses come from
  the server's roster response and the browser submits only the mode (a phone's
  `localhost` is the phone). An unavailable pairing store is shown separately
  from an empty roster and disables new codes until a successful refresh.
  Renewing an expired code keeps the platform. Web does not probe the retired
  browser Android Apps owner endpoints, so they cannot hide a connected iPhone.
  Android Apps enrollment, action review and recovery live in signed Magican
  Desktop Settings — see the
  [device bridge contract](../magician/device-bridge.md#android-apps-action-review).
- **Desktop Edge.** The roster recognizes `ios`, `android`, `desktop`; desktop
  rows are **Desktop Edge** and carry `edge_client`. In a Tauri view on a remote
  engine, **Connect this desktop** creates a one-time ticket through the
  authenticated WebView and hands only the ticket to native code for exchange and
  OS-keyring custody; revoking the current desktop removes server authority and
  the local keyring profile. In a browser, **Connect Desktop Edge** creates the
  same remote-only ticket as an **Open Magican Desktop** custom-scheme link plus
  copy fallback (no QR); the desktop verifies its selected server origin.
- **Android screen observation** is managed from the same panel. It discovers an
  enrolled Mac through Desktop Edge. Two server-derived trust choices —
  **Private / self-hosted build** and **Google Play release** — each list missing
  prerequisites and stay disabled until signer, app-version and attestation
  policy are ready. A local macOS page uses an origin-fenced loopback POST (with
  the `magican://android-observation` deep link as fallback); a remote server
  uses the bounded `host.android-observation/open-settings` Edge capability.
  Both only open the bundled desktop approval surface, so browser JavaScript
  never reaches owner signing, fingerprints, receipts or recovery authority; the
  desktop signs the final request with the carried trust method and cannot
  change it.

### Observe stats

`/observe/stats` separates queued distillation within the history window from
expired work and history awaiting retirement, and labels a zero-rate nonempty
queue as queued. Throughput counts completed summaries and mechanical
skips/coalescing; expiration and window changes are not processing. Source
records and prior summaries are retained. Optional queue partition fields may be
absent on older servers. Tests: `make test-distill-history`.

## App surfacing

An **app destination** belongs to an installed package and uses that
installation's permissions and projected records, distinct from a built-in page:

| Destination | Related built-in page |
| --- | --- |
| Brainstorm | `/thinking-maps` creates and manages live maps; the installed canvas displays projected map data. |
| Meetings | `/observe` hosts capture controls and history; `/meetings` redirects there, while `/meetings/[thread]` remains the transcript detail route. |
| Claims review | `/claims-review` reads the authoritative statement register, shows delivery provenance, and supports named review decisions; the installed app presents its permissioned projection. |
| Learning | `/memory` manages stored memory and `/feed` includes learning cards; the installed app supplies the dedicated learning review queue. |

Brainstorm, Meetings and Claims review declare interactive pages plus standard
view fallbacks; Learning uses a standard declarative view. The native Claims
Review page follows the Crew/Observe masthead, count-card, queue/detail layout
and needs no app install — see [Claims Review](claims-review.md).

- **Destinations.** Command-palette app destinations resolve declarative view
  names through the directory's actual view routes (a `queue` view can live at
  `/`). A scripted destination that 404s loads its manifest-declared standard
  fallback view (same enabled installation, normal authorization) and explains
  the change; auth, stale-grant and service errors stay errors. See
  [custom surface setup](../magician/custom-surfaces-v1.md#operator-switch).
- **Surface loading** probes another host only after a typed 404 saying the
  requested surface kind is absent; network, contention, auth, stale-revision and
  host failures stay on their path and show their own error. Legacy no-script
  hosting remains after absent native and scripted surfaces.
  `AppSurfacePage.component.test.ts` pins request counts.
- **Navigation and mini-frames.** `lib/apps/appNavigation.ts` +
  `lib/stores/appNavigationStore.ts` mount manifest-declared navigation.
  `lib/apps/appMiniFrame.ts` + `AppMiniFrameHost.svelte` are the page-bounded
  mini-frame host (per-page and per-session budgets, unmount TTL, visible-frame
  limit), admission-gated because a mini-frame is the reviewed escalation for
  bespoke rendering; declarative-native widgets are the ordinary path.
  `lib/observe/observeRetreat.ts` lets the meetings app surface stand where
  first-party `/observe` UI did, additively.
- **Scripted surfaces.** The kernel mints the host plan's `entry_url` with the
  live session reference as its first asset-path segment — the asset route's
  credential, since the sandboxed opaque-origin frame can send no headers.
  `appScriptedSurface.ts` pins the path to exactly the plan's `session_ref`,
  digest and document (anything else, or any query, is refused and nothing
  mounts); relative subresources inherit the session binding.
  `AppScriptedSurfaceHost.svelte` submits frame messages through a per-frame
  `ScriptedSurfaceBridgeSubmissionFifo`, preserving the kernel's strict sequence
  order despite HTTP/2 reordering; each success/failure settles in its item and
  replies stay keyed by request id. See
  [custom-surfaces-v1.md](../magician/custom-surfaces-v1.md).
- **Widgets.** `AppSlotRegion` resolves a page's regions through one max-twelve
  slot batch and renders unique targets through one native-model batch. Today
  fits `primary` + `secondary`, Observe `reviews`, entity surfaces `contextual`
  (after the surface resolves). Empty or unavailable assignments expose the
  picker across the bounded 512-widget inventory; cursor pages must keep the
  same inventory digest and assignment posts echo the picker package binding.
  Conditional bodies bind scope and bearer revision (widgets also slot, package
  digest/revision, installation generation, ETag). No widget app JavaScript
  runs; widget actions carry the rendered installation generation and package
  revision as a stale-write precondition. `renderAppWidgetBatch` renews each
  retained item's `refresh_after` on `304`, and a missing/regressed/passed
  deadline defers the next revalidation by `APP_WIDGET_REFRESH_FLOOR_MS` (2 s)
  instead of throwing. `AppSlotRegion` keeps the last good render for up to
  `APP_WIDGET_MAX_STALENESS_MS` (5 min past deadline), shows a quiet loading tile
  on first render, and treats a `workspace_default` slot hidden for any reason
  other than `disabled`/`update_pending` as empty. `AppIndicatorRegion` reads
  materialized values but is not mounted in TopBar: passive app status stays on
  the app's page and manifest indicators are inert until the backend gains an
  exact selector/aggregate. Not provided client-side: a trusted system
  boot/default/navigation owner.
- **Update reviews** accept the backend's custom-surface, scheduled-behavior,
  event-behavior and owner-notification permission diff axes; omitted axes
  normalize to `unchanged` (as in Rust) while unknown fields and invalid change
  kinds reject. Code-only updates may have dataset generation zero; a record
  migration needs a positive destination generation. Update approval correlates
  with the migration plan's destination revision (initial installs require
  equality with the installation revision). Approval receipts accept declared
  custom-surface routes.
- **Action runs.** Apps action dialogs accept an existing run reference and
  verify its installation and action before exposing status and cancellation, so
  controls survive lost browser history.

### Town Square (app-backed)

Uses the shared Apps live collection adapter: 25-row pagination,
websocket-driven catch-up, new rows first while opened older pages remain,
content-visibility for offscreen posts, paused when hidden, backoff and
reconnect catch-up, Apps query/change protocols only (no LLM). See
[the reusable SDK contract](../magician/typescript-apps-sdk.md#paginated-live-collections).
Head refreshes update overlapping rows by post id and keep the older-page cursor
at the loaded boundary; a burst with no overlap opens pagination at the new head;
a complete server snapshot replaces the list; scope change discards state.

An empty roster bootstraps through the installed app's governed `sync_roster`
action with the exact installation generation/revision and a stable
per-generation idempotency key; the page follows the durable run (reattaching via
Apps' scoped run history) and offers Refresh roster. Failed or waiting runs are
visible. Roster setup grants no posting policy or opt-in; opted-out agents are
listed with explicit status and cannot be mentioned or participate.

### Delete older app data

Apps installation cards offer **Delete older data…**; Town Square's social
component and Storage's Apps database card link to the same dialog. Timestamp
options load lazily (nothing added to app startup). Choose record type, date
field and cutoff (local midnight), preview the count, then confirm permanent
deletion; changing a criterion clears the preview. It shows deleted, remaining,
changed and referenced counts, advances bounded batches, can pause/stop, and
reopens saved progress without silently resuming. Closing stops after the
in-flight request; scope or installation changes close it and reject late
responses. Physical reclamation is a separate Storage action.

### Storage: App database

Its own Storage section shows the path relative to the runtime root, encrypted
format and WAL/SHM sizes, and offers only owner-inventory maintenance actions.
Integrity reads "not checked in this session" until a scoped response arrives.
Checkpoint/optimization and compaction preserve records and report checkpoint
status, verification time and shared-memory size. Missing SHM inventory on older
servers shows as unavailable, not zero. Maintenance never dispatches a model or
an App workflow.

## Analytics

Microsoft Clarity project `vvj2b1qbs0` is loaded from `src/app.html`
`<head>` after `load` / idle. Loopback and `.localhost` sessions skip the
remote tag: development navigation must not incur recording overhead or
errors from the analytics script.

## Local development

```bash
make run-ui-dev          # http://localhost:5173, proxies Magician :3002
make check-ui            # svelte-check
make test-ui             # Vitest unit + component + coverage
```

The UI imports the Apps TypeScript SDK source from `sdk/typescript`. UI build,
check, dependency setup, and dev Make targets install that package's locked
dependencies too, including `@noble/hashes` for contract BLAKE3 verification.
For direct npm use, first run `make setup-ui-deps`, then
`cd ui/unified-ui && npm run dev` (or `npm run build`). See [testing.md](testing.md).
