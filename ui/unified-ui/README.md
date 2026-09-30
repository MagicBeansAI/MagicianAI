# Unified UI

Settings → Engines includes per-operation local/cloud Decision Model primary and optional backup selectors, model-specific thresholds, explicit remote-in-local permission, and saved/active route status.

SvelteKit frontend for Magician task/execution workflows.

**Use npm here, not pnpm.** `package.json` pins `packageManager` to npm, so a
stray `pnpm` command refuses rather than converting `node_modules` in place —
pnpm treats an npm-installed tree as foreign, moves it to `node_modules/.ignored`
and reinstalls. Every Makefile target for this package uses `npm ci` / `npm run`.
`desktop/` is the one project in this repo that genuinely runs on pnpm (see
`make setup-desktop-pnpm`); the two do not mix.

**Current development version:** `0.1.31`.

Version `0.1.8` completes the Magican public-domain cutover, adds directly
openable `/privacy`, `/manifesto`, and `/terms` pages for
`next.magican.ai`, and pins npm 10.9.3 as this package's package manager.

The landing narrative keeps its canonical Magican manifesto copy in
`src/lib/landing/manifesto.ts` and its bounded activity/hold cadence in
`src/lib/landing/lifeCycle.ts`. Contract tests pin the short landing excerpt
against the complete manifesto, the finite activity sequence and closing
“all your side quests” hold, the `/manifesto` handoff, and mobile access to
that public route. These modules are the reviewable source for landing copy and
timing rather than browser- or model-authored content.

`0.0.809` is the Magican landing with HowTrack: five stations, product
screens that cycle inside one chrome window, carousel icons, Today CTAs,
the real chat composer, and a slow float.

`0.0.808` adds actionable mobile-enrollment recovery for outdated routes and
unavailable durable pairing storage. `0.0.807` kept the production bundle
warning-free by giving the composer
drop surface an accessible group name and removing selectors left behind by
retired UI. It retains `0.0.806`'s generated realtime event taxonomy alignment
with Magician `0.6.1275`, including the user-visible Recipe cancellation
request. The VibeDev composer picker also shows **Claude** (`claude-default`)
and **Antigravity** (`agy-default`) when Magician reports those profiles
selectable (introduced with Magician `0.6.1273`).

`0.0.801` adds Town Square **Agent chatter** (durable, default off) and
Floor `social-talk` walks to cooler/pantry/lounge for new public posts.

`0.0.800` renders Phase 7 no-script custom surfaces through
`AppCustomSurfaceHost.svelte` (`srcdoc` + empty sandbox). It does not
reuse `Iframe.svelte`. App review and VibeDev App options show
`io_kind` and when a lockable tool is not yet dispatchable (Magician
`0.6.1235` bind kernel), and review names inert workflows without
forcing a broader grant.

Product-facing defaults are imported through
`src/lib/presentationIdentity.ts`; its committed generated constants come from
the repository-wide `data/presentation_identity.json` manifest. Run `make
presentation-identity-codegen` at the repository root after changing that
manifest. Do not add a second handwritten product-name fallback in the UI.

The global error fallback includes Dots and Boxes and Tic-Tac-Toe. Their shared
game row consumes the frame's remaining block space, and each square board is
sized from the smaller available inline/block dimension so short viewports do
not push either game beyond its container. The compact turn-status band keeps
the board visually centered while still expanding for game-over controls. The
compact brand/status header and message spacing preserve more of short screens
for the playable area.

Town Square's Three.js world uses a deliberately flat pixel-art render path:
one-third internal resolution, hard upscaling, stepped toon materials, hard
shadows, and a bounded blue-hour night floor. It keeps orthographic geometry,
picking, HUD coordinates, and occluder fading unchanged while reducing fragment
work and preventing soft distance shading from becoming low-resolution mud.

