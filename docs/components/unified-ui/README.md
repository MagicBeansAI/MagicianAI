# Unified UI Docs

Landing page for `ui/unified-ui` documentation.

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
- [Unified Work](unified-work.md) — tasks, Crew, Playbooks and Quality checks in one workspace
- [Unified task panel](unified-task-panel.md)
- [Task and execution UI architecture](threads-tasks-conversion.md)
- [HITL attention](../magician/hitl-attention.md)
- [Chat mode](../magician/chat-mode.md)
- [Quick start](../../quickstart.md)

## Feature docs

### Shell, landing, overlays

- [Root landing](root-landing-native-route.md) — `/` Magican manifesto landing
- [Public-site metadata](public-site-metadata.md) — prerendered marketing, social cards, crawler assets and preview noindex
- [HUD command bar](hud-command-bar.md) — Tauri `/hud` two-state summon
- [Notification overlay](notification-overlay.md) — `/notify-overlay` + `$lib/notify`
- [Personal tutor overlay](personal-tutor-overlay.md) — tutor / App Copilot draw
- [Warroom ops deck](warroom-ops-deck.md) — `/warroom` demo HUD
- [Composer dock](composer-dock.md) — chat composer dock on `/chat`, threads, HUD
- [Theme tokens](theme-tokens.md) — CSS custom properties, 22 themes
- [Auth session gate](auth-session-gate.md) — `(app)` bearer gate and workspace switch
- [Event hub](event-hub.md) — the app's one realtime socket; subscribe API, resume, resync

### Tasks, execution, history

- [Unified task panel](unified-task-panel.md) — one panel for tasks and runs
- [Task and execution architecture](threads-tasks-conversion.md) — task vs execution APIs
- [Tasks and history native surfaces](tasks-history-native-surfaces.md) — `/work`, `/work?thread=<name>`
- [Execution native panels](execution-native-panels.md) — native inspectors (not GAUI)
- [History native route](history-native-route.md) — `/history`
- [Monitors](monitors.md) — `/work?type=monitors`
- [Recurring App execution history](recurring-app-tasks.md) — one internal task per scheduled App behavior
- [Paged-list removal](paged-list-removal.md) — optimistic delete of a paged row
- [Structured response rollout](structured-response-rollout-controls.md)

### Attention, Today, square

- [Today attention lanes](today-attention-lanes.md) — `/today` Morning Edition: realtime wire, operations carousel, swipe deck / broadsheet, and the backend-backed lanes behind them
- [Attention pagination](attention-pagination.md) — `/attention` inbox paging
- [Browser notifications](web-push.md) — web push for asks: Settings toggle, service worker, answer buttons, bearer mirror
- [Town Square crew world](fleet-civilization-world.md) — `/square` Floor/Campus game
- [Agent update feed](agent-updates-feed.md) — `/feed` operator timeline

### Chat, voice, HUD, tutor

Chat deletion shows **Deleting…** until server cleanup finishes, prevents repeated
deletes, and reports failures without removing the row. Opening a conversation
lands instantly on its newest page; earlier history loads one page at a time
while preserving the reading position. Message links still open their target.
Historical activity loads near the viewport or when opened; live activity stays
subscribed. History timestamps and server-paged ordering refresh on chat activity
without blanking the list or changing its filters.

- [Chat turn progress](chat-turn-progress.md) — live turn card
- [Concurrent voice requests](concurrent-voice.md) — independent background questions, coordinated playback, context selection, cancellation
- [Wake word](wake-word-voice.md) — ambient Orb admission (not a composer button)
- [Voice call boundary](voice-call-boundary.md) — display-only call boundary
- [Voice resume framing](voice-resume-framing.md) — resume context is reference-only

### Operator surfaces

