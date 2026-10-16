# Chat next (lively presentation layer)

On by default in every theme (`resolveChatNext` returns `true` when no URL or
stored value decides); `?chatNext=0` or a stored `off` returns the classic
surface.

`src/lib/magician/chat/next/` holds chat renderers built on the liveliness
tokens of the pastel themes (Sorbet, Sky, Candy and their darks), gated by the
`chatNext` flag that `ChatPanel.svelte` reads (see [Wiring](#wiring)).

## `chatNext` flag

`src/lib/magician/chat/next/chatNextFlag.ts` exports `chatNext` (a derived
store), `setChatNext(on)` and `resolveChatNext(...)`. First match wins:

1. URL `?chatNext=1|0`, read once at module init and persisted to storage.
2. `localStorage['magican-chat-next']` = `'on' | 'off'`.
3. Default: on, in every theme (all inherit the liveliness tokens' `:root`
   fallbacks); the flag is an escape hatch.

Unrecognised values fall through.

## Helpers

- `tint.ts`: `tintFor(key)` maps a key (host, artifact name, agent id) to a
  stable slot `1..5` (FNV-1a 32-bit). `tintVars(key)` returns a `style=` string
  setting `--cn-tint` / `--cn-tint-ink` to `var(--tint-N)` / `var(--tint-N-ink)`.
- `motion.ts`:
  - `readDuration('--dur-quick' | '--dur-base' | '--dur-slow')`: the token in ms
    from `<html>`; the app.css default when unreadable (SSR, jsdom).
  - `rise(node, { delay, y })`: rise, scale .98 → 1, fade over `--dur-base`
    (JS ease-out; Svelte `css` transitions cannot take a `linear()` token).
  - `revealWords(node, { enabled })`: **available, not wired**. Staggers a
    finished answer's words in threes (`span.cn-word`, `--dur-quick / 4`, max 40
    groups, skips `pre` / `code`). **Never on streaming text**: `enabled` is read
    at mount only (pass `enabled: false`).
  - `morph(source, () => target, update)`: `document.startViewTransition` with a
    unique `cn-morph-<n>` name (never `command-ask`); plain `await update()` when
    unsupported, reduced motion, or a transition is already starting.
- Reduced motion (`prefers-reduced-motion: reduce`): `--dur-*` are `0ms`,
  `readDuration` returns 0, `rise` returns `{ duration: 0 }`, `revealWords` is a
  no-op, `morph` skips the transition.
- **Washes are images, not colours.** Pastel `--wash-*` tokens (`--wash-card`,
  `--wash-hero`, …) are `linear-gradient`s: only `background` /
  `background-image`; inside `color-mix()` the declaration is dropped. Mix tints
  into `--bg-card` / `--bg-elevated`. `chat/next/colorMixGuard.test.ts` fails on
  a `color-mix()` referencing `--wash-` in `chat/next/**/*.svelte`.

## Block variants

`StructuredResponseRenderer.svelte` takes `variant: 'classic' | 'next'`
(default `'classic'`). Under `variant="next"` three kinds use
`src/lib/magician/chat/next/blocks/`:

| Block kind  | `next` component    | Shape |
|-------------|---------------------|-------|
| `sources`   | `SourcesStrip`      | Lettermark + host chips; avatar stack fans out on hover / focus-within (`--dur-base`, `--spring-snappy`). First 6, then a `+N` button (`aria-expanded`) expanding inline. |
| `metrics`   | `MetricsHero`       | First metric as hero (`--font-display`, unit, ↑ ↓ → with `aria-label`, direction only, no good/bad colour); rest as pills tinted by `tintVars(label)`. |
| `artifacts` | `ArtifactCard`      | Cards tinted by `tintVars(label)`; only cards joining a rendered grid rise (`in:rise`, 40 ms stagger, max 320 ms); a new message rises whole via the message entrance, a history load or session switch renders still. `image/*` → lazy thumbnail + lightbox (`role="dialog"`, Esc / backdrop closes, focus held on the close button, `morph` from the thumbnail); `video/*` → `<video controls preload="metadata">`; `audio/*` → `<audio controls>`; else a type glyph (PDF, table, doc, code, zip, file; mime, else extension), label, human size, mime. |

Every other kind (`text`, `markdown`, `callout`, `key_values`, `table`, `list`)
renders the classic block under both variants (the fallback guarantee).

Link safety matches classic: opening goes through the host's `onAction`
(`open_url` / `open_artifact`); source chips are real `<a>` links
(`target="_blank" rel="noopener noreferrer"`) only when the href passes
`isSafeHttpUrl`, else inert text; inline `<img>` / `<video>` / `<audio>` only
from a safe http(s) `href`. An `artifact_id`-only artifact gets a glyph card
with an Open button (the host's authenticated opener resolves it).

**Lettermarks, not favicons.** Avatars are the host's first letter (minus
`www.`) on `tintVars(host)`; the strip never fetches a favicon, since a
third-party service would leak every source the user reads.

## Turn progress

`ChatTurnProgress.svelte` takes `variant: 'classic' | 'next'` (default
`'classic'`); `next` swaps the rolling list for
`src/lib/magician/chat/next/TurnStage.svelte`, keeping row model, subscription
and settling (`chat-turn-progress.md`).

`TurnStage`:

- The last `running` row is a working chip (pulse from `tintVars(tool ?? label)`,
  glyph web / search / file / code / generic from the tool name, label, muted
  host or file name), crossfading with `rise`.
- Settled rows are dots (failed `--color-error`, waiting `--color-warning`, done
  the tint; `data-status`, `aria-label`), latest 5 only (`latestRows`,
  `STAGE_LATEST_LIMIT`); earlier fold into `+N` ("N earlier steps").
- `Show N steps` (`aria-expanded`) opens the latest 5, then `Show all N steps` /
  `Show latest only`; collapsing resets to the latest window.
- No running row: a `--radius-chip` label, `Waiting on your input` if a row is
  `waiting`, else `Finishing…`.
- A hidden `aria-live="polite"` region announces the current step only.

`src/lib/magician/chat/next/stageRows.ts`: `toolTarget(args)` (host of `url` /
`href` minus `www.`, else basename of `path` / `file_path` / `filename` /
`file`), `toStageStatus` (anything not `running` / `done` / `failed` /
`waiting`, e.g. `idle`, is `done`),
`toStageRows(rows, metaByKey, fallbackTarget?)`.

**Live chat turns.** `RequestActivityCard.svelte` takes the same `variant`; with
`next` **and** `live` its expanded row tail becomes `TurnStage` (header and
full-log modal unchanged). Rows are the card's leaves (same order and statuses);
`tool` / `target` come from a read-only pass over the turn's raw `tool.*` events
in `chatTurnEventsStore`, keyed like the tool leaves (`<agent>::tool::<call_id>`),
never feeding a leaf. No leaves yet → no stage (never a premature
`Finishing…`). Historical cards (`live` false, "What happened") stay classic.
Test: `RequestActivityCard.component.test.ts`.

## Task card order

Replies are stamped at turn end, task cards mid-turn, so `normalizeMessages`
sorts a `task_status_update` whose `chat_turn_id` matches an assistant reply no
earlier than that reply (ties to the reply); `collapseTaskProgressMessages` then
places the card at its latest update, still before the next user turn. The
sending tab resolves the reply's turn via the chat-turn side-map (resolver from
`ChatPanel`; same default in `finishAuthoritativeBatch`). Cards without a turn
id keep time order; streaming placeholders stay last.