The shared Today/Town Square `ResurfacingBand` renders information-complete
Worth a look rows from defensive old/rich payload mappers. It lazily resolves
safe detail and bounded original content, capability-gates one recommended
primary plus a keyboard More menu, and executes task/reminder/share/memory
operations through revision-bound idempotent dialogs. Ask Presto enters normal
thread chat with a candidate-only URL and stages the resolved safe brief as an
editable attachment; it does not copy raw original content or auto-send.
Progressive attention learning is additive on both Message Follow-ups and
Worth a look: typed Useful/Acknowledge/Dismiss outcomes can return an auditable
re-score receipt, each evaluated card can show its learned-versus-baseline rank
delta, and a shared health strip keeps the complete active-candidate total,
source-family mix, embedding coverage, and changed-rank count visible. When
semantic ranking is disabled the same evidence is labelled as a preview; the
UI does not imply that observe-mode ranks changed what the owner saw.
Calibrated actionability is progressive too: shadow scores and deterministic
explanations are explicitly labelled as previews, while only enforced mode is
described as active ordering. The shared health strip exposes semantic-feature
coverage, scored and Slice 1 fallback volume, and the model snapshot; card
audit titles retain explanation code, model version, and snapshot. Missing,
invalid, disabled, or partially rolled-out Slice 2 metadata never hides a
candidate or removes its existing actions.
The same strip now consumes the server-authored semantic extraction health
contract for the full active scoped universe, not merely the current page. It
shows exact Follow-up and Worth a look state partitions (succeeded, missing,
invalid, pending, in-flight, retry, and dead), compatible-revision coverage,
extractor/prompt contract identity, and the backfill queue, pause, degradation,
and checkpoint state. Malformed health fails closed as a diagnostic only:
cards remain present in Slice 1 fallback order. Extraction and backfill stay
server-side; the browser does not invoke an extractor or render-time LLM and
does not use these diagnostics to filter or reorder candidates.
Evidence-preserving grouping is similarly additive. Shadow mode shows cluster
diagnostics while leaving every current card separate; only an enforced,
snapshotted, exactly reconciled response may present one representative with an
“N related updates” affordance. Expansion fetches and reconciles every member,
then exposes each member's source, route, rank, revision, model evidence, and
original lifecycle controls. “Duplicate of this” and “Not duplicate” submit an
exact revision-bound pair label and reload the affected grouping generation;
ordinary dismissal, acknowledgement, and missing interaction never create pair
evidence. System health retains candidate/member/cluster/representative totals,
collapsed-member volume, pair/cannot-link counts, and ungrouped fallback volume.
If the complete eligible universe requires more pair evaluations than the
configured finite budget, grouping stays inactive, every original row remains
visible as a singleton, and health shows required-versus-budget pair counts.
Verified impressions are visibility events, never list-fetch proxies: a shared
Follow-up/Worth tracker waits for the server-provided continuous dwell interval,
cancels unfinished dwell on visibility loss, and retries the same identifier-only
event without sending card text. Receipts are retained as quiet DOM diagnostics
rather than user notifications. Lane-routing shadow disagreements are labelled
as previews; a canary route is labelled active only when the returned decision
item says it was applied. Cards retain baseline, learned, and served routes,
while system health preserves evaluated/selected/returned totals, decision-item
and verified-impression coverage, deduplication/degradation diagnostics, and a
canonical All candidates link for every non-surfaced item.
Personal contextual-bandit metadata is also additive and server-authored. Each
selected card can expose its policy/model snapshot, posterior version and
uncertainty, proposed and served positions, served propensity, exploration,
support, and degradation state. Shadow is always preview-only; canary is called
active only on a card whose server decision says it was applied, and the browser
never samples or reorders. Explicit Follow-up and Worth a look outcomes attach
the exact decision/candidate/revision plus a matching durable verified
impression id when available; action recording still succeeds with
decision-only or missing attribution, and acknowledge/no interaction remain
neutral. Quiet receipts report posterior update/degradation, version,
uncertainty, and affected ranks before the list reloads. Health keeps existing
candidate totals alongside propensity coverage, exploration rate, posterior
update count, snapshot/support/degradation, and the first-page-only scope.
`/observe/stats` also has an independently loaded Worth a look rollout block
for V2/legacy coverage, backfill pressure, active routing repair,
recommendation conversion, and contextual-action errors.

Live Pause, Resume, Steer, and Stop controls mutate only a task's
`active_root_execution_id`; latest and completed execution ids remain available
for inspection only. Shared controls use authoritative backend capabilities,
coordinate concurrent actions by execution id, reconcile after success or
failure, and retain an explicit retry state when refresh fails. Task Abort
cancels the active root before resetting task status.

Settings → Mobile devices creates distinct iPhone and Android five-minute
connection QRs. The request names only the client kind: the server supplies its
deployment-owned public origin, binds the one-time capability to the current
scope, and renders the QR locally. The panel never derives a phone endpoint
from `window.location` and never receives the durable device token or
Cloudflare secret. It polls the authoritative roster to show completion,
platform, capability grants, last seen, and immediate revocation.

## Routes

- `/` landing page
- `/chat` full rich-chat surface with session history, attachments, and grouped task progress;
  session links are ID-addressed so active siblings and archived transcripts open
  exactly across thread-route navigation
- Chat task panels resolve internal tasks directly by id for Run, Stop, and
  Reset actions; internal tasks remain hidden from the ordinary Tasks list,
  while terminal history reconciliation prevents stale running/Steps spinners.
- `/today` Today: daily operating surface for Needs You, Delivered, Changed,
  Active Work, Follow-ups, and secondary Activity. `/home` and `/desk` redirect
  here for compatibility.
- `/tasks` structured task management surface. Its controller and body live in
  `src/lib/magician/tasks/TasksWorkspace.svelte`; the route uses URL-backed
  filter/selection state while the Town Square overlay reuses the same workspace
  with local navigation state so `/square` URL state remains untouched. Every
  task-opening surface uses the capability-driven unified panel; paused and
  archived are distinct verdicts, embedded drawers retain last-good data while
  polling, and Run renders the newest 200 timeline rows with the full count.
- `/dev/structured-response` fixture-only route for isolated structured-response
  renderer review.
- `/crew`, `/approvals`, `/budget`, `/briefing`, `/history`, `/memory`, `/runtime/resources`, `/triggers`, `/api-mining`, `/skills`, `/settings`, `/channels`, `/debug`, `/about`
- `/crew/[id]` includes a compact Live execution view directly below the route header, backed by `muijStore` (`agent.ui.delta` snapshots/deltas), showing the agent-emitted runtime document outside tests
- `/feed` operator timeline showing every agent's activity (cycles, tasks, approvals, goals, circuits, delegations, artifacts, tier consolidations, feed stalls, agent lifecycle, memory/feedback, published surfaces) with kind/agent/thread filters, hide-resolved toggle, and consecutive-same-kind collapsing
- `/debug/update-cards` fixture gallery rendering every `AgentUpdateKind` variant — useful for visual QA of card designs
- `/contextual-assist` native-only WebView content for the Tauri Contextual
  Assist overlay. It renders the passive `M` chip, compact action menu,
  personality selector, suppression states, and inert action buttons; normal
  browser loads intentionally show a native-only placeholder.
