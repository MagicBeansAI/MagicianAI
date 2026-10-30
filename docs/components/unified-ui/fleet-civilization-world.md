# Town Square Crew World

`/square` is the desktop command game for Magician's live agent crew. The first
viewport is a full-bleed hero with two views, a 2D **Floor** (default) and the 3D
**Campus**, behind an HTML command layer. Below it sits the Today operating
surface (same scoped stores and server-paginated APIs as Today). The canonical
crew leaderboard belongs to Work → Crew and opens over the world from the Crew
HUD command; “Open full crew record” opens `/work?view=crew&agent=…` (no legacy
Crew page needed).

Everything is driven by real agents, programs, tasks, approvals, execution
trees, deliveries and cost data. Visible UI uses Crew, Tasks, Playbooks,
Delegation, Capabilities and Usage; fleet/citizen/guild/quest are
implementation names only. Mobile is out of scope. Design source:
`2026-07-10-town-square-aaa-desktop-game-experience.md`.

## Runtime contract

```text
GET /api/magician/v2/fleet-state
Authorization: Bearer <workspace-bound-token>
```

Scope comes from `scopeIdentityStore`. Schema `fleet_state.v1alpha1` carries
per-section availability (`available` / `partial` / `unavailable`) for citizens,
guilds, quests, attention, handoffs, deliveries, economy and social; the client
never invents a value for an unavailable section. Polled every 30 seconds, with
bounded specialized readers as fallback while the endpoint is unavailable.
Active handoffs inspect only active root executions (no historical tree
fan-out). Backend: [`fleet-state-api.md`](../magician/fleet-state-api.md).

Crew readiness comes from the shared `crew_health.v1` projection
(`GET /api/magician/v2/agents/health`: current score, trailing seven-day
calls/spend/success, rolling daily average and delta, bounded history); Town
Square neither queries analytics SQL nor computes health locally. See
[`crew-health.md`](../magician/crew-health.md).

## Experience layers

### Hero view modes

The hero is a two-column grid: world pane and **command dock**
(`.square-hero__dock`, `CommandDock.svelte`), open by default and hideable
(`localStorage` `square.commandDock`). The dock is fleet-wide (Activity, Crew,
Work, Delegation, Spend) until a crew member is selected (Work, Activity,
Command, Capabilities, Delegation). An `unavailable` section says so; an empty
tab is a different claim.

The `Floor | Campus` toggle (`square.heroView`, **Floor default**) mounts exactly
one view (the campus owns a WebGL context, render loop and polling). Both share
`SquareWorldChrome` (Create task, Attention, status bar, roster strip). Click
selects into the dock; keys `1`–`9` select the first nine; on Campus a
double-click flies the camera. Status-bar clicks open the matching dock tab or
Attention. Town Square and sound controls live in the Floor | Campus | Dock
strip.

Hero height is `height: var(--hero-h, calc(100dvh - var(--v5-topbar-h)))`; the v5
shell publishes `--v5-topbar-h` (also read by `TopBar`), so the CSS fallback is
exact on first paint (plain `100dvh` would overshoot by the bar) and the JS
`--hero-h` measurement covers variable-height bars.

### Floor (default)

`office/OfficeFloor.svelte` renders the whole crew as a fixed 2D office, no
camera. Module `lib/magician/square/office/`:

| File | Role |
| --- | --- |
| `floorPlan.ts` | pure `(citizens, guilds, aspect) -> FloorPlan` layout solver |
| `officeCrew.ts` | `AgentSummary[] -> CitizenVM/GuildVM` via shared derivations |
| `officeTone.ts` | measures the app theme's luminance to pick a palette |
| `office-theme.css` | `--office-*` surface tokens, light and dark |
| `actors.ts` | per-citizen actor state, owned outside the render pass |
| `officeEvents.ts` | snapshot differ; emits only what the data can prove |
| `eventQueue.ts` | staggered playback of proved events |
| `ambient.ts` | idle milling; never pre-empts an event walk |
| `officeMotion.ts` | CSS-transform action and hop runner |
| `sprites/` | hand-authored inline SVG parts and character casting |

#### Layout