- Unified Work interface plan — archived completed interface and core migration; the maintained Unified Work contract owns current behavior.
- [API mining](api-mining-native-route.md) — `/api-mining`
- [Claims Review](claims-review.md) — `/claims-review`, Envoy capture, native/app parity
- [Crew native forms](crew-native-forms.md) — `/work/crew/new`
- [Memory native route](memory-native-route.md) — `/memory`
- [Taste proposals](taste-proposals-panel.md) — `/memory` taste review
- [Notes search](notes-search.md) — `/notes`
- [Town Square posture banner](town-square-posture-banner.md) — `/town-square` status line
- [Observe channel toggles](observe-channel-toggles.md) — `/observe` Mail & chat card
- [LLM spend breakdown](llm-spend-breakdown.md) — `/llm` Spend
- [Model routing panel](model-routing-panel.md) — dedicated Settings operation/profile/model routing page and engine-affinity precedence
- [Settings page](settings.md) — `/settings` categories, sticky nav and scroll-spy, live-filter search, mobile chip bar, `?embed=1`
- [On-device generation](settings-local-generation.md) — Settings kitty pin (`runtime.ollama.local_generation.selected`) with RAM-tier warnings and an Ollama reload
- [Plane terminal grants](plane-terminal-grants.md) — Settings `plt_` grants
- [Budgets](settings-budgets.md) — Settings per-run limit, daily spend limit, today's spend and top-up
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
| `/vibe` | VibeDev coding cockpit; `?tab=workbench` for CLI sessions; trusted projects review and land their branch in the [Branch tab](vibedev-trusted-branch.md); the composer's engine picker (`shell/CodingEnginePicker.svelte`, also the compact `@vibedev` chip in `FloatingComposer`) groups profiles by engine with a one-line note on whose login each runs, shows unavailable engines with their reason inline, and shows what Grok's MCP gateway can reach (`gateway` on the profile row) before it is chosen; project settings (`shell/vibe/ProjectBuildSettings.svelte`) show where the project's checks come from (project, repo file, task runner, preset) with Customize / Use detected, and edit its build caches (env var + optional path, with presets) and the extra folders coding runs may write (`sandbox_write_paths`); the build stage (`shell/vibe/stage/StudioStage.svelte`) collapses to a strip on the right, remembered per browser (`vibeStudioStore.stageExpanded`); from 769px to 1280px it opens as a drawer over the conversation from that strip (closed on load, slides out before collapsing; see `docs/VIBEDEV_COCKPIT.md`); the rail loads the project's runs 10 at a time from `GET /vibedev/projects/{id}/runs` (`vibeDevRunsStore`: *Load earlier runs*, re-read after each task-list load and every 10 s while a loaded run is live) with placeholder rows, project name and lifetime-cost placeholders until data arrives; per-run LLM costs query only the days since the oldest visible run, and the lifetime rollup runs in its own `lane` so it does not hold them back; the scope-wide `vibedevInternalTasks` feed is opt-in (`taskStore.useVibedevInternalFeed`, used by `/llm`), and overlapping `loadTasks()` calls within a second join one load; *App options* (`shell/vibe/AppOptionsPanel.svelte`) fetches each authoring catalog only when its tab is first visited, retains entries and selections while the cockpit is mounted, and retries only the selected tab; a slow or failed tab does not block another tab; the spine shows `coding.handoff` / `coding.session_too_large` as notice cards when a switched engine is handed the run's earlier turns, naming engines as Pi, Claude Code, Codex, Grok and Antigravity (`spineModel.codingEngineName`); the composer shell (`shell/VibeComposer.svelte`) stacks at `z-index: 5`, above the spine's sticky *Jump to latest* and stream-health pills, because its `backdrop-filter` makes it a stacking context that would otherwise keep its menus (engine picker) beneath them ([coding run handoff](../magician/coding-run-handoff.md)) |
| `/coding-runs` | "Work in progress": every work item from `GET /work/live` (`workLiveStore`) — coding runs with Steer, Stop and Open (`LiveRunsStrip`), crews and Work tasks with Open and Follow; the desktop notch side panel shows it when a runs-ear dot is tapped, `?run=` or `?item=` highlighting that item |
| `/chat` | Rich chat (attachments, grouped task progress, session history) |
| `/t/[name]/chat`, `/t/[name]/settings` | Thread workspace; bare `/t/[name]` redirects to chat; thread work opens `/work?thread=…` |
| `/chat/files`, `/t/[name]/files` | A chat's uploads and task artifacts: sizes, downloads, delete a chat copy |
| `/chat/models`, `/t/[name]/models` | Per-model calls, tokens, cache and cost for a chat and its delegated work. Refreshes on a viewer-chosen interval (default 1 min, kept per browser) with a countdown, only while the tab is visible (`stores/visiblePoll.ts`) |
| `/work` | Unified work; Tasks, Monitors and System work filters, Needs you, [Goals and target dates](../../features/goal-workspaces.md#work--goals), Crew, Playbooks, Quality checks and Team routines |
| `/observe` | Listen / join-as-agent / watch screen |
| `/reviews`, `/evidence` | Impact reviews and work-evidence inbox |
| `/claims-review` | Statement register review |
| `/town-square` | Agent social feed (TopBar / ⌘K "Town Square") |
| `/square` | Crew world (2D Floor / 3D Campus) with Today below |
| `/work?view=crew`, `/work/crew/…` | Crew management and full member inspectors/editors; live member surface |
| `/attention` | Canonical HITL inbox (approvals, clarifications, pauses, escalations) |
| `/vault` | Provisioned secret CRUD and setup-token management |
| `/channels` | Telegram/WhatsApp bot runtime, QR pairing, logs |
| `/api-mining` | Learned APIs + noisy-origin triage |
| `/skills`, `/skills/evolution` | Skill/pack catalog and evolution |
| `/history` | Sessions and threads |
| `/memory` | User memory + taste proposals |
| `/notes` | Notes, audio notes and search; `/notes?path=…&workspace=…` is the canonical relative note link (switches workspace when the session can, keeps the address bar on the open note, Copy link gives the absolute form; `lib/notes/noteLinks.ts`); the reader hides YAML front matter (`noteFrontMatter.ts`), the editor keeps the whole file |
| `/triggers` | Trigger inventory |
| `/budget` | Resource Authority dashboard (`/budget/tokens/[id]`, `/transactions`, `/audit`) |
| `/llm`, `/llm/queue` | LLM observability and dispatch queue |
| `/runtime`, `/runtime/resources` | In-flight spans; observe-only local resource governor |
| `/storage` | Storage sizes, retention, Track B activation |
| `/settings` | [Eight categories with search](settings.md), ending in About → Versions: trust policy, config reload, theme, devices, plane grants, [harness credentials](../magician/harness-auth.md) (`lib/settings/HarnessAuthPanel.svelte`: signed in / session expired / not signed in with the CLI's reason and remedy; Magician's key only after approval), [budgets](settings-budgets.md) (`lib/settings/BudgetsPanel.svelte`) |
| `/evals` | Eval-lane dashboard |
| `/thinking-maps` | Live thinking-map boards |
| `/feed` | Operator agent-update timeline |
| `/events` | Unscoped live event stream |
| `/mirror` | Execution-scoped event log |
| `/debug` | Direct browser actions and SOTA fixture launcher |
| `/warroom` | Demo ops deck |
| `/hud` | Tauri collapsed command bar |
| `/dev` | Full-screen workbench (sessions also appear under `/vibe`) |

The VibeDev App options catalogs keep separate loading/error states and request controllers. Closing and reopening the menu preserves loaded tabs and selections; destroying the cockpit aborts outstanding requests. An unvisited tab still needs its live authoring endpoint: Tools, Agents and Procedures each resolve the current primitive catalog on the server. Exact-content validation caches are warmed during server startup, so unchanged skill manifests are not parsed again when opening the menu. Independent tab loading also removes the wait for unrelated catalogs.

Local App options qualification (2026-10-12): first Tools render 3.7 s → 403 ms; first Agents/Personalities/Procedures visits 308/26/269 ms; reopen 14 ms with no catalog request, selections retained, and unchanged counts of 178/30/8/18. Fresh CLI discovery returned the same tool entries as the warmed HTTP catalog.

Compatibility redirects: `/home` and `/desk` → `/today`; `/approvals` →
`/attention`; `/meetings` → `/observe`; `/about` → `/#about`. The old Tasks,
Crew, Internal Tasks, Relentless and harness route aliases are removed. Empty `dashboard/` and `v2/`
directories are not routes.

Tauri overlay routes (siblings of `(app)`, no TopBar): `/hud`,
`/contextual-assist` (native-only placeholder in a normal browser),
`/draw-overlay`, `/notify-overlay`, `/screen-region-picker`.

## Architecture

The v5 TopBar shell is the only `(app)` shell. `(app)/+layout.svelte` mounts
`TopBar`, `CommandPalette` (⌘K), `HistoryDrawer`, `AttentionCenter`,
`AtmosphereLayer`, and the attention prompt modal. Primary nav: Today, VibeDev,
Chat, Work, Observe, then any tab an installed app declares. Briefing, Apps,
Reviews and Square stay in the command palette (Square under Jump to; the others
under Manage). The palette jumps to **Work** (`G T`); its searchable description
includes Tasks, Crew, Monitors, System work, Playbooks and Quality checks.
The left icon rail omits Quality checks and Reviews; Quality checks remains
under Work and Reviews remains searchable in the command palette.

**Command palette.** The closed palette preloads nothing (agents, tasks,
sessions, approvals, notes settings); opening it refreshes those stores
independently, keeps usable cached data, and reports failures without unhandled
rejections — keeping palette-only requests off initial navigation. Filtering
uses `commandFilter` (`src/lib/shell/commandMatch.ts`), not bits-ui's
subsequence scorer (which ranks unrelated phrases): NFKC-normalised,
case-folded, whitespace-collapsed; a command matches when the whole query is a
prefix/substring of its text or every query word is an exact or prefix word of
it; anything else is hidden. Root results are filtered before rendering, so
empty sections, including asynchronously loaded Apps, are omitted. Exact label
matches lead, followed by label prefixes/word matches, then supporting text and
IDs. Each section is ordered by its strongest match, with stable section order
for ties. Svelte owns this root ordering; bits-ui handles keyboard selection and
filters nested flows. An empty query retains the usual sections and Suggested
items ranked by use and context.

**Archived items.** Every archivable list reads its archived rows from the
server, paged (`archived=only`): the history drawer and chat rail have an
Active/Archived switch for sessions and threads; Tasks has an **Archived tasks**
toggle (`ArchivedTasksList`, read-only, rows expand to the description, with
the shared `ServerPager` above and below and Work's page sizes);
internal tasks take Status = archived; the VibeDev rail lists archived projects
under their own label and its Runs section has an **Archived** toggle whose rows
open the run in internal tasks, with a server search over all the project's
archived runs and a page footer like the chat rail's.

The history rail and the history drawer show Archive and Delete on hover for
every thread except `#general`. The row reads Archiving…, Restoring…, or
Deleting… until the server finishes. A long thread name stays on one line,
and the full name is in the tooltip.

The chat history rail marks sessions with current chat/task activity as
**Working**, separately from selection or active/archived status; a bounded
snapshot refreshes every three seconds while the rail and browser tab are visible.

Selected God-mode auto-approve controls use the theme's paired primary-button fill and foreground for readable contrast.

**Work in progress.** `/coding-runs` ("Work in progress") lists the owner's
long-running work: every project's going, waiting and just-ended VibeDev runs
(`LiveRunsStrip`, `vibeDevLiveRunsStore` over `GET /vibedev/runs/live`) with a
state dot, Steer in place, Stop after a two-second undo, and Open in `/vibe`; a
working run whose `running` turn has no progress for the verdict's
`STALL_AFTER_MS` reads `Stalled · no progress for …` (`liveRunStalledForMs`); a
`notice` shows with Acknowledge, and Review (Answer for a `decision`) only on a
needs-you run;
then one row per crew and per Work task from `workLiveStore` (`GET /work/live`,
the summary the phones show) with its state, step and spend, Open (the item's
`web_path`) and Follow/Unfollow (`POST /work/follow`, `DELETE
/work/follow/{item_id}`; a 409 `follow_limit` past three shows as a notice).
`workLiveStore` polls every 5 s while anything works or needs the owner, else
30 s, and re-reads on run events; `start()`/`stop()` are reference counted. The
notify overlay sends its items to the desktop notch's runs ear as dots
(`notchRunDots` → `publish_notch_runs`: item id, state, `title · context`,
`path`; a crew is one dot, a stopped item none, a done one stays two minutes,
the latest four). Tapping a coding-run dot opens `/coding-runs?run=<id>` in the
notch side panel with that run highlighted; a crew or task dot opens
`?item=<item id>`; a later tap moves the highlight through
`notch://runs-select` (`{runId, itemId}`). Attention does not list runs. The VibeDev rail's **Crews** section (`CrewSection`,
`vibeDevCrewStore`) plans a crew from a goal and optional budget on a trusted
project, shows each crew's lanes with their state, starts a proposed crew (the
same question also waits in Attention) and stops a running one; a lane opens its
run. A finished crew shows its spend, merged branch and pull request (or the
conflicting lane with **Resolve with an agent** or **Merge again**). The section
lists only crews still in play (`?state=open`), plus any the owner acted on there
since the project loaded; finished crews move under **Show N earlier crews**,
which pages them from the server five at a time (`loadSettled`, `?state=settled`). A new crew can split the goal or race engines (2–8
attempts, then **Keep …** per finished attempt) and can ask for a review of the
merged branch; integrator and reviewer lanes are labelled. A run started as a crew lane shows its lane as a chip on
the cockpit's run row (`runLinks` from the runs page's `links`) and in the strip.
Crew actions use the long fetch timeout (planning runs a model; a pull request
pushes), track busy crews one by one, and drop a result that is older than the
last applied read or that belongs to a project the owner has left. A Stop
pressed in the strip still goes out if the strip closes inside the undo window,
and goes to the run's newest live turn at send time.