- `/t/[name]` thread surface with pinned thread tasks, thread-scoped chat, compose bar, and execution-panel entry
- `/observe` is a four-pane console. **Now** holds upcoming meetings, recent captures, and one open start form at a time (Listen, Join as the agent, or Watch screen). **Sources** holds continuous sources, Mail & chat, calendar, browser tabs, and startup catch-up. **Audio** holds the Meeting and Listening profiles, including stage overrides. **Notes** holds published task notes. Live captures sit above the panes. Watch still chooses deep read and optional system/mic audio; active sessions show audio-source, STT, and transcript chips. `?pane=` selects a pane. See `docs/components/magician/screen-capture-and-ask.md` and `realtime-media-rails.md`.

## Local Development

```bash
cd ui/unified-ui
npm ci
npm run dev
```

Default dev URL: `http://localhost:5173`

## Backend Expectations

`vite.config.ts` proxies frontend API calls to Magician (default `http://localhost:3002`).

Primary APIs used:

- `/api/magician/v2/*`
- `/health`

## Build and Checks

```bash
npm run check
npm run build
```

## Testing

Run the complete Vitest suite from `ui/unified-ui`:

```bash
npm test
```

From the repository root, `make test-ui` runs the same unit and mounted
component projects. The root `make test` and `make test-verbose` flows include
the UI suite alongside Rust and host-native tests. Both `make test-ui` and
`make test-ui-verbose` collect V8 coverage, retain timestamped raw artifacts in
`coverage/frontend/results/`, and write a stable self-contained dashboard to
`coverage/frontend/latest.html`. The command prints a clickable local link
after successful and failed runs while preserving Vitest's exit status. Set
`UI_TEST_REPORT_DIR` to move the report root. `make setup-ui-deps` installs and
verifies the locked frontend packages, including Vitest and its V8 coverage
provider; the broader `make setup-all` flow invokes that target automatically.
Test targets only run a read-only dependency preflight and never install or
change the lockfile.

Tasks and Chat have focused unit coverage around their stable stores and pure
projection helpers. Keep tests beside their source as `*.test.ts`; prefer pure
helper tests for normalization, filtering, lifecycle, parsing, and rendering
contracts, then add component tests where DOM interaction is the behavior under
test. `vitest.unit.config.ts` runs `*.test.ts` in Node;
`vitest.component.config.ts` runs `*.component.test.ts` in JSDOM with Svelte
Testing Library, and the root `vitest.config.ts` composes both projects.

The mounted suite covers task creation/filtering, persistent task state flows,
shared server pagination, Chat and VibeDev composers, compact Attention
list/direct/reask behavior, Today Pulse, Execution, Deep Work, realtime voice,
settings persistence, and polling lifecycle. Typed API/store tests cover
Observe, VibeDev projects, internal tasks, impact reviews, and Magician config.
As of Unified UI `0.0.677`, the complete suite contains 1,029 tests across 111
files. The V8 report for the source set loaded by that run measures 42.70% lines,
39.70% statements, 40.69% functions, and 33.18% branches. See
`docs/components/unified-ui/testing.md` for commands, reports, naming, and
test-boundary guidance.

Structured-response V1 is the default renderer for generic chat message kinds. The remaining task-status and domain-specific expansion work is tracked in the active domain migration roadmap; the completed implementation record is archived.
## Notes

- Native app surfacing lives in `src/lib/apps/appWidgets.ts`. It accepts only
  the bounded schema-v1 DTOs from the app widget runtime: a page resolves its
  ordered page-qualified slots with one bounded server batch, then
  `AppSlotRegion.svelte` renders all resolved declarations through one
  ETag-aware batch. Empty slots expose a native picker backed by strict,
  cursor-bounded settings pages across the complete bounded 512-widget
  inventory. Cursor composition retains one inventory digest, and assignments
  echo the exact picker package binding. Assignment and opt-out writes carry the
  settings revision, write fence, and one retry-stable mutation id. Today owns
  logical `/` regions `primary` and `secondary`; Observe owns `/observe`
  region `reviews`; canonical `/apps/<installation>/<surface>` pages expose a
  contextual region only after the canonical surface resolves. Surface paths
  containing dynamic/noncanonical slot
  characters are deliberately omitted instead of being normalized.
  `AppIndicatorRegion.svelte` reads only the server's materialized,
  unexpired projections for TopBar. Cached widget and indicator bodies are
  bearer-revision fenced; widget bodies are additionally package-identity
  fenced. A `304` renews every retained item's deadline; a missing or
  already-passed deadline waits a 2 s floor instead of failing. The last good
  render stays on screen through revalidation and transient errors for up to
  five minutes past its deadline (stale-while-revalidate), the first render
  shows a quiet loading tile rather than the unavailable placeholder, and a
  workspace-default slot the server hid (other than `disabled` /
  `update_pending`) reads as an empty slot. Revalidation runs only while
  the document is visible. These components execute no package JS,
  do not mount mini-frames, and widget buttons can launch only a governed
  action with exact `{}` input plus the rendered installation generation and
  package revision as stale-launch preconditions.
- Apps directory Review / Approve and enable is the owner surface for a
  `ready_for_review` installation (`src/lib/apps/installationReview.ts`).
  VibeDev App options (`src/lib/shell/vibe/AppOptionsPanel.svelte`) inserts
  the same authoring catalog names used by `magician app tools|agents|
  personalities|procedure list`.
