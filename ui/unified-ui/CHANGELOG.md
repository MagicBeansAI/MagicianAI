# Changelog - Unified UI

## [Unreleased]

- Add per-operation local/cloud Decision Model primary, backup, thresholds, and saved/active routing controls.

- App install review wording states the real key rules: declared hosts, the host(s) the owner picks, and any site only through the per-key tick.

- 0.1.31: App install review shows each tool's reachable hosts, a per-key host multi-select for tools that declare none and an "any site" opt-in with a warning; the Vault page shows a pending approval's domain set or "All sites".

- App widgets no longer flip to "temporarily unavailable" on unchanged 304 refreshes or near-now deadlines: item deadlines renew, past deadlines wait a short floor, the last good render stays through refreshes and transient errors (5 min cap), first load shows a quiet loading tile, and a hidden workspace-default slot reads as empty.

- Notes library: search, folders, and the reader stay in /notes. Published task notes open there instead of the old notebook site.

- 0.1.30: App install review adds a Network table (runs in place from <skill>, hosts or "any website") and an "any public website" checkbox that starts off; the Secrets table says where each key can go and when it is a config file.

- 0.1.29: Today's operations carousel (Economics with a stat grid, State of Operations, State of the Crew) with dots and auto-advance. Failed follow-up/worth actions now report errors and roll back instead of toasting success; snooze and dismiss offer only what the server supports; the Realtime Wire no longer seeds placeholder items.

- 0.1.27: App install review lists each key a tool needs, with the one host it is sent to; keys start unticked.

- Verify Claims Review write acknowledgements before clearing retries or reporting success, and show the current commitment status on receipt replay.

- Recover saved claim confirmations after navigation and retain decision retries when successful HTTP responses contain no valid JSON object.

- Give edited Claims Review imports a new conversation key while preserving exact retries for interrupted imports.

- Prevent stale commitment reads and fix Claims Review app paging, detail refresh and full statement display.

- Structure task deep panel timeline delegations into bounded branch envelopes featuring agent identity, active subtask time window, step count, and settlement/handback footer, with view switching between Grouped and Chronological modes always accessible across runs and styled cleanly using system theme tokens.

- Add native `/claims-review` with searchable review history, delivery status and relationship commitments, and align the installed app’s design while retaining Envoy statements without relationship bindings.

- Display and dismiss service-health notices for provider authentication, credit/rate limits and outages.

- Add the shared All Engines / Magician Only / Off Decision Engine selector to Settings → Engines.

_Current development version: `0.1.27`._

- Opening a chat from History renders the requested chat first — ~120 ms instead of ~5 s.

- Resolve app destinations to declared routes, review exact interactive-page grants, show standard fallbacks, and support localhost frames without changing hosted CSP.

### 2026-09-27 — 0.1.25 — Observe Command Deck & Modern Four-Pane Console

- Modernized the `/observe` console into an interactive Command Deck with high-affordance KPI cards (Now & Live, Sources on, Audio Profiles, Notes & Recents) that map 1:1 to views with semantic icon badges, hover depth, active glow rings, and responsive grid layout.
- Elevated the Capture Launchpad to the top of the Now pane for instant capture triggers (Listen, Join as agent, Watch screen) with seamless expandable inline configuration cards.
- Refactored section navigation into a sleek segmented pill tablist with live counter badges and icon anchors.
- Polished the Audio pane with structured profile containers, preserved all live capture promotions above the pane deck, and maintained 100% test contract compatibility.

### 2026-09-27 — 0.1.24 — Crew Fleet, Single Agent Canvas & Horizontal Scroll Containment

- Upgraded the Crew Fleet (`/crew`) surface with KPI metric cards, server-side pagination controls, fleet search, kind/status filters, and responsive layout.
- Modernized the Single Agent detail page (`/crew/[id]`) with a 2-column responsive dashboard grid for Overview, segmented pill tabs, and strict horizontal scroll containment across the shell (`.layout-v5 .v5-main`, `.presto-main-pane`, and native components).
- Preserved all existing contracts: 5 operational tabs (Overview, Memory, History, Corrections, YAML Settings), Presto GAUI interaction handlers, and task panel drawer execution.

### 2026-09-27 — 0.1.23 — Morning Edition Today cutover & Realtime Wire

- The new Morning Edition newspaper layout is promoted to the canonical `/today` surface, featuring categorized swipeable brief cards ("All", "For you", "Worth a look"), daily economic ledger telemetry, responsive Masonry debug metrics, custom app panel pins, and square integration.
- Integrated a live collapsed Realtime Wire mini-feed ticker on Today with rolling event count badge, smooth animation, and expand-on-click filtering across Events, Insights, and Activity.
- Retired and deleted the legacy Today surface and its `/today?mode=legacy` route.

- VibeDev names the primary personal agent in the build prompt and uses neutral wording while that name loads.

- Install review and installed apps let the owner choose what memory each app may read, while in use and in the background.

- VibeDev keeps the project rail visible and moves the stage below the conversation on narrower screens.

- The VibeDev coding picker groups profiles by engine and lists blocked engines greyed out with their reason.
- The VibeDev profile selector text is centred, and the button beside the mic shows a visible options caret.
- App options lists runnable tools first and groups blocked tools by reason.

### 2026-09-25 — 0.1.21 — Desktop grounding eval follows CuaDriver 0.28

- The debug page's grounding recipe captures once with a capture-only `get_window_state` (0.28 has no `screenshot` tool) and single-quotes every JSON argument, which the shell had brace-expanded.

### 2026-09-25 — 0.1.20 — Engines card in Settings

- Settings → **Engines** holds the chat and background-run engine pickers (moved out of Terminal grants); the run picker is now "Background run engine", the default for schedules, monitors, autonomous agents, and API starts.
- Voice calls, the command palette, the chat bubble, and the war room send the composer's engine; "Make this the default for all clients" sets the server chat engine, and composers adopt a server change, live or on their next start; a browser with no pick sends none.

---

Older entries: `docs/archive/changelogs/unified-ui.md`