## Live task card

A chat task is a live card that expands in place (wired in `ChatPanel.svelte`:
[Wiring](#wiring), [Chat-started tasks](#chat-started-tasks)); the old
`TaskProgressPanel` side panel was removed and **Inspect →** goes to the task
drawer.

**View-model.** `liveTask.ts`: `toLiveTaskView(message, taskExecutionGroups)` →
`LiveTaskView`, and `formatElapsed(ms)`, over `TaskStatusCard`'s inputs via the
shared `taskStatus.ts` helpers:

| Field | Source |
|-------|--------|
| `id` | `content.task_id`, else the message id |
| `title` | `labelOrShortIdTitle(display_label, task_id)` (`tasks/taskPanelTitle.ts`, drawer's `deriveTaskTitle`: routed `slug / …` → its goal, id-shaped labels ignored), else `Task · <short id>` (`task_2679cefd…` → `Task · 2679ce`), else `Task`; never the raw id or `summary` |
| `status` | `content.status` (the store's `ready`/`pending` wins over a lagging `planning`/`draft`, dropping the summary) → `queued` (`ready`, `pending`, `rejected`: no timer, Cancel or activity; `Not started` tag) / `running` / `waiting` (`paused`, `draft`, `waiting*`, `awaiting*`) / `completed` / `partial` / `failed` / `cancelled`; `synthesis_pending` stays `running`. `partial` = `completed` with `content.completion_kind === 'partial'` (backend `CompletionKind`, `TaskStatusUpdate.completion_kind`, `chat/models.rs`) or `partial_success` |
| `verb`, `terminal` | `taskStatusVisual`, `isTerminalTaskStatus`; partial reads `Partially completed`, terminal. `archived` is terminal (`Archived`, muted like `cancelled`, runs settle `done`), so a card whose last status update never arrived settles on the store's `archived` instead of reading `Running` forever (the iOS `LiveTaskCardModel` and `ChatViewModel` treat it the same) |
| `reason` | `taskStatusReason(content)`: trimmed markdown `content.summary` (live line, pause, failure or completion text) |
| `steps` | one per run group: `taskExecutionLabel` + `running` / `done` / `failed` / `waiting` |
| `currentStep` | non-terminal only: the verb while `synthesis_pending`; with a `reason`, the last running step's label only when there are several runs; without, that label else the verb |
| `startedAt` / `finishedAt` | min / max `created_at` over message and group updates (epoch ms; below 1e11 = seconds); `finishedAt` only when terminal |
| `outputs` | per run group, `getMessageContentBlocks(group.message.content)`; empty runs dropped; `label` only with several runs |
| `taskId` | the real `content.task_id` (absent on id fallback) |
| `executionId` | latest run group's `execution_id`, else the message's |

**`LiveTaskCard.svelte`** takes `TaskStatusCard`'s props unchanged (one-for-one
swap; `onOpenTask` also gets the card element, the morph source) plus optional
`onCancel(taskId)`. Root: `<div role="group">` named `<title>, <verb>`. Header:
status dot; title, **Inspect →**, **Cancel** (while running), chevron (when
expandable); verb, elapsed time (1 s tick while non-terminal, cleared on
destroy), **Needs you**;
current step (if not the verb); reason. Styled with `tintVars(taskId)`,
`--radius-md`, `--shadow-float`.

- **Toggle.** When expandable, an empty `<button>` (`aria-expanded`,
  `aria-controls`) at `position: absolute; inset: 0` covers the header; chips,
  reason links and **more** sit above it as siblings (`z-index: 1`; Cancel and
  popover `z-index: 2`), never nested. Name: `<title>, <verb> — show details`
  (never the elapsed time). Not expandable → no toggle, `aria-expanded` or
  chevron, and the header is not clickable.
- **Action chip.** **Inspect →** calls `onInspectFromIds` when the view has
  `taskId` and `executionId`, else `onOpenTask(message.content, cardEl)`. It sits
  right after the title (which ellipsizes), visible while collapsed, and uses
  the shared `.mg-chip-action` class (`src/app.css`: 0.72rem, weight 600,
  `--text-secondary` text, transparent, 1 px `--accent-secondary` /
  `--border-soft` mixed border, `--radius-chip`, padding 0.05rem 0.5rem; hover
  `--accent-secondary-soft` + `--text-primary`; `--ring-focus` on
  `:focus-visible`; colours over `--dur-quick`), as do the bubble's
  **Inspect run →** and every `ChatContentBlocks` file action; callers add
  placement only (`.cn-live-task__chip { flex: none }`,
  `.request-activity__inspect { margin-left: auto }`).
- **Reason.** Always visible, via `ChatMarkdown` (no `stripMarkdownPreview`),
  outside the toggle since a `<button>` cannot hold links. Clamped to three
  lines collapsed; when clipped (`scrollHeight > clientHeight`, re-measured by a
  `ResizeObserver`) a **more** button expands the card.
- **Needs you.** Shown when an ask is pending for the task (`pendingHitlTaskIds`,
  from asks `pendingHitlStore` parses) or a draft plan awaits approval
  (`awaitingApproval`); never on a terminal card or for a user pause alone; a
  running task blocked on a mid-run ask shows it.

**Status dot.** 10 px, `data-status`, `aria-hidden="true"`. No percentage:
chat tasks carry run groups, not plan steps (`done ÷ runs` would jump 0 % →
100 %); planned runs show [Plan-step line](#plan-step-line). Colours are the
app-wide `--status-*` tokens, defined for every theme in `app.css`.

| `data-status` | Dot |
|---------------|-----|
| `running` | `--status-running`, slow breath (scale 1 → 1.2 + glow, `--dur-slow` × 4, infinite) |
| `waiting` | `--status-paused` (amber), gentle pulse (`--dur-slow` × 3) |
| `completed` | `--status-completed`, solid |
| `partial` | `--color-warning` (amber), solid (unlike waiting, no pulse) |
| `failed` | `--status-failed`, solid |
| `cancelled` | `--text-muted`, solid |

Only a live transition into `completed` pops (`cn-live-task__dot--pop`,
`--spring-bouncy`, once). Reduced motion: every dot animation is `none`.

**Outputs in the body.** A terminal task's outputs show under the header
(`aria-label="Outputs"`) through `ChatContentBlocks` per run
([previews](#chatcontentblocks-previews)); collapsed, the first two
(`OUTPUT_PREVIEW_LIMIT`) plus **+N more** (expands the card); expanded, all.

**Expanded details.** Click or Enter / Space flips a per-card `expanded`
(several may be open); region `role="region"`, `aria-label="<title> details"`,
revealed over `--dur-base` / `--spring-gentle` (none under reduced motion). The
card is **expandable only when expanding reveals something** (details, a
clipped reason, outputs behind **+N more**; once open it stays a toggle so it
can close); Cancel never counts. Content:

1. **Live steps or runs.** Non-terminal with a real `taskId`: the named
   `activity` slot (props `taskId`, `executionId`), kept mounted (`[hidden]`);
   a `MutationObserver` counts only non-empty activity as details (the chevron
   appears with the first step). Otherwise the run list (`view.steps`,
   `aria-label="Runs"`), only while running or when two or more finished runs
   ended differently (never a lone `Run · done`; a finished task shows no
   current-step line).
2. **Outputs of a running task.** `view.outputs` via `ChatContentBlocks`.

There is intentionally no **Watch live**; the activity slot streams the run.

**Cancel chip.** Non-terminal with `onCancel`: a danger-tone `.mg-chip-action`
left of the chevron (`margin-inline-start: auto`; `aria-haspopup="dialog"`,
`aria-expanded`) opening a popover under it, never `window.confirm`:
`role="dialog"`, `aria-modal="false"`, **Cancel this task?**, **Keep running**
(focused on open) / **Cancel task** (awaits `onCancel(view.id)`, then closes).
Esc or **Keep running** close and return focus to the chip; an outside
pointer-down closes without moving focus. Enters over `--dur-quick` /
`--spring-snappy`; while open the card sits at `position: relative; z-index: 5`.
Never toggles expansion; closes when the task turns terminal.

**Cancel.** The card never fetches. The host injects `ChatPanel`'s
`handleTaskPanelAbort(taskId)`, wrapping
`taskStore.abortTask(taskId): Promise<TaskStatus>` (`src/lib/stores/taskStore.ts`),
which loads the record if not in the store, cancels the active execution via
`POST /api/magician/v3/executions/{id}/cancel` and persists the reset status.

### ChatContentBlocks previews

`ChatContentBlocks.svelte`, shared by every chat surface (cards, bubbles,
attachments, escalations, activity, voice), previews drawable files by default:

| File | Default |
|------|---------|
| Image | thumbnail |
| PDF (`application/pdf`, ≤ 20 MB or size unknown) | inline `<iframe>` above the Open card via `authenticatedTaskOutputPdf` (`taskOutputs.ts`), which re-types the blob `application/pdf` so a mislabelled body never runs as HTML on the app origin; on failure only the Open card remains |
| Markdown, JSON, `text/*`, `.txt/.log/.csv/.tsv`, common source/config extensions | auto text preview within 256 KB (`File too large for inline preview` above, no fetch when size known); **Hide preview** collapses, **Show preview** reloads |
| HTML, video, audio, other binaries | file / Open card |

Previews fetch on first view (`IntersectionObserver`, 200 px margin;
immediately without one). Every file action (Open, Download, Open in app,
Reveal in Finder, preview toggles, Open original) uses `.mg-chip-action`.

## Presence + composer lift

**`PresenceOrb.svelte`**: `state: 'idle' | 'thinking' | 'working' | 'needs-you'`,
a 24 px CSS orb with `data-state`, `role="status"` and hidden text from
`presenceLabel` (`Magician is idle` / `is thinking` / `is working` /
`needs you`) that the live region announces.

| State | Look |
|-------|------|
| `idle` | `--tint-1` → `--tint-2` core, slow breathe (scale 1 → 1.04, `--dur-slow` × 6) |
| `thinking` | rotating conic ring (`--accent-primary` + tints), smaller core |
| `working` | `--accent-primary` core, faster pulse (`--dur-slow` × 2) |
| `needs-you` | `--color-warning` fill, one `--spring-bouncy` bounce, then steady |

Reduced motion: `animation: none`.

**Derivation.** `presence.ts` `derivePresence({ hasPendingHitl, liveTurn,
streaming, toolsRunning })`, precedence **needs-you > working > thinking >
idle**. ChatPanel computes `presenceState` only under chat-next:

| Signal | ChatPanel source |
|--------|------------------|
| `hasPendingHitl` | a visible message's turn id is in `$pendingHitlByChatTurnId` (persisted escalation cards do not count, or old clarifications would pin the orb) |
| `liveTurn` | `liveTurnId`: in-flight send, else a correlated active task's turn, else `latestOpenUserTurnId` (`liveTurn.ts`: newest typed user turn without reply or terminal task, under 10 minutes, 30 s clock; never `voice-turn-*` / `voice-request-*` / `realtime_voice` rows) |
| `streaming` | a visible assistant message with id prefix `streaming-` and non-empty text |
| `toolsRunning` | `activeTaskTurnId` (a turn-spawned task still running) |

**Mount.** FloatingComposer's `dock-actions` slot, after the session
ContextPill and before the host's `composer-dock` slot, only when `$chatNext`.

**Composer lift.** Under `.chat-next`, `.composer-shell:focus-within` gets
`translateY(-2px)` and `box-shadow: var(--shadow-float), var(--ring-focus)`
(transform `--dur-base` `--spring-snappy`, box-shadow `--dur-base`), only while
focused, so the resting `view-transition-name: command-ask` shell is
untouched; instant under reduced motion.

## Empty state

`ChatEmptyStateNext.svelte` is a drop-in for `ChatEmptyState`: same props
(`isReadOnly`, `recentSessions`, `onSuggest(prompt)`, `onOpenSession(sessionId)`)
and prompts (`EMPTY_STATE_PROMPTS`,
`src/lib/magician/chat/components/emptyStatePrompts.ts`); no new fetch. Root:
`data-testid="chat-empty-next"`.

- **Greeting.** `h3` in `--font-display`: `Good morning` (< 12:00),
  `Good afternoon` (< 18:00), else `Good evening` (local clock at mount), plus a
  subline; no name. Read-only: `Nothing in this session`, no subline.
- **Suggestion cards.** First four prompts as buttons, 2 columns (1 under
  480 px): icon tile on `tintVars(prompt)`, label as title, full prompt as a
  one-line hint; click / Enter / Space calls `onSuggest(prompt)`, which fills
  the composer without sending. Hidden when `isReadOnly`.
- **Recent sessions.** Up to five pills on `tintVars(session.id)` (title,
  relative time) → `onOpenSession(id)`; ChatPanel passes at most three.
- **Motion / surface.** `in:rise|global`, cards 50 ms apart, 2 px hover lift
  (`--dur-base` `--spring-snappy`), `--ring-focus` on focus-visible (none under
  reduced motion); no own background, so `.chat-next` `--wash-hero`
  shows through, with an `--accent-primary-soft` glow behind the greeting.

Tests: `ChatEmptyStateNext.component.test.ts` (card → `onSuggest`, shared
prompt source, read-only hides cards, pill → `onOpenSession`, five-pill cap,
greeting by hour via `vi.setSystemTime`).

## Wiring

`ChatPanel.svelte` reads `$chatNext` (see [`chatNext` flag](#chatnext-flag))
and switches presentation only; pipeline, stores and handlers are shared. Flag
off, the panel renders exactly as before.

- **Variant props.** Every transcript `StructuredResponseRenderer` and both
  `RequestActivityCard`s (assistant bubble, pending "typing" bubble) get
  `variant={$chatNext ? 'next' : 'classic'}`. `ChatPanel` mounts no turn-level
  `ChatTurnProgress`; its only one is the task-scoped instance in the live card's
  activity slot, always `variant="next"`.
- **Typing dots.** Under chat-next, `.chat-typing-dots` hide once the pending
  bubble's card renders a `TurnStage`
  (`.chat-turn-typing__bubble:has([data-testid='turn-stage'])`); before the first
  row, or while the card is collapsed, they stay.
- **Live card over the sidecar.** In the `task_status_update` branch chat-next
  renders `LiveTaskCard` (`onOpenTask={openTaskFromLiveCard}`) even with a
  validated structured sidecar; classic renders the sidecar via
  `StructuredResponseRenderer`, else `TaskStatusCard`.
- **Cancel.** `onCancel` is `handleTaskPanelAbort` only with a real
  `content.task_id`; otherwise no Cancel.
- **Open task.** `openTaskFromLiveCard(content, sourceEl)` calls
  `openTaskDetailsFromStatus(content)` (drawer `TaskPanelDrawer`, or the run
  inspector without a task id), wrapped in
  `morph(sourceEl, () => document.querySelector('[data-morph-target]'), …)` only
  when the task is cached in `taskStore` (the drawer's dialog carries
  `data-morph-target`); uncached it just opens, since a transition over a fetch
  freezes the page. `openTaskPanel` opens the drawer **at once on its loading
  skeleton** (`taskPanelOpeningId`), reads the task by id
  (`taskStore.fetchTaskRecordById`, else a `/v3/tasks` list load) and fills the
  same drawer; a missing task closes it with a "Task not found." toast. A
  close, another open or a scope change supersedes an in-flight open
  (`taskPanelOpenRequest`). `onInspectFromIds` is `openInspectionPanelFromIds`.
- **Live activity slot.**
  `<ChatTurnProgress variant="next" bare taskId executionId>`, mounted while the
  task is non-terminal with a real id (hidden until expanded; one subscription
  even collapsed), keyed on task + execution id (latest run group's, else the
  message's), so a new run re-subscribes; it backfills via one
  `/v3/events?backfill_only=true` request when it has an execution id, else is
  live-only. Live events come from the app's event hub; the card holds no
  stream. **No `sessionId`** on
  purpose: unmount would capture rows into `chatTurnActivityStore` as the next
  reply's "What happened". `bare` (`chat-turn-progress.md`): no second
  "Assistant / Working…" bubble; a HITL pause still reads `Waiting on you`.
- **Ask dock above the composer.** `ComposerAskDock` lists asks from
  `pendingAsks` (`src/lib/attention/pendingAsks.ts`) whose resolved thread
  (`scope.thread_id`, else the feed row's `ui_thread_id`, else its task's) is
  this one or whose task belongs to it (e.g. a Relentless review). Choice/field
  kinds get Answer; a `review_href` renders `Review →`. Plan approvals stay in
  the planner dock.
- **Chat-started tasks.** <a id="chat-started-tasks"></a>Chat-next's
  `visibleMessages` is `collapseTaskProgressMessages(…)` alone; classic also
  applies `attachInTurnActivityToAssistantMessages` (`chatStore.ts`), folding an
  in-turn non-terminal `task_status_update` (`inTurnActivityHidden`) into the
  reply's activity dropdown. Chat-next shows the card from its first update (the
  delegate path persists a `running` card with `execution_id` at dispatch,
  `chat_running_task_status_message` in
  `magician/src/magician_v2/chat/service.rs`), coalesced by task id; the reply
  does not remove it.
- **Message entrance.** `in:msgIn` calls `rise` under chat-next, `fadeUp`
  otherwise; the streaming row's `animate:flip` duration-0 rule is unchanged.
- **Surface.** Root `class:chat-next`; `.chat-page.chat-next` paints
  `var(--wash-hero)`, `.composer-shell` takes `var(--surface-glass)`.
- **Empty state.** `ChatEmptyStateNext` vs `ChatEmptyState`
  (`.chat-empty-state`) via one `<svelte:component>` ([Empty state](#empty-state)).
- **Left alone.** `EscalationCard` (escalation / HITL) renders identically.

Tests: `ChatPanel.chatNext.component.test.ts` (flag on vs off across cards,
orb, empty state, Cancel, chat-started ordering); `presence.test.ts`;
`PresenceOrb.component.test.ts`.

## Shell around chat

The floating quick-chat bubble (`routes/(app)/ChatBubble.svelte`) is not mounted
on chat routes (`/chat`, `/t/<thread>/chat`); it stays on every other page.

## Expand / collapse keeps the card in view

`keepToggledInView` (`src/lib/magician/chat/keepToggledInView.ts`), on the
messages scroller: a click (capture phase) on a control with `aria-expanded`,
`aria-controls` or `data-toggles-content` follows its card for 3 s, per frame,
until wheel / touch / key scroll. A grown card scrolls down without losing its
top; one collapsed above the view scrolls up to it. Scroller: nearest scrollable
ancestor with room, else the page. Capture phase because toggles like "Show
preview" replace themselves in their handler. New expandable cards get this by
using one of those attributes on their toggle. `TaskPanelDrawer` uses it with
`.act` and `.tech-details` as follow scopes.

## Date dividers + floating date

Each local day's first message carries a `.chat-day-divider` pill ("Today",
"Yesterday", a weekday within the week, else a short date with the year only for
other years; `src/lib/magician/chat/dayLabel.ts`), inside the row since
`animate:flip` needs one element per keyed item. Scrolling history (not pinned
to the tail, so streaming never flashes it), a sticky zero-height rail shows the top day as a glass
pill, fading ~1.1 s after scrolling stops.

## Planned tasks on the live task card

The host passes the store's `storeStatus`, `plan` and three callbacks.

- **Plan preview:** before a run (`queued`, or `draft` awaiting review), the
  expanded card shows **Plan · N steps** and a **Plan graph** chip
  (`onOpenPlan`, the plan sheet).
- **Loading the plan:** a not-started card fetches steps once (`onNeedPlan` →
  `taskStore.refreshTaskPlan`); list rows carry none.
- **Run:** an approved, not-started task (`startable`: effective `ready`) gets a
  **Run** chip next to **Inspect →** (`onRun` → `taskStore.executeTask`).
  Approval alone does not start a run.
- **Dot colour:** not started uses `--accent-secondary`.

### Plan-step line

While a planned task runs (`liveTaskHasPlan`, `running`, real task id), the card
polls the run's `/execution-panel` for `debug.selected_execution.step_statuses`;
when present, a summary line and step bar replace the generic step line. Strings
and segment rule match iOS (`PlanProgressModel.swift` `PlanSteps`,
`LiveTaskStepBar`) and Android (`PlanProgress.kt`).

- **Mapping** (`chat/next/planStepProgress.ts`, parsed by
  `extractPlanStepsFromExecutionPanel`): `completed` / `failed` / `in_progress` /
  `skipped` map through; anything else (cancelled, unknown) is pending, read
  as not reached once the run ended. Plan order kept. No plan steps or a
  finished task → card unchanged.
- **Line** (`planStepSummary`): `Step 3 of 6 · <name>` while running,
  `Step 2 of 3 failed · <name>` after a failure, else `6 of 6 done`
  (`2 of 3 done · 1 skipped`).
- **Bar** (`planStepBar`): a segment per step up to 12
  (`PLAN_STEP_BAR_MAX_SEGMENTS`), else one fill of (done + skipped) ÷ steps.
  Done `--status-completed`, failed `--status-failed`, in progress
  `--status-running`, skipped `--text-muted`, pending `--border-default`; 5 px
  segments rounded at `--radius-sm` / 3, colour transitions off under reduced
  motion.
- **Accessibility:** bar `role="img"` labelled `planStepAccessibleSummary`
  (`Step 3 of 6: <name>`); the line is `aria-hidden`.
- **Poll:** `chat/next/planStepPoll.ts`, one `createSharedPoll` per task across
  cards: every 5 s while subscribed, jittered backoff to 60 s on failure, last
  good value kept. Reads use `taskUsage.readTaskRunStateShared` (the usage
  snapshot's throttle: at most two panel reads at once; refreshes that
  snapshot). The drawer's own poll covers only its open task.

## Usage on the live task card

The status line shows `N calls · X tok · $Y` from `taskUsage`
(`chat/next/taskUsage.ts`): live from each `llm.succeeded` the task-scoped
`ChatTurnProgress` (with `taskId`) sees for the run and its delegated children,
once per call id, dropping events not attributable to the task
(`eventBelongsToTask`); plus, once per phase (planned, running, done),
`loadTaskUsageSnapshot` from `/execution-panel` (run: `deriveTimeline` →
`runCost`; planning: `debug.planning_timeline` `cost_usd` / `tokens`). Shown as
run + planning (live run total preferred; tooltip splits them); survives reload.

The turn chip (`RequestActivityCard`) is the whole turn — own calls plus every
task started by `create_task` / `run_task`, "N calls · X tok · $Y · incl. K
tasks" — so it is never below any of its task cards.

- **Task ids.** `tool.call.finished` `spawned_task_ids` (backend fills it from
  the full tool result); older turns fall back to
  ids in the 240-character `content_preview` (may miss some). The chip loads
  each task's `loadTaskUsageSnapshot` itself, on screen or not.
- **Own calls only.** `summarizeOwnCalls` (`turnUsageSummary.ts`) counts rows with
  no `execution_id` or the turn's own, once per `llm_call_id` (the chat call
  lands as both `llm.succeeded` and `LLMResponseReceived`; id-less rows count
  individually). Spawned/delegated tasks (also from `chat.delegate.*` /
  `task.status_changed`) come only from the task usage store.
- **Whole-turn totals.** `GET …/turns/{tid}/events` returns the newest ~1 MiB
  (`truncated: true`), with `usage` (`calls`, `input_tokens`, `output_tokens`,
  `cost_usd`) over the whole file under the same own-calls, per-call-id rule.
  `fetchEventsPage` stores it in `chatTurnUsageBaselineStore` with the dedupe
  keys then held; the chip adds only rows outside that set (a later row of a
  covered call is skipped). Cache figures use `cache_read_tokens` /
  `cache_creation_tokens` when sent, else the rows. Older servers send no
  `usage`; the chip sums rows.

## Slash commands in the composer

Contract: `docs/plans/2026-10-03-slash-commands-and-run-budgets.md`. A leading
`/` in a single-line draft opens a picker in the `@` picker's dock (ArrowUp/Down
wrap, Enter/Tab or click). Commands run client-side, never sent as messages.

| Command | Options | Runs |
|---------|---------|------|
| `/budget` | this chat's running/paused tasks, newest first (`task_status_update` cards ∪ thread `$taskStore` rows; store status wins) → `+$25`, `+$100`, `+1M tokens`, `+50 iterations`, `Unlimited` | `POST /api/magician/v2/tasks/{task_id}/budget` with `add_cost_usd` / `add_tokens` / `add_iterations` / `unlimited` |
| `/budget default` | harness `$50 / $100 / $250 / Unlimited`, native `$5 / $15 / $50 / Unlimited`, current from `GET /budgets/defaults` | `PUT /api/magician/v2/budgets/defaults`, both caps (only the changed one if the GET failed) |
| `/engine` | engine × model rows from `GET /plane/engines` (browser choice marked), then "Make default everywhere…" (server default marked) | browser: `chatHarnessPreferenceStore.select`; everywhere: `PUT /plane/chat-engine` then `adoptServer` |
| `/run-engine` | same rows, run engine marked | `PUT /plane/engine` (`setRunHarnessEngine`) |

- **Levels.** The draft reads `/<chosen tokens> <query>` (`slashPrefix`); every
  query word must match, label-prefix first. Deleting into a token, Backspace on
  an empty query, or Escape steps back (`resolveSlashInput`); Escape at the top
  closes and keeps the text.
- **Plain `/` text still sends.** At the top level a non-command draft
  (`/usr/bin is full`) closes the picker and sends; deeper, Enter and the send
  arrow pick the highlighted option, so a half-typed command never goes out.
- **Confirmation.** Success clears the composer with a `showSuccess` toast
  (e.g. `+$100 for "Research ACME"` with the effective budget
  `Cost $98.20 of $125 · 11 of 60 iterations · resumed`); failure shows
  `Command failed` with the server's `message` / `error`; a bare 404 on a budget
  route reads "This Magician server does not offer run budgets yet". An
  unreadable defaults GET shows a note and leaves rows pickable.

Code: `composerCommands.ts` (pure triggers, levels, options),
`slashCommandActions.ts` (budget API, runner), `SlashCommandPicker.svelte`,
`FloatingComposer.svelte` (`slashCommandsEnabled`, `slashContext`; emits
`slashNavigate` per level and `slashCommand`), `ChatPanel.svelte` (refreshes
engines, defaults or tasks per level; toasts). Tests: `composerCommands.test.ts`,
`slashCommandActions.test.ts`, `FloatingComposer.slash.component.test.ts`.