- This UI no longer includes MagicTunnel dashboard routes.
- Legacy `magictunnel` library references were removed from active runtime paths.
- MUIJ is the Magician surface JSON format. Keep it for generated, published, replayable, and live-agent surfaces; use native Svelte for stable hand-authored product routes unless the route needs MUIJ serialization or agent composition. See `docs/components/unified-ui/muij-native-ui-boundaries.md`.
- Presto summary surfaces remain GAUI-driven where they are generated/published/replayable, but stable product-page chrome should migrate toward native Svelte components.
- SQL-backed dashboard widgets use the shared live data source helper. `llm_calls_sql` and `memory_events_sql` requests are coalesced into short-window batch endpoints (`/analytics/llm_calls/query_batch` and `/analytics/memory_events/query_batch`) so pages such as `/llm` and `/memory` avoid repeated DuckDB view setup during initial load; the helper falls back to single-query endpoints when a running backend has not picked up the batch route yet. Generative chart components use `chartUtil.ts` to intercept and clean raw DuckDB-Wasm object outputs like `HugeInt(...)` for safe UI rendering.
- `/vault` is the localhost secret-vault management surface. It uses `/api/magician/v2/secrets/*` for provisioned secret CRUD, pending approval resolution, and setup-token acknowledgement / rotation.
- `/channels` is the consumer-channel bot management surface. It uses `/api/magician/v2/bots/*` for runtime controls, QR pairing, logs, and scoped bot-config editing. Bot log severity is source-owned: routine reconnect/auth/listening notices should stay non-error because the bot/sdk producers now keep them on stdout/info, while genuine failures remain stderr/error.
- Bot working directories and editable capability-pack files are now V3-rooted. The bot editor points operators at scoped paths like `{scope_capabilities_root}/bots/<bot>`, and capability packs are skill-authored: scope edits live under `magician_data_v3/scopes/<principal>/<workspace>/skills/<skill>/tool_schema.yaml`, otherwise the embedded compiled pack defs apply (the legacy `<scope>/capabilities/packs/<name>.yaml` fallback was removed). Hot reload was retired — restart magician to pick up edits to `tool_schema.yaml` (the legacy `POST /api/magician/v2/capabilities/reload` endpoint was removed in v0.6.464).
- `src/lib/stores/chatStore.ts` now treats `channel` + `channel_address` as the authoritative chat identity for enrolled clients and revalidates cached enrollment before using a stored principal.
- `src/lib/stores/chatStore.ts` now also treats the backend `messages[]` batch as the authoritative continuation projection for sync sends, streaming sends, and confirm-tool continuations. Attachment uploads are staged into session-local chat output storage, interrupted streams reconcile from server state instead of blind resend, and chat-only task-progress messages are projected into one task panel per task wave with per-execution run cards while raw messages remain unchanged underneath.
- `/chat` and `/t/[name]` now treat escalation `request_id` as the switch between backend responder paths. Service-backed tool-authorization / sandbox-override cards post to `/api/magician/v2/user-requests/{request_id}/respond` using the installed workspace-bound bearer, while native execution-owned pauses still use `/executions/{execution_id}/execution/agentic-resume` or `/agentic-continue`.
- `src/lib/magician/components/chat/ChatMarkdown.svelte` is now the shared safe Markdown wrapper for web chat text. `/chat`, `/t/[name]`, and the bubble use it for freeform user/assistant/system text plus rich-result summaries, so headings, lists, links, emphasis, and inline code render consistently without a second Markdown implementation.
- `ChatMarkdown.svelte` accepts an optional `sessionId: string | null` prop (unified-ui v0.0.305). When set, the rendered subtree is scanned via `MutationObserver` for inline `<code>` elements whose content matches a conservative filesystem-path regex (`/^(?:\/[^\s]+|~\/[^\s]+|[a-zA-Z0-9_.\-]+(?:\/[^\s]+)+\.[a-zA-Z0-9]+)$/`). Each match gets two trailing icon buttons rendered via `document.createElementNS` (no `innerHTML`, no XSS): folder-reveal posts the path to `/api/magician/v2/chat/sessions/{id}/outputs/open-folder`; file-open posts to the new `/outputs/open-file` route (backend v0.6.507). Both `/chat` and `/t/[name]` wire `sessionId={message.session_id}` on every `<ChatMarkdown>` site — assistant text, system text, tool-call summaries, rich-tool-result summaries, pack-progress summaries, task-status summaries, escalation question.
- `src/lib/magician/components/chat/ChatContentBlocks.svelte` now shows the full saved output path when available and exposes both `Open file` and `Open folder`. The open-folder path resolves through `/api/magician/v2/chat/sessions/{id}/outputs/open-folder`, which accepts scoped output roots and persisted session-referenced external output paths but still rejects arbitrary paths.
- `ChatContentBlocks.svelte` (unified-ui v0.0.306) defensively strips a leading `outputs/` segment from `relative_path` before constructing the v3 task-output URL. Legacy chat messages saved by pre-v0.6.508 backends stored `relative_path = "outputs/<file>"`, which produced a `/tasks/{id}/outputs/outputs/<file>` 404 when the route already serves from the task's `outputs/` directory. The strip lets historical chat history render correctly without a data migration alongside fresh writes (which carry the bare filename).
- `ChatContentBlocks.svelte` pretty-prints `application/json` inline previews: parse with `JSON.parse` + re-stringify with 2-space indent into a `<pre class="chat-rich-text-preview-json">` with `white-space: pre`, horizontal scroll for long values, monospace, `tab-size: 2`. Falls back to the raw content on parse failure so partial/malformed streams still surface. Replaces the prior approach of wrapping minified JSON inside a markdown code fence (rendered as one unreadable horizontal wall).
- Inline file previews are now collapsible (unified-ui v0.0.307). The ready state renders a "Hide preview" toggle button beneath the body that clears the per-preview state back to `idle` so the block returns to its header-only form. Re-clicking "Preview inline" refetches.
- `src/lib/realtime/event-taxonomy.ts` SOURCE_HASH refreshed alongside the chat-side changes (no taxonomy-row behavior change; hash drift only).
- The generated realtime taxonomy binding was re-pinned for Magician `0.6.1162`; only its source hash changed, with no frontend event-category or severity contract change.
- `/chat` and `/t/[name]` paginate older history via a drain-on-top loader (unified-ui v0.0.306). `loadActiveSession` seeds `hasMoreMessages = rawMessageCount >= 200` from the pre-coalesce response count (since `normalizeMessages` collapses `pack_progress` runs, the post-coalesce count under-reports). The `scroll` handler enters a `drainOlderMessages()` while-loop once `scrollTop < 80`, chaining `loadOlderMessages` until the backend returns `has_more: false` — we can't keep re-checking `scrollTop` per iteration because browser scroll-anchoring repositions the viewport out of the trigger zone after each prepend. The auto-scroll-to-bottom reactive block now tracks `lastTailMessageId` and only fires when the *tail* message changes (a real new send/receive), not on every length change (which prepends triggered).
- `/chat` and `/t/[name]` `EscalationResolved` cards now fold the deliverable `<ChatContentBlocks>` output-files panel inside the same green summary box (unified-ui v0.0.306). Previously they rendered as side-by-side flex siblings inside `chat-action-card-wrap` (summary-left / file-right), reading as two unrelated UI elements. `chat-escalation-resolved-card` is now a flex-column container with `max-width: 720px` holding both the summary row and a `chat-escalation-resolved-files` sub-section divided by a subtle accent-tinted border.
- The shipped `generate_image` and `generate_video` chat flows now cover both Gemini image and Veo video generation through one tool each. The chat model chooses `quality_tier` (`fast`, `balanced`, `pro` for images; `fast`, `balanced` for video) from the backend-exposed tool description and parameter cues, `auto` is a neutral fallback to `balanced`, and there are no dedicated tier pickers in the UI yet. Both backend paths still support longer-running media jobs: each pack watchdog allows up to 20 minutes, each Python helper sets a slightly shorter internal Google GenAI timeout, and backend logs distinguish provider wait time from local save/download time when a job appears slow or stuck. Veo setup is backend-owned and uses the same `make setup-capability-tools` + direct Gemini API key path documented in the core Magician docs; the UI does not expose a separate Veo setup workflow.
- History Drawer thread rows now support destructive deletion for non-`#general` threads (unified-ui v0.0.403 / magician v0.6.591). The UI asks for themed confirmation, calls `DELETE /api/magician/v2/ui-threads/{id}`, refreshes chat sessions after success, and redirects from `/t/<deleted-thread>` to `/t/general`. The `#general` thread never shows the delete affordance.
- `src/lib/stores/chatProfileStore.ts` now only exposes backend-approved rich-chat profiles in the `/chat` and `/t/[name]` composers. OpenAI profiles pinned to `openai_api_mode: chat` are hidden because they cannot safely replay rich multimodal chat state, and the UI renders backend warning strings inline when such profiles exist in config.
- The seeded `anonymous/default` personal-assistant runtime definition now ships with `onboarding_completed: true`, so the default materialized scope does not reopen the welcome modal unless operators explicitly reset onboarding state.
- `src/lib/types/surfaces.ts` now carries both the older durable-surface manifest metadata and the V3 published-surface render contract used by Today and `/briefing`.
- `src/lib/magician/presto/surfaces/publishedSurfaces.ts` now reads V3 published-surface projections and per-surface render contracts from `/api/magician/v3/published-surfaces/*`. The active path no longer reads older durable manifest or layout APIs; durable MUIJ documents now arrive through the same V3 render contract, realtime refresh keys off the scoped V3 `published_surface.changed` websocket event, and the old `surface.published` refresh path is no longer active.
- `src/lib/attention/AttentionCenter.svelte` is the global urgency surface. It is fed through `src/lib/stores/attentionStore.ts` by `/api/magician/v2/feed/attention` and live feed deltas, not by legacy task/approval aggregation, and it now includes both V3-backed `input.requested` items and scoped `user_request.pending` escalation items. Service-backed requests respond through `/api/magician/v2/user-requests/{request_id}/respond`; typed execution pauses still use `agentic-resume`.
- `src/routes/(app)/settings/+page.svelte` now exposes a backend-owned `Reload magician config` action alongside the trust-policy editor. The button posts to `/api/magician/v2/settings/magician-config/reload`, surfaces the returned `live_reloaded`, `restart_required`, and `warnings` fields, and is intended for operator reloads of `magician/magician-config.yaml` after LLM-routing or policy edits without restarting the `magician` process. The live path currently refreshes the operation router, multi-LLM chat service, native-tool-calling policy, tool authorization, and complexity routing; other config sections are deliberately reported as restart-only. The same page hosts **On-device generation** (`LocalGenerationPanel`): switch `runtime.ollama.local_generation.selected` among the kitty models, warn when the RAM-tier rule is violated, still allow the switch, and reload Ollama.
- `src/lib/stores/chatStore.ts::clearMessages(sessionId)` issues a single `DELETE /api/magician/v2/chat/sessions/{id}/messages` (no message_id) to clear every message in a session while preserving session identity. `/chat` and `/t/[name]` expose this via a small "Clear chat" pill above the messages area, hidden when read-only/archived and disabled while the request is in-flight. Walks all message segments server-side, so it removes paginated messages that aren't in the local cache.
- `src/routes/(app)/ExecutionPanel.svelte` now caps the panel header at `max-height: 35vh` with internal scroll and clamps the task title `<h2>` to two lines (`-webkit-line-clamp: 2; word-break: break-word`). Long task titles previously grew unbounded and pushed the tabs nav below the viewport with no way to reach it; the title remains discoverable in full via the `title=` tooltip on hover.
- The execution panel now also surfaces both execution-scoped direct pause requests from the shared agentic pause store and scoped `user_request.pending` items. Service-backed tool/sandbox escalation responses go through `/api/magician/v2/user-requests/{request_id}/respond`, direct pause cards still submit through `/api/magician/v2/executions/{execution_id}/execution/agentic-resume`, and classic ask-loop clarifications still use `/clarify/{question_id}/respond`.
- Those task/execution surfaces now also depend on canonical backend status persistence after resume/reset flows: task-backed `agentic-resume`, continuation, and direct status updates write back the resulting V3 projection immediately, so `PlanningComplete` returns to `ready`, paused runs do not stay falsely active, and the panel does not need to guess around stale local state.
- The attention bar’s grouped `input.requested` cards now receive the full typed pause schema from the V3 feed metadata (`input_schema` plus parsed options/hints), so UI responders can honor `allow_other`, min/max selection bounds, file filters, external-action instructions, and other typed pause constraints instead of collapsing everything down to free-text prompts.
- Password-style `input.requested` cards now use a masked responder path in the UI instead of plaintext `window.prompt`; the submitted value still goes through the backend password `UserInputValue` resume path so existing redaction and ephemeral-secret handling remain intact.
- `src/lib/stores/feedStore.ts` is the shared Activity/feed client store. It hydrates from `/api/magician/v2/feed`, loads `/api/magician/v2/feed/counts`, applies `FeedItemCreated/Updated/Removed` websocket deltas in place, and falls back to a scoped refresh only when a relevant delta cannot be applied locally.
- `src/lib/shell/DiffStrip.svelte` supports opt-in syntax-highlighted diffs through Shiki. `src/lib/shell/diffSyntaxHighlight.ts` loads the Shiki core/theme bundle only when highlighting is enabled and then loads per-language grammar chunks only for expanded files, preserving broad coder-language support without preloading every grammar.
- `src/lib/stores/taskStore.ts` now refreshes task lists from both scoped `TaskCreated` / `TaskUpdated` / `TaskDeleted` lifecycle deltas and task-scoped `FeedItem*` deltas. Feed task CRUD cards are projected directly by the backend task API; the feed transport stream remains for live delivery of the resulting `FeedItem*` updates and supplemental non-task cards.
- `src/lib/magician/tasks/TasksWorkspace.svelte` is the canonical Tasks application controller and body for both `/tasks` and the Town Square Tasks overlay. Route mode owns `/tasks?filter=&tag=&selected=` synchronization; local mode keeps those interactions in memory and offers an explicit full-page link instead of mutating the host route.
- Task reset actions on `/tasks`, `/today`, and `/t/[name]/tasks` persist directly through `/api/magician/v3/tasks/{id}/status` with `status: "ready"` and use the backend-returned status for the final toast. Do not re-route reset through V2 execution status: plan metadata such as `plan_status: failed` can remain historical while the task state is ready for re-run.
- `src/routes/(app)/today/+page.svelte` is the canonical Today route and owns
  the current Today implementation while `/home` and `/desk` remain redirect
  aliases.