`buildFloorPlan(citizens, guilds, targetAspect)` is pure and deterministic (no
clock, DOM or `Math.random`), so re-derived view models never swap desks.
`floorPlan.test.ts` pins: each member seated once, no shared desks or overlapping
rooms, doors onto the corridor, nothing outside the shell, identical output for a
reordered roster. Two room bands flank a corridor; rooms are sized by team and
balanced across bands by greedy width; meeting room north, pantry and lounge
south, corridor amenities with the entrance at one end.

#### The plan is solved to the pane, not fitted into it

The pane aspect (quantised to two decimals, so drag-resize does not re-solve
every frame) is a solver input, so the floor fills the pane with no `contain`
dead margin. The solver drafts the floor at several column biases and keeps one
desk grid per plan (smallest solved width; ties keep the widest grid), then
grows the short axis via `spreadSlack` (vacant rooms excluded) and a capped
corridor slice, up to `MAX_STRETCH`; only a near-empty office may miss the
target, by being narrower. **An unstaffed program gets about a third of a
staffed room's frontage.** The plan reports its own `width`/`height`: a larger
roster makes a larger building, never overflow.

#### Rooms come from membership, seats from the primary program

Rooms are the union of programs anyone is attached to (`citizen.guildIds`);
citizens sit only in their primary program (`citizen.guildId`). A program with
only secondary attachments is drawn as a **Vacant** room.

#### Two render layers

Graphics are inline SVG scaled to fit; type is unscaled HTML over it (Departure
Mono goes soft in a scaled `viewBox`). Below a 54px rendered desk width, plaques
give way to a desk-edge status chip.

#### Sprites and people

`sprites/` are flat-fill SVG (no gradients or filters). `personLookOf` picks
gender, skin tone, hair and outfit independently from a hash of the citizen id,
so a face is stable across polls and views; role badges reuse the HUD's three
(primary, CEO, envoy). `plan.personScale = plan.seatScale * PERSON_RATIO`
(`PERSON_RATIO` = 0.66, size not projection); `DESK_H` and the solver-owned
`plan.seatBox` (click target, plaque anchor) derive from it. Status uses the
shared `--fleet-*` contract; the needs-you bubble is the only world-space
status.

#### Name plaques

Each member has a `.fw-plaque` (same object as the campus; `plaqueRole()` in
`derive.ts`), `.fw-plaque--stacked` rotated 90° to fit the desk cell, hung from
a band above the tallest possible head so a row aligns. `cullPlaques()` places
by priority (selection, hover, needs-you, live work, your own agent, rest; ties
on id, so polls never reshuffle) and hides overlaps with `visibility: hidden`
(not removal, so the cull cannot oscillate). Verify after `document.fonts.ready`
(`font-display: swap` measures plaques narrower until the font lands).

#### Theming

Architecture and furniture are toned via `--office-*`; **people are not**: skin
and hair are fixed, so diversity never depends on the theme. There is no
per-theme table (no `[data-theme]` block declares `color-scheme`):
`officeTone.ts` measures the app background's luminance and stamps
`data-office-tone="light" | "dark"`. Chrome wears `data-game-skin="pixel"`.

#### Selection

Click emits `select` with the citizen id; the page owns `dockTarget` and the dock
shows `CitizenDetail`. The Floor overlays no selection card.

#### Floor motion

Motion comes only from changes a snapshot **differ** can prove between
consecutive `fleet-state` polls; anything indistinguishable from a re-derivation
artefact emits nothing. Actor state (`at-desk` / `walking` / `away` /
`signalling`) lives in `office/actors.ts`, keyed by citizen id; reconcile keeps
unchanged actors, drops the departed, seats arrivals. Routes come from the plan
(desk → own door → corridor → target door → target), not pathfinding, and the
browser interpolates CSS transforms (no `requestAnimationFrame` simulation).

`officeEvents.ts` emits only:

| event | proof |
|---|---|
| `delivery-landed` | a `created_at` newer than the last seen |
| `handoff` | an unseen handoff id with both ends seated |
| `task-routed` | a work item no citizen held last snapshot |
| `blocked` / `unblocked` | an attention item for a citizen appeared/disappeared |
| `work-started` / `work-ended` | vibe crossed into/out of `working` |
| `social-talk` | an unseen public-feed `post_id`, author seated, social section visible on both snapshots |

Reordered rosters, identical re-derives, unavailable sections and the first poll
after mount emit **zero** events.