Up/Down and Ctrl+J/K move the selected command. Plain H/J/K/L navigate when
the results list has focus (Tab from search); navigation preserves that focus
across nested pages, and `/` returns focus to search.
Left/Right and Ctrl+H/L go back or open a selected nested flow. In the search
field, plain letters stay searchable and horizontal arrows move the cursor
until it reaches the corresponding end of the text. Right opens nested pages
only; final actions and submissions still require Enter. The shell ignores
keyboard events already consumed by the focused surface, so Ctrl+J does not
open quick chat while navigating the palette.

**Auth.** The root `+layout.ts` redirects to `/login` when no workspace
bearer is present (hydration-aware for Tauri) on every route that
`isPublicRoute` does not list — the marketing pages, `/login`, and the theme
specimen sheet are the only public ones. Mount-time `refreshScopeSession()`
and the scoped-fetch 401 re-gate catch a dead token. See
[auth-session-gate.md](auth-session-gate.md).

**Desktop session restoration.** Tauri webviews restore the selected server's
native credential-store snapshot; an old browser-stored bearer cannot replace
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

Steer drafts survive same-run refreshes; responses from a previous execution or credential scope cannot clear the current draft or report success for it.

Composer docks page through every open ask (`completePendingFeed`) with a Work
plan-review fallback; an answer counts only on `accepted`/`resumed: true`, a
closed plan approval never starts a run, and a credential change clears pending
asks.