- `src/routes/(app)/TaskList.svelte` now uses GAUI primitives for batch actions, status filters, agent filters, the task compose form, and the empty-state CTA, and `src/routes/(app)/TaskItem.svelte` now uses GAUI primitives for task-row actions, tag editing, date/priority pickers, and the context menu, so the structured `/tasks` surface stays on the same shared theming contract as Today and Thread. The inline `/tasks` compose surface also exposes the V3 task `output_mode` contract, letting a user choose `accumulate` vs `overwrite` when creating a task.
- `src/lib/magician/components/PublishedScrollCanvas.svelte` is the shared recurring-scroll dashboard canvas used by both Today’s full-screen expand modal and the standalone `/briefing` route. It now renders both durable MUIJ surfaces and direct `task + user` output publications (`markdown`, `html`, `json`, `plain_text`, `xml`) entirely through the V3 render contract, while still persisting client-local widget order/size/hidden preferences scoped by `(principal, workspace, route_target, task_id, agent_id)`.
- `src/routes/(app)/t/[name]/` is now the real MAGICAN thread surface (`+layout.svelte` shell; bare `/t/<id>` redirects to the `chat/`, `tasks/`, or `settings/` sub-route). It uses thread-scoped tasks plus thread-scoped chat sessions rather than the earlier placeholder stub, its editor/task/chat controls are built from shared GAUI primitives, and `selected_item` navigation can focus assistant message cards in-context.
- The legacy left-rail `src/routes/(app)/TodoSidebar.svelte` was removed when the v5 ("modern") shell became the only shell (v0.0.419). Its persisted server-scoped UI Thread records — inline thread-scope creation and backend-persisted archive/reorder state — now live behind the v5 nav surfaces: `src/lib/shell/HistoryDrawer.svelte` (threads/sessions), `src/lib/shell/TopBar.svelte` (primary nav), and `src/lib/shell/CommandPalette.svelte` (control-panel actions).
- `src/routes/(app)/ExecutionPanel.svelte` now follows the redesigned `Run / Output / Debug` contract backed by `ExecutionPanelState` + `ExecutionPanelDelta`. It no longer does raw legacy execution-event stitching in the client, sends explicit `(principal, workspace)` scope on panel state loads and clarification submissions, and its run-surface controls, header actions, tab navigation, and debug observation cards now use GAUI primitives instead of bespoke raw controls.
- The execution panel refreshes from `ExecutionPanelState` / `ExecutionPanelDelta`, canonical execution facts, and PlanGraph lifecycle events. The retired `TaskPlanUpdated` markdown refresh event is no longer part of the realtime contract.
- The execution panel Plan tab includes a compact planning activity stream backed by task-scoped `V3PlanningProgress` events from `/api/magician/v3/events`. It intentionally filters by task id rather than selected execution id so preplanning stages remain visible before a normal run execution exists.
- The execution panel’s plan tooling now lives inside `Debug > Plan Inspector` via `src/routes/(app)/ExecutionPlanInspector.svelte`. That integrated surface uses graph view, waterfall view, inline edit/save, and version history/restore against the PlanGraph APIs.
- `/debug` is now linked from the sidebar Control Panel so archived-task inspection and developer diagnostics are discoverable without typing the route manually.
- `/debug/tutor-cursive` is the focused Personal Tutor cursive review page. It previews the selected Playwrite USA Traditional primary font and macOS Brush Script fallback for `cursive_text`, including the `r`/`wr` samples that originally exposed the bad cursive shape.
- `/debug` now carries the canonical Canvas Mode SOTA fixture group (`25`-`31`) for diagram, hybrid connector, whiteboard, map, chart, cross-origin, and noisy collaborative spatial workflows. Canvas/spatial fixtures initialize their visual targets on page load; reset controls must keep the ids used by the page-owned handlers so initial canvas/SVG/iframe rendering cannot be skipped by a missing-element script error.
- `/debug?mode=sota-tests` also carries the `Live Concept Tutor` fixture group (`36`-`38`) for deterministic math, physics, and computer-science tutoring overlays. These fixtures are static browser pages with hidden `tutor-ground-truth` metadata; their debug goals open the page first and then run the tutor with the full agent tool set instead of the browser-only env restriction.
- `/debug?mode=sota-tests` also carries the `Desktop App Tutor` live-app group for Notes, Calculator, TextEdit, and Apple Music onboarding/help flows. These cards do not open browser fixtures; their goals launch real macOS apps and run with the full tutor tool set so `screen-draw`, tutor run state, and delegated `mac-operator` actions can exercise the observe/draw/act/verify loop.
- `/debug` also includes a paginated archived-task debug inspector for ephemeral direct-run shells. It reads `/api/magician/v3/tasks/archived`, `/api/magician/v3/tasks/archived/stats`, and the existing execution responsibility endpoint so operators can inspect retained terminal shells, cleanup eligibility, and execution lineage without restoring those tasks into the main Presto task list.
- `longhand` is the default theme, `longhand-dark` is its dark companion, and the retro pairs remain available as alternate styles.
- The dark alternate themes `arcane-terminal` and `midnight-ocean` now define sidebar shell tokens explicitly, so the left navigation stays dark-themed instead of inheriting lighter fallback surfaces.
- Task-plan preplanning affordances are now part of the normal task-surface logic. There is no longer a dedicated feature-disabled shim for this flow.
- `/reviews` and `/evidence` are the work-evidence-graph surfaces. `/reviews` generates grounded impact reviews (window slider + facet, grounding verdict, stale + regenerate) and hosts a **Dashboard** tab (metric tiles, coverage/top-entity/weekly tables, visibility gaps, and **Publish to /briefing** which pushes the dashboard through the artifact-driven surface machinery). Reviews-page requests use the active workspace-bound bearer, empty review windows render an explicit no-evidence notice, and dashboard top-entity rows are backed by the server-side same-name/same-type entity roll-up so split anchors do not crash or duplicate the summary. `/evidence` is the trust inbox (Evidence + Entities tabs: suppress/delete/re-facet, rename/merge/split). Both consume `/api/magician/v2/evidence/*` and are reachable from the command palette.
- The VibeDev cockpit (`src/lib/shell/vibe/VibeStudio.svelte` + `conversation/submit.ts` + `stores/vibeCheckpointsStore.ts` + `stores/taskStore.ts`) treats a "run" as a **chain** of turns (root run + threaded follow-up turns, resolved via `buildVibeRunChainIds`), v0.0.547. Composer follow-ups thread onto the active run (tag `vibedev-threaded`, folded into the chain root; a "Starting the next step…" footer until the new turn's first card lands) while the follow-up-context **Run** button still opens a separate run. The rail's **Checkpoints** list spans the whole chain — `fetchVibeCheckpointsForChain` unions every turn's `GET /vibedev/runs/{task}/checkpoints` (deduped by id), so an earlier turn's rewind point is no longer hidden once a follow-up turn mints its own. A coding run that settles to `completed` auto-runs one project check (so its checkpoint appears without a manual Tests click; plan/Discuss runs skipped), and a jittered ~15–20s backstop poll re-fetches the task list while any run is in-flight so a missed realtime event can't strand a finished run at "running". `submitCodingRun` makes create+execute atomic (a thrown `executeTask` unwinds the project pointer + hard-deletes the just-created task), and a selected non-terminal run with no live execution shows a soft **✕ Dismiss** (marks it `cancelled`, removes no files; hard delete stays the rail row ✕) with honest "Prepared — never started" copy instead of a dead Stop + disabled Cancel.
- VibeDev Project settings now include a Deploy panel for the Cloudflare Pages static-publish target. It uses `vibeDevProjectStore.loadDeploySettings/saveDeploySettings/checkDeploySettings` against `/api/magician/v2/vibedev/deploy/settings`, never renders saved token values, shows the active runtime env/config paths returned by the backend, and saves/tests credentials through the Magician process rather than WebView-local filesystem access.

## Plan 1.6 scripted custom-surface host (2026-08-26)

`AppScriptedSurfaceHost.svelte` + `appScriptedSurface.ts` host a package's
declared scripted entry document in a `sandbox="allow-scripts"` iframe
(never `allow-same-origin`; opaque origin). The host page relays the
frame's `postMessage` bridge to the authenticated
`custom-surface-v1/bridge` endpoint, keying each session to the exact
frame object it created (two sandboxed frames share origin `null`; the
frame is the discriminator), enforcing the 32-message budget and the
3-strike reload budget, and refusing any non-relative or dev-origin
frame source (the desktop/Tauri constraint; the forbidden host origins
are matched by name before the relative-path requirement). Failed
surfaces are replaced by the closed "failed safely" notice; unsupported
clients get the closed unsupported notice. `AppSurfacePage.svelte`
attempts the scripted host at the installation root and at every
declared surface route (`?route=/<path>` selects the entry point) before
the Phase 7 no-script host, and on navigation between installations
resets the scripted plan and remounts the host keyed on the plan's
`session_ref`, so budgets, sequence state, and torn-down state can never
carry across plans. Packages without the declaration fall through to the
unchanged Phase 7 no-script host.

- 2026-08-28 (review): `parseScriptedSurfaceHostPlan` validates `session_ref`
  with `isAppReference` (matching the iOS host's `isQueryableSessionReference`).

- 2026-08-28 (Phase 5): the monitors specForm comment points at the canonical
  monitors/ module home (was artifact_v2/).

- 2026-08-28 (Phase 5 cross-batch review): the vibe conversation submit comment
  points at the canonical vibedev/run_service path.

- 2026-08-29: `src/lib/realtime/event-taxonomy.ts` (generated from
  `magician-event-taxonomy` by `make event-taxonomy-codegen` — do not hand-edit)
  gained `'loop.outbox.gap': t('observability', 'warn', false)`.

  **What it means.** The agentic loop's event outbox buffers journalled events per
  run segment and evicts past a bound. Since the projector became the only path
  from a journalled event to a transport, an evicted record is a **permanently
  lost event** and nothing else would have said so. This marker leads the drain
  that lost records, so a hole is detectable rather than silent.

  **It is operator-facing, not user-facing, deliberately.** `user_relevant: false`
  keeps it off chat, the feed, webhooks and agent memory. The records it is *about*
  — `plan.step.*`, `tool.result.projected`, `reasoning.*` — are themselves
  operator-only and never reached those surfaces either, so a hole marker in a chat
  bubble would report a transport-buffer defect to somebody who never saw the
  records and cannot act on the loss.

  **What changed for filtering.** An *unregistered* name soft-passes every taxonomy
  filter: before the row existed this appeared under `severity=info` and
  `severity=error` simultaneously, and `taxonomyFor()` fell back to a catch-all
  `observability/info/false`. It is now classified, so `severity >= warn` finds holes.

  **A stale comment in `EventStreamCard.svelte` (~:593, ~:672) will mislead you here.**
  It claims ExecutionPanel's Activity card mounts with `defaultUserRelevant='true'`.
  No caller passes that prop, it defaults to `'any'`, and ExecutionPanel does not
  mount an `EventStreamCard` at all. Believing it makes `user_relevant: false` look
  like it hides this event from the operator panel — the reasoning that would push
  somebody to flip the flag and leak an internal diagnostic into a user's feed.

Service-health HITL notices appear in Attention and support scoped dismissal; provider errors and credentials are never copied into the notice.

- 2026-09-26: `src/lib/today/pulseQueries.ts` preserves `hourlyCalls` in `LlmPulse` from the `today_hour` section's `COUNT(*)` aggregate, enabling hourly call-count tooltips alongside hourly spend in the newspaper ledger.
- 2026-09-26: Promoted the Morning Edition (`src/lib/today/MorningEdition.svelte`) to the default `/today` route. `/square` features the Fleet Civilization game hero on top with live agent social (`TownSquareSocial`) docked directly below.
- 2026-09-26: Integrated the Realtime Wire mini feed (`src/lib/today/TodayRealtimeWire.svelte`) into Today's page. Unifies streaming events (`/events`), insights, and fleet activity (`/feed`) into an ultra-fast, single-row collapsed live ticker showing 24h event volume, cycling the latest incoming items, and expanding on click to filter tabs (`All`, `Events`, `Insights`, `Activity`) with deep links and the latest 5 dispatches.
- 2026-09-27: Retired the legacy Today surface and deleted the `/today?mode=legacy` route, establishing Morning Edition as the sole and permanent `/today` surface.
- 2026-09-27: Modernized the Crew Fleet (`/crew`) and single agent detail (`/crew/[id]`) pages with KPI ribbons, server-side pagination, 2-column overview dashboard, and strict viewport horizontal scroll containment.

LLM usage preserves unreported harness costs/cache buckets as unknown; partial cache reports do not produce a whole-run cache hit rate.

Decision Model telemetry: `/llm` Cost includes a separate Jev/Laya/Kev breakdown
(calls, success, USD, tokens, cache reporting coverage, average/p95 latency), and
Usage filters Decision Models with per-attempt cache columns. Shared spend views
and chat turn totals include these calls; unknown cache/pricing stays distinct
from free local inference, and tiny Jev charges retain sub-cent precision.
Today and `/llm` spend summaries retain harness cost estimates alongside model
calls while excluding duplicate chunk summaries.

Memory classification emits workspace-scoped `DecisionShadowAgreement` activity
rows and warning-level `DecisionAccountingGap` rows; the generated taxonomy
keeps these visible through the ordinary event surface and retention path.