`eventQueue.ts` releases events ~1.6s apart from a capped queue; overflow drops
the oldest low-priority events (`blocked` > `handoff` > `task-routed` >
`social-talk` > `delivery-landed` > rest) and logs it. Playback: task token
Task router → assignee; handoff pair meets in the meeting room; social talk
walks to a venue hashed from the thread root; delivery lands on the shipper's
desk; `blocked` raises a signal that opens the HITL prompt. Social talks and
ambient never pre-empt a blocked signal or in-flight handoff.
The social feed lives at `/square?tab=social` (`/town-square` redirects there).

The Floor refreshes ~1.5s (debounced) after `v2Events` task/execution/delegation
events; the 30s poll is the backstop. Ambient (`office/ambient.ts`) is a
function of vibe and pauses while the tab is hidden. `?office-motion-preview=1`
enqueues a synthetic preview (not a proved diff). `prefers-reduced-motion`
applies state changes instantly.

### World (Campus)

`FleetWorld.svelte` maps the fleet snapshot and live `AgentSummary` records into
`CitizenVM` / `GuildVM` and owns the Three.js engine lifecycle, keeping the
renderer independent of Svelte stores. It shows crew state, program
buildings, the Task router, Capabilities and Delegation landmarks, handoff
beams, local time and weather, minimap, status rings and merit markers.

#### Layout

`engine/grid.ts` is an orthogonal campus lattice (avenues every eight tiles,
7×7 blocks) centred on the paved **quad** with the Task router.

- Each structure takes a 2×2 lot in a block's north-west corner fronting two
  avenues, door on the avenue tile due north (all doors face one way); lots go
  nearest-the-quad first: Capabilities, Delegation, then programs.
- **Capacity:** a 48-tile grid holds 35 lots (two landmarks + 33 programs).
  Beyond that a program gets no building, pick mesh or centre entry (HUD focus
  does nothing), and its crew parks on the first program's benches. Housed
  programs are a roster prefix, so gaining one never evicts another; more
  programs need more lots, not a softer fallback.
- Avenues are paved first and never built on, and work spots come only from
  paved tiles, so every door and work spot is on one connected network.
  `engine/grid.campus.test.ts` pins these invariants.
- Desks back onto their own lot; two of six spots per lot stay desk-free but
  walkable.

#### Idle wander

`startWander` (`engine/citizens.ts`): a walkable tile within 8 (Manhattan) of
the work spot, 15% of trips within 20, at 1.7 tiles/s. Members of an unhoused
program anchor on the fallback benches; unseen ones on the quad.

#### Model assets