**MUIJ / GAUI.** Agent-generated and published surfaces render through
`MuijRenderer.svelte` and the catalog in `componentCatalog.ts`. Stable
product pages are native Svelte. Live agent documents (`agent.ui.delta`)
hydrate via `muijStore` on `/work?view=crew&agent=<id>`. Contract:
[muij-native-ui-boundaries.md](muij-native-ui-boundaries.md).

**Realtime.** `src/lib/realtime/eventHub.ts` owns the app's ONE realtime
socket (`/api/magician/v2/realtime/ws`): it reconnects forever (jittered,
≤ 30 s, at once on foreground/online), resumes by `(epoch, seq)` when the
server says `hub.hello`, dedupes, and fans `hub.resync` out to subscribers.
Features `subscribe(filter | predicate, onEvent, onResync)` and keep history
as one REST request per mount/resync; no feature opens a live `/v3/events`
or chat-turn stream. `v2-websocket.ts` (`v2Events`) is a frame listener on
the hub that routes frames into the stores. Contract and consumer list:
[event-hub.md](event-hub.md). `event-taxonomy.ts` is generated from the Rust
taxonomy (`make event-taxonomy-codegen` / `make event-taxonomy-check`).
`event-filter.ts` is shared matching. Chat-turn activity
(`chatTurnEventsStore.ts`) reads the turn projection over REST
(`GET /api/magician/v2/chat/sessions/{sid}/turns/{cid}/events`) and takes
live events from the hub; a 30s watchdog re-reads a turn idle for 60s. Browser upgrades offer
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
`POST /api/magician/v2/hitl/{id}/respond` (`src/lib/hitl/`); every answer path
posts the server's `respond` handle when present (`readHitlRespondHandle`,
`hitlCallFromHandle`; see
[respond handle](../magician/hitl-attention.md#respond-handle-respond)). `/attention`
is the inbox. TopBar Attention opens the compact center. Other surfaces
show count + CTA, not response widgets.

Desktop's notch hosts this same inbox with `native_attention=1&notch_attention=1`:
compact rows, search, category counts, server pagination and the shared prompt
modal fit the original docked panel. It hydrates the selected native session
through the origin-bound `magician-session` bridge. Full review destinations use
the bounded `magician-attention` application router; passive inbox/count updates
do not take keyboard focus. The notification overlay derives its summaries and
total badge from the canonical Attention stores, including durable requests.

**Stores.** `src/lib/stores/` — `taskStore`, `agentStore`, `chatStore`,
`muijStore`, `botStore`, `approvalStore`, `attentionStore`,
`pendingHitlStore`, `scopeIdentityStore`, `secretVaultStore`,
`resourceAuthorityStore`, `feedStore`, plus chat-turn / vibe / observe
stores. Theme selection is `src/lib/shared/stores/themeStore.ts`. New pollers use
the shared poll + jittered backoff.

**Theme and layout.** `VALID_THEMES` is 22 ids (11 light/dark pairs).
Default is `sorbet` (light; dark is opt-in — `longhand` until 2026-10-01,
and installs with a saved theme keep it). Persistence key `magican-theme` (legacy
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

- **Machines** (`lib/devices/MachinesPanel.svelte`, above Mobile devices) lists
  the owner's machines from `GET /remote-machines`, marks this one, and opens
  any other machine's own UI host (the web UI is same-origin, so another machine
  means another UI origin). The pairing flow says which machine its QR pairs
  with (`connection_options.machine_name`).
- **Mobile devices** offers separate **Connect iPhone** and **Connect Android**
  codes through the companion enrollment API; both grant ordinary mobile access.
  Before creating a QR, Settings asks whether the phone is on the same trusted
  Wi-Fi or needs the remote `connect.example.com` route; both addresses come from
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
  `RESERVED_FIRST_PARTY_ROUTE_ROOTS` holds every route root the shell serves
  (`/coding-runs` was missing until 2026-10-16), so no manifest can declare a
  route that impersonates a first-party page; `appNavigation.test.ts` scans
  `src/routes` and fails on any root left out.
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

App recording is an optional runtime setting (`MAGICIAN_CLARITY_PROJECT_ID`),
configured from shell, TUI or Desktop. After authenticated app navigation, the
UI reads only its public tracking ID from `/runtime/client-config` and defers
the tag until load/idle. Unset/invalid IDs and auth/network failures record
nothing; loopback sessions always skip it. See [Clarity setup](../../setup/clarity.md).
Website tags run only on marketing routes. Active or pending recording forces
a document reload across website, signed-out and app boundaries, so one
surface's recorder cannot continue into another without its own opt-in.

Standalone website recording is disabled unless the operator sets `PUBLIC_CLARITY_PROJECT_ID`
in the UI build environment. Its browser-visible tracking ID is not an export
API credential; never put an export token in public UI env. An opted-in tag loads
from `src/app.html` only after `load` / idle, and loopback and `.localhost`
sessions still skip recording. Public source and examples contain no operator
project or Cloudflare account ID; remote address examples use `example.com`.

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

`vite.config.ts` sets `optimizeDeps.entries` (every route and `src/lib` module,
tests excluded) and lists the packages only ever imported dynamically
(`vosk-browser`, `shiki` / `@shikijs/*`, `diff`, `cronstrue`, `@xterm/*`).
Without them, SvelteKit's dev server discovered dependencies lazily: the first
page to reach one (the attention wake word reaching `vosk-browser`, say)
re-bundled every dependency mid-session, modules transformed earlier kept the
old `?v=` hashes, those answered `504 Outdated Optimize Dep`, and routes such as
`/tasks` rendered the 500 page until the dev server restarted. A newly added
dynamic-only package belongs in `include`. Running `npm ci` (for example
through `make build-ui`) under a live dev server replaces its `.vite` cache;
restart the dev server afterwards (`make restart-ui-dev`).

## Relentless draft workspace

Manual Quality drafts use the shared model and thinking controls for drafting
and independent review. The Quality catalog restricts choices to bounded, priced
routes; the main catalog supplies model labels, supported thinking steps and
availability. Changing the draft family requires an explicit independent review
choice. Missing, unavailable or conflicting choices block guided review without
replacing the saved values. Preview still seals the generated route handles in
the existing contract; complete contracts and historical records retain their
exact contents.

The 2026-10-04 workspace opens on **Needs review** rather than the setup form.
The inbox contains agent briefs, waiting runs, accepted drafts and attached
captures awaiting policy review. Search, review-type/status filters, proposing
agent filters and 20-row pagination keep the visible queue bounded. Runs and
task policies have separate views; communication and learning panels first mount
when visited and retain editor/page state during navigation, clearing scoped
content when the workspace changes. Campaign/guidance catalogs page 10 policies;
capture lists default to awaiting review and page 20 records. Inspecting a bound
capture opens its exact artifact and provides a return action to the same review
page. Desktop uses adjacent list/detail panes;
mobile shows the selected detail with a Close action back to the list. All three
workspaces use the app's background, foreground, border, accent and status tokens.

Agent proposals show their originating agent/task/request, definition of done,
reference snapshots, handoff and budget before contract preview. Manual setup is
a deliberate **New draft** action, with models, limits, execution ownership,
complete JSON and recurring policies behind disclosures. Manual ownership uses
enabled configured agents, defaulting to the workspace's primary agent. Without
a primary, the owner must choose an agent; no built-in agent ID is assumed.
Scope changes clear the previous workspace's selection. The Tasks view lists
tasks already using Relentless; adding another task uses **Improve quality**
in its Work task menu and `/work?view=quality&task=<id>` path. OPC coordinators and workers retain their current
task roles: a coordinator delegates, the assigned agent proposes the brief and
references, and Relentless reviews its output. No new agent authority is granted.

The reference picker searches the configured primary notes provider and reads
the exact provider/path before attaching a full content snapshot. Note/fact-sheet
documents and UTF-8 text uploads carry their provenance inside `proposal.sources`;
task setup includes its goal and context revision as a reference. Bare links and
search snippets are not substituted for source content. Reference changes
invalidate preview; pending attachments block admission, and scope/credential
changes reject late reads. Starting a new draft or selecting task context also
invalidates pending reference reads within the same workspace. Binary/larger source files still use the complete
contract. The picker accepts text uploads up to 1 MB.

Run inspection starts with the artifact selected by acceptance, even if a later
attempt exists. Approval still requires the exact selected bytes to finish
loading. Prior-attempt comparison, attempt history, evidence, reports and learning
are deliberate actions. Detailed polling fetches only the selected unfinished
run every three seconds; catalog refresh is every thirty seconds and pauses while
the document is hidden. Pagination and search run on the authenticated server:
`/relentless/catalog` combines compact proposal/run rows with exact filtered
counts and agent facets. Unverifiable run records appear as blocked attention
rows with no invented spend or approval controls. Full run/proposal details load independently by ID;
changing pages preserves an open review. Tasks, campaign policies/captures and
guidance policies/proposals also use server pages. UI search is debounced and
late responses are rejected by query, request generation and workspace scope.
Limit/offset pages are live views; concurrent additions can move rows between
pages. Existing stores still scan scoped journals for discovery and counts; no
indexed-store or snapshot-cursor scaling claim is made.

`make test-ui-relentless` covers both workspace component regressions and scoped
reference resolution. An isolated synthetic frontend preview verifies light/dark
and mobile layouts without starting the runtime stack. This does not replace the
shared-stack/provider qualification tracked in the implementation plan.

The workspace's complete-contract JSON editor supports the full server proposal
schema beyond the simple draft form, including declared source sets, bounded
public research, explicit code checks and audio segments/provider pins. Editing
invalidates the preview; server resolution and strict contract validation still
gate admission. The profiles read exposes qualified audio-provider identity.
The artifact view can inspect cached fetched sources, code-check results and
audio receipts from the scoped evidence endpoint, with selection/generation
guards against stale responses. WAV/MP3 preview uses exact authenticated bytes
and revokes object URLs on selection/scope changes. Preview playback widens no
critic coverage. Runtime/provider qualification remains tracked in the plan.
Failed declared-file attempts keep their diagnostics in the run history. They
use bounded automatic repair when usage is known; they do not masquerade as
owner questions or accepted artifacts.
Changing the reviewed task context or selected proposal also discards a prior
complete-contract edit, preventing stale recipes from following new ownership.

**Work → More → Quality checks** previews a generic acceptance contract
and its bounded reservation, then creates a task-backed local draft run. The owner supplies a goal, source
notes and quality requirements. Scope changes clear drafts, cancel pending reads
and reject responses from the previous credential revision. The run view shows
attempts, sealed artifact versions, reviewer findings, settled/reserved cost,
absolute deadline, cancellation and matched-attention resume. Comparing attempts
renders their exact text side by side; artifact content is never executed as HTML.
Declared binary artifact files show their media type/size and use authenticated
downloads of exact sealed bytes. Downloads reject a changed workspace credential
revision. Stored task output links to `/relentless?run=<id>` clicked inside the app
normalize to Work → More → Quality checks and select the matching scoped run, preserving the accepted file set instead of decoding media as text. Shared
runtime/provider checks remain separate from the isolated UI checks.
An unfinished actor checkpoint matching the pending attention ID directs the
owner to the existing task question flow. Infrastructure blockers retain their
retry action; a status-only retry cannot stand in for answering a task question.
Acceptance is local and does not grant submission or sending authority.
The owner can require human review or let the frozen independent critic decide.
Pending approval names the exact artifact; automatic handoff remains local or
capture-only. Work hosts the complete workspace; the old `/relentless` page
is removed. In-app saved links preserve their query targets through the shared
Work normalizer; direct old bookmarks return 404. Task completion
preserves the reviewed bytes instead of synthesizing another result.
Displayed bytes bind to both the selected run and sealed artifact hash. Starting
another run clears the previous draft; late artifact responses cannot replace
the current selection, and approval waits for those exact bytes to load.
The panel-disagreement selector can admit bounded revision of qualified required
defects while preserving strict acceptance. This policy participates in contract
preview invalidation and is frozen when the owner starts the run.
API, browser and real-provider qualification are in progress; see
[Relentless](../magician/relentless.md) for current limitations.


The reusable-guidance panel reviews an exact owner evaluation/canary policy and
its frozen procedural body before starting qualification. It displays phase
settled/reserved costs, paired trial evidence, adoption slots and regression
monitoring, links exact runs/tasks, and supports version-bound revocation and
paused qualification resume. Editing the policy invalidates its preview; scope
changes abort reads and clear editor/review state. Adoption is limited to exact
recurring draft targets and never grants capture/sending authority. It
refreshes promoted policy monitoring, displays expiry/disable rules, and labels
unknown monitoring evidence without claiming it was within quality bounds.
Trial and monitoring rows can load their exact sealed evidence snapshot. Reads
must match the row's recorded hash and current scope; changing scope clears it.
Pending agent guidance proposals show the strict body and training run links;
preparing a policy seeds only candidate/agent/training provenance and activates
nothing. Cases, targets, budgets and expiry remain explicit owner choices.
The panel and automatic policy worker await the coordinated verification cycle.

The Relentless workspace also lists ordinary-task contract proposals from existing
durable owner requests. Owners inspect the exact proposed sources, requirements,
model choices and budget, preview the resolved contract, then start with its hash.
A changed preview cannot admit a different contract. New drafts default to bounded
revision of qualified required defects, while acceptance remains strict. Attempt
history shows evidence/identity/quorum validation reasons alongside raw scores.
The thread-task action menu links to `/work?view=quality&task=<task_id>`. That flow
loads the scoped task context, keeps its goal/agent/thread fixed, and reviews
criteria and budget before starting a native policy root on the same task.
Unsettled work shows its blocking reason. An interrupted admission retries its
original run ID and reviewed task-context hash. Scope changes clear the loaded
context and late responses are discarded. This UI and its regression are in
the implementation batch and have not been executed yet.

For an existing task, the workspace can activate the exact reviewed native
contract for future starts and scheduled occurrences. It requires a finite run
count and explicit expiry and displays the aggregate spend ceiling. Current
policy usage/expiry and a disable action are shown in scoped task context.
Agent proposals with matching native actor and goal may attach to their source
task after prior work settles. Binary proposal sources can be downloaded for
inspection before admission. Campaign policy preview validates bounded templates
and computes their content hash before exact owner activation. The workspace
shows clean simulated approval counts per identity; first captures and novel
content require review. Pending capture review also has a durable attention
notice whose dismissal grants no approval. Terminal runs with retained cost reservations expose
a receipt-only reconciliation action; it invokes no actor or critic. Run lessons
show current journal spend separately from their immutable publication snapshot.
Existing task forms accept an optional exact campaign target for one-off or
recurring capture handoff; the campaign panel exposes the target bindings.
Accepted-run views show campaign receipt status separately from local acceptance
and explain unavailable current coverage. Review links load the matching run
and campaign. These source paths still await batched UI/runtime qualification.
Run lessons are loaded on demand through the scoped
API; their observed scores/findings are evidence for review, not activated
procedures. These additions are source implemented and await batched UI/API checks.
Run reports are also loaded on demand, with a Markdown download and an action to
inspect the reported artifact. The report labels unaccepted and earlier-source
outputs and explains stop reasons, unresolved checks and unsettled charges.
Snapshots do not update while being read; inspect again after the run changes.
Scope changes clear reports and discard outstanding responses. This source path
has not been validated in the implementation-only batch.
Declared PNG/JPEG/WebP and MP4/WebM artifacts have an on-demand preview. It fetches
the exact file through the scoped download API, checks declared size/type, and
uses a browser-local object URL. Selection/scope changes discard stale responses
and revoke previews; closing the view revokes its URL too. Video playback is
owner inspection, separate from the critic's declared frame coverage. The
preview source is connected, with browser/type validation still deferred.

The on-demand work view groups existing task identities by an optional exact
goal identifier and shows task deadlines, policy allowance consumption, local
review/handoff state and settled versus reserved spend. It does not create
another roster or scheduler. Capture/local acceptance remains separate from the
existing external submission, delivery and obligation records.

Campaign captures are loaded on demand. The advanced owner policy editor shows
the exact structured policy/source document before activation and preserves
expected-version checks for successors and revocation. The counterparty register
supplies exact identity IDs; work-view details expose task-context and contract
bindings. Capturing the selected accepted artifact requires its exact bytes to
be loaded and an explicit matching target. Pending capture approval stays disabled
until that bound artifact is inspected. The view shows finite reserved slots,
simulated clean approvals and audit suspension, with zero production trust.
Scope changes clear the editor/catalog and reject late responses. This source
path is unvalidated during the implementation batch; nothing is sent.

A cold `/work` visit hydrates the agent catalog for its crew picker; creating
an ordinary task no longer depends on visiting Chat or Crew first. The shared
agent store retains scope-generation checks for late responses and scope changes.

Task panels render canonical pending run questions even before attention-feed
projection, preserving exact answer identities and deduplicating controls.
Relentless polling selects a newly created attempt and the exact accepted
artifact so a run started in the UI becomes reviewable without reopening it.

The shell and login use the shared `BrandStar` mark, selecting light or dark
assets from the surrounding theme text color. Browser, pinned-tab, home-screen
and Open Graph icons live under `static/`.

Shared HITL adapters preserve the pause storage key separately from the exact
revision-bound correlation ID. Direct task answers and canonical attention events
validate the documented 12-hex ask suffix and POST the full correlation ID, so
a new question on a reused pause cannot receive an earlier answer. Legacy bare
identities still work; mismatched or malformed pairs remain invalid.

Relentless review badges use a scoped class to avoid DaisyUI’s fixed-size
`status` indicator. Review rows constrain long titles so the review status stays visible;
review totals remain on one line alongside the inbox heading.

### Ordinary Work time grants

The chat composer's `/budget` task menu includes `+24 hours`. It records
`add_duration_secs: 86400` through the existing owner budget endpoint. Time is
added on the next resume, without clearing accumulated cost or token usage;
a budget-paused run resumes through the existing owner confirmation path.
Quality-owned attempts continue to use their reviewed round budget controls.

## Speaking voices

Dictation shows a Listening timer and microphone waveform; live call controls show a measured microphone/speaker waveform; muted input stays flat, playback follows the audio queue, and reduced-motion settings disable scrolling.

Settings exposes Text to speech voices and Live call engine voices. Composer
pipeline controls select a speech model and its speaking voice; Call controls
select a realtime model and its voice. `VoicePersonaPicker` persists partial
`tts_voices` / `realtime_voices` updates through the scoped media preferences
API, offers search for large catalogs and a cancellable TTS preview. Browser
voices retain their device-local controls; those preferences are read and written
only in the browser, never during server rendering. See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).

**Chat auto-speak** (`maybeAutoSpeakTail`, `lib/media/tts/autoSpeak.ts`) reads
only a reply that lands while the chat is on screen. `ChatPanel` takes the
tail of each conversation it shows as a baseline, so opening another chat
(thread switch, `?session=` link, chat tab) never reads its last message. A
voice-origin reply speaks even with auto-speak off, except replies already
spoken where they were produced: `realtime_voice` (the call) and
`meeting_room` (the meeting bot), and tutor/App Copilot narration.

All selectors inside Voice options (pipeline, stage/model, realtime model, persona, and browser voice/speed/pitch) use `AudioChoicePicker`: the LLM profile picker’s compact 28px trigger, theme colors, checked rows, and a bounded searchable popup. Popups are positioned from the trigger’s viewport rectangle, follow scrolling, and flip using their actual height so short menus stay next to the field. Arrow keys skip unavailable choices; Escape returns focus.

VibeDev's composer ask dock matches the project's chat session and every turn of the selected run,
and includes workspace God approvals while preserving their originating grant scope.