One pack, `city`, at `static/fleet/<pack>/models` (licences at the pack root);
characters in `static/fleet/chars`, shared assets (clouds) in
`static/fleet/shared`, never a pack. Every model `uri` names a sibling file.
`worldStyle.ts` idiom fields carry one member each. Loading is best-effort: a
404 keeps the procedural campus. When models load, the capsule stays as the
hidden raycast proxy with `colorWrite = false`, **not** `transparent` (part of
three's program cache key; flipping it needs `needsUpdate`). The Task router is
the pack's tallest generic building, held out of the program pool.

#### World palette

`fleet-theme.css` has one `--fleet-*` block on `:root`; the hero carries a fixed
`data-game-theme`. The world ignores the app light/dark theme except the five
status colours, which the HUD aliases into `--game-state-*` (retuning one
repaints HUD chrome and the leaderboard). `--fleet-needs` stays amber because
the HUD pairs it with `--color-error`; `--fleet-handoff` is the only world-only
token. Toon shading means **separation comes from value, not hue**: terrain
tokens are `InstancedMesh` instance colours in a ~10 L\* ladder, status rings
sit below the lawn in value, the delegation beam is the only overlay not
co-planar with the ground, and contrast is judged with alpha on both sides.
Reachability follows the asset manifest, not `useModels` guards:
`--fleet-wall` / `--fleet-roof-hall` reach only the procedural fallback,
`--fleet-roof` reaches nothing (read by `palette.ts`, no consumer), and the city
pack has no rock models. Every `var(--fleet-*, …)` fallback equals its canonical
token.

#### Flat-shaded render contract

Orthographic, CSS-pixel coordinates, antialiased, DPR capped at 2, filtered
shadows, no fog. `flatLook.ts` converts materials to shared stepped-toon twins
and returns an old→new map so occluder fade follows rendered materials; cached
glTF materials are never mutated or disposed.

**Plaques** are placed by priority (selected, needs-you, working, owner's agent,
paused, rest); needs-you markers, health bars and the nap indicator never cull.
The HUD owns the order, derived from crew state + anchor key, **never screen
position**, and the overlap test is **asymmetric** (standing plaques stay until
they touch; stood-down ones need clear air); collapsing the two paddings
reintroduces flicker. The role chip holds one whole word: a title collapses to
its head noun, falling back to live status if that is too long or repeats the
name.

### Command HUD

- `hud/SquareWorldChrome.svelte`: shared chrome on both views (Create task,
  Attention badge, online/active/needs-you/cost bar, roster strip).
- `hud/CommandDock.svelte`: the docked command panel.
- `hud/FleetHud.svelte`: campus needs-command markers, plaques, hover
  nameplates, minimap, hall task composer (regular or CEO-decomposed),
  program/landmark selection routing, God-Hand redirects into a live execution.

Shared stroke icons, stable dimensions, no emoji as chrome, no raw personas as
names.

### Inspectors

- `CitizenDetail.svelte` (dock): identity, portrait, status, top needs-you,
  execution controls, work drill-down; **Capabilities** and **Delegation** tabs
  are `AgentKit.svelte`. Role prose and history stay in the crew record.
- `GuildInspector.svelte`: program status, goals, crew, reversible goal history,
  bounded tasks; infers no new task-to-program ownership.
- `LandmarkInspector.svelte`: Task router readiness and outcomes (title, tone,
  recency, at most one summary line), or Capability categories and equipped
  crew.

`fleetWorkDrilldown.ts` derives summaries from task rows already in
`fleet_state.v1alpha1` (no fetch, no persistence), bounded to one focused task,
two more current tasks, one latest outcome, four artifact names; opening a task
goes to the canonical Tasks workspace.

### Workspaces

Dismissible overlays hosting canonical components:

- `QuestJournal.svelte`: `TasksWorkspace` with a path to the full Tasks page.
- `CrewBoardSheet.svelte`: Work → Crew's `CrewLeaderboard.svelte` (Needs you /
  Working / Available / Whole crew), on the same rows as Work → Crew.
- `CouncilSheet.svelte`: delegation with observed handoffs and configured routes.
- `TreasurySheet.svelte`: fleet-reported seven-day spend plus `/llm`'s usage
  overview.
- `CrewMemberOverviewOverlay.svelte`: the crew record's
  `CrewMemberOverview.svelte`.

Task-backed result review selects the task in the Tasks overlay; non-task
results link to Today's Delivered view (no synthetic task); a prefilled
follow-up appears only with follow-up context. None add polling: the LLM
overview mounts only when opened (one bounded seven-day query) and the crew
overview loads on its command and tears down on close. The crew record
(`CrewMemberWorkspace.svelte`) keeps ownership of its tabs, loading, execution
state and record actions.

### Scroll body

An intersection observer pauses world rendering and restores normal app chrome
while the user is scrolled into Today (header, pulse, digest, section tabs;
backend pagination for sections, message follow-ups, digest entries and Worth a
look).

## Shared game UI

`src/lib/magician/square/ui/`: `GameHudButton`, `GameStatusInstrument`,
`GameInspector`, `GameWorkspace`, `GameSection`, `GameStat`, `GameObjectiveRow`,
`GameCommandBar`, `GameEncounter`, `GameTooltip`. `game-chrome.css` holds the
tokens, scoped at the Town Square root; HUD materials resolve through app
`--bg-*`, `--text-*`, border, focus and status tokens, and bodies set surface
and text colour explicitly so a live theme switch cannot mix two themes.

Any rule setting `--game-tone` must also set `--game-tone-text` (mixed toward
`--text-primary`), because custom properties substitute where declared.

### Crew skin

`data-game-skin="pixel"` is set by `SquareWorldChrome.svelte`,
`CommandDock.svelte`, `CitizenInspector.svelte`, `RosterStrip.svelte`,
`CrewBoardSheet.svelte` and the hero view strip; `GameInspector`,
`GameWorkspace` and `GameSection` pass it to nested surfaces and `GameTooltip`
copies it to its `<body>` bubble. Quest, treasury, council, guild and landmark
panels do not opt in. The skin changes structure and type, never palette:

- **Display face:** Departure Mono (SIL OFL 1.1), `static/fonts/departure-mono/`,
  declared once; `--game-display-sm`/`-lg` are 11px/22px with whole-pixel
  tracking; Regular only, so rules pin `font-weight: 400` and
  `font-synthesis: none`. Labels, headings, chips only; body copy keeps the
  theme face.
- **Flat plates:** square, hard borders, no blur/shadow/gradient, **opaque**
  (translucent HUD over the canvas can drop to ~4:1).
- **Hard selection:** the plate inverts (fill `--game-text`, paint with the
  surface background), so contrast is symmetric.

The skin stays theme-following because it hosts non-square components
(`ExecutionControls`, `CrewLeaderboard`); every text pair it adds must clear
4.5:1 in all shipped themes. `gameAudio.ts` synthesizes short cues via Web
Audio (no assets, no audio context before user interaction).

Layout rules: world chrome never overlaps the dock or view strip; inspectors keep
world context and overlays use a dismissible scrim; dynamic text is ellipsized
or scroll-contained and never resizes fixed controls.

## Data flow

```text
scopeIdentityStore
  -> GET /v2/fleet-state
  -> GET /v2/agents/health
  -> FleetWorld merges authoritative identity/work/program data with live
     agent status and canonical crew health
  -> CitizenVM / GuildVM / Quest
  -> FleetEngine.setData()
  -> Three.js world + FleetHud command projection

FleetEngine callbacks
  -> hover/select/camera/God-Hand state
  -> HTML HUD inspectors and workspaces

HUD commands
  -> existing task, execution-control, approval, program, budget, and analytics
     APIs
```

Fallbacks:

- fleet-state unavailable: keep the last good snapshot and bounded readers;
- crew-health analytics unavailable: runtime/task health stays partial, LLM
  metrics show unreported, never zero;
- handoffs unavailable: omit beams rather than synthesize them;
- weather or model assets unavailable: deterministic/procedural fallback.

Social mood and recent activity are bounded ambient inputs from the
[Fleet Social Network](../magician/fleet-social-network.md); they never create
quests or outrank Attention. The social feed polls its v2 health/read surface
every 30s with a non-overlapping cancellable request and renders unavailable,
paused, disabled and degraded states explicitly (a failed read is not an empty
square). Agent chatter is a durable per-scope switch on `/square?tab=social`,
default off, persisted via `PUT /social/policy`; roster and daily budgets stay
on each agent's `social_persona`.

## Identity and task semantics

- Display name: explicit name, then stable agent ID; persona text is never
  identity.
- Program membership: authoritative program refs first, focus-area fallback.
- Current work: non-terminal task records (step, substep, blocker, update
  time); a stale active-root pointer never revives terminal work.
- Working: a live roster execution or an executing/planning task. Ready and
  terminal tasks are not working; waiting/blocked work is paused unless an
  Attention item needs the operator.
- Needs you: the shared global Attention count (same store and Attention centre
  as the app top bars).
- Task states (`QuestState`): `planning`, `ready`, `active`, `awaiting_orders`,
  `blocked`, `delivering`, `succeeded`, `failed`, `cancelled`.
- Deliveries: task-backed (review in Tasks) or feed-only (Today's Delivered);
  completed counts also produce bounded district merit markers.

## Controls and accessibility

- `1`–`9`: select crew member; `F`: focus selected target; `Esc`: close the
  topmost surface or clear selection.
- Mouse: pan/orbit/zoom, select landmarks and districts, drag a citizen for a
  live redirect.
- Icon-only commands have names/tooltips; selection controls expose pressed
  state; focus treatment comes from the shared game tokens.

## Verification

Tests: `src/lib/magician/square/{derive,fleetQuests,fleetObjectives,fleetState}.test.ts`,
`office/floorPlan.test.ts`, `engine/grid.campus.test.ts`,
`src/lib/magician/llm/overview.test.ts`. Visual QA of the Floor at `1440x900` and
`1920x1080` (the plan is solved per aspect): shell reaches the inset on all
sides, no letterbox or scrollbar, crew read as small tokens, no overlapping
plaques; check hero fit with the JS `--hero-h` measurement disabled and the
plaque cull after `document.fonts.ready`.
