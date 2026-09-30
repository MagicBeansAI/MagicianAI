# Town Square Crew World

`/square` is the desktop command game for Magician's live agent crew. Its first
viewport is a full-bleed hero with two interchangeable views — a 2D **Floor**
(the default) and the 3D **Campus** — behind an HTML command layer. Scrolling
below the world reveals the Today operating surface, backed by the same scoped
stores and server-paginated APIs as the main Today experience. The canonical
crew leaderboard belongs to `/crew` and opens over the world from the Crew HUD
command.

Everything is driven by real agents, programs, tasks, approvals, execution
trees, deliveries and cost data. Visible UI uses the app's Crew, Tasks,
Programs, Delegation, Capabilities and Usage language; fleet/citizen/guild/quest
names are implementation details only.

Design source:
`2026-07-10-town-square-aaa-desktop-game-experience.md`.
Mobile is out of scope.

## Runtime contract

```text
GET /api/magician/v2/fleet-state
Authorization: Bearer <workspace-bound-token>
```

Scope comes from `scopeIdentityStore`. Schema `fleet_state.v1alpha1` carries
independent availability (`available` / `partial` / `unavailable`) for citizens,
guilds, quests, attention, handoffs, deliveries, economy and social; the client
never invents a value for an unavailable section. Refreshed every 30 seconds;
specialized readers are bounded fallbacks while the endpoint is unavailable.
Active handoffs inspect only active root executions (no historical tree
fan-out). Backend: [`fleet-state-api.md`](../magician/fleet-state-api.md).

Crew readiness comes from the shared `crew_health.v1` projection
(`GET /api/magician/v2/agents/health`): current score, trailing seven-day
calls/spend/success, rolling daily average and delta, bounded history. Town
Square neither queries analytics SQL nor computes health locally. See
[`crew-health.md`](../magician/crew-health.md).

## Experience layers

### Hero view modes

The hero is a two-column grid: the world pane and the **command dock**
(`.square-hero__dock`, `CommandDock.svelte`). The dock is open by default,
hideable (persisted in `localStorage` `square.commandDock`), fleet-wide by
default (Activity, Crew, Work, Delegation, Spend) and swaps to the selected crew
member (Work, Activity, Command, Capabilities, Delegation). An `unavailable`
fleet-state section is said so — an empty tab is a different claim.

The world pane mounts exactly one view via the `Floor | Campus` toggle
(`square.heroView`, **Floor default**). Only the selected view is mounted — the
campus owns a WebGL context, render loop and its own polling. Both share
`SquareWorldChrome` (Create task, Attention, compact status bar, roster strip).
Clicking selects into the dock; keys `1`–`9` select the first nine; on Campus a
double-click flies the camera. Status-bar clicks open the matching dock tab or
Attention. Town Square and sound controls live in the Floor | Campus | Dock
strip.

The hero is `height: var(--hero-h, calc(100dvh - var(--v5-topbar-h)))`; the v5
shell publishes `--v5-topbar-h` and `TopBar` reads the same token, so the CSS
fallback alone is pane-exact on first paint (plain `100dvh` would overshoot by
the bar). The JS `--hero-h` measurement is kept for bars of variable height.

### Floor (default)

`office/OfficeFloor.svelte` renders the roster as a fixed 2D office floor: the
whole crew on one screen, no camera. Module `lib/magician/square/office/`:

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
clock, DOM or `Math.random`): view models re-derive every few seconds, and an
order-dependent layout would swap desks on every poll. `floorPlan.test.ts` pins
each member seated once, no shared desks or overlapping rooms, doors onto the
corridor, nothing outside the shell, and identical output for a reordered roster.

Two room bands with a corridor between. Rooms are sized by team, assigned to
bands by greedy width balance (seeded with each band's amenities), then widened
flush with the shell. Meeting room anchors north; pantry and lounge south; the
corridor carries cooler, plants and printer, with the entrance at one end.

#### The plan is solved to the pane, not fitted into it

A `contain` fit leaves dead margin on one axis, so the component passes the
pane aspect (quantised to two decimals, so drag-resize does not re-solve every
frame) and the solver lays the floor out **to** it — purity intact since aspect
is an argument.

1. **Pick the desk grid** per plan, not per room: draft the floor at several
   column biases and keep the smallest solved width (the fit is usually
   width-bound, so smallest width = largest crew); ties keep the widest grid.
2. **Grow the short axis** inside rooms (`spreadSlack`, proportional to natural
   width) and bands (capped corridor slice), so growth reads as floor, not
   letterbox. `MAX_STRETCH` caps it; only a near-empty office may miss the
   target, and only by being narrower.

Every non-desk plan unit costs crew size, so minimums, padding, corridor and
amenities are sized to contents, and **an unstaffed program gets about a third
of a staffed room's frontage**; `spreadSlack` excludes vacant rooms. The plan
reports its own `width`/`height`, so a larger roster makes a larger building,
never overflow.

#### Rooms come from membership, seats from the primary program

Rooms = union of every program anyone is attached to (`citizen.guildIds`);
citizens sit only in their primary program (`citizen.guildId`). A program that is
only a secondary attachment gets an empty room, drawn truthfully as **Vacant**
(hatched floor a value step darker, stacked chairs and cartons).

#### Two render layers

Graphics are inline SVG scaled to fit; type is unscaled HTML positioned over it.
Departure Mono is an 11px bitmap grid and goes soft in a scaled `viewBox`. Below
a rendered desk width of 54px the per-person plaques are dropped in favour of a
desk-edge status chip.

#### Sprites and people

`sprites/` are flat-fill SVG components (no gradients or filters). People are
**composed**: `personLookOf` picks gender, skin tone, hair colour/style and outfit
independently off a hash of the citizen id, so a face is stable across polls and
views. Role badges reuse the HUD's three (primary, CEO, envoy).

The floor is top-down with front-facing figures, which reads as a map token only
while small: `plan.personScale = plan.seatScale * PERSON_RATIO` with
`PERSON_RATIO` = 0.66 — size, never projection. Derived from the ratio rather
than tuned: the desk row pitch (`DESK_H`, from plaque + figure reach + desk) and
the solver-owned `plan.seatBox` (click target and plaque anchor). The figure is
painted before the desk so the slab crosses under the shoulders. Hover/selection
is a soft ellipse at the desk front, no frame; the rug is an edgeless tint.

Status uses the shared `--fleet-*` contract: plaque role chip, desk-edge chip
when plaques are hidden, and a needs-you bubble over the head (the only
world-space status).

#### Name plaques

Each member has a `.fw-plaque` above their head (same object as the campus;
`plaqueRole()` in `derive.ts` shared), `.fw-plaque--stacked` rotated 90° because
a side-by-side plaque overruns the ~108px desk cell. Plaques hang from a band
reserved above the tallest possible head so a row aligns.

`cullPlaques()` measures rendered boxes and places by priority — selection,
hover, needs-you, live work, your own agent, everyone else, ties on id (so polls
never reshuffle) — standing down any that would overlap. Culled plaques are
`visibility: hidden`, not removed, so measurement does not depend on visibility
and the cull cannot oscillate. Verify after `document.fonts.ready`
(`font-display: swap` makes plaques measure narrower until the font lands).

#### Theming

Architecture and furniture are theme-toned via `--office-*`; **people are not**
— skin and hair are fixed, since re-tinting per theme would make diversity a
function of the theme. No per-theme table (22 `[data-theme]` blocks, none
declares `color-scheme`): `officeTone.ts` measures the resolved luminance of the
app background and stamps `data-office-tone="light" | "dark"`. Chrome wears the
`data-game-skin="pixel"` skin and derives colours from the app theme.

#### Selection

Click emits `select` with the citizen id; the page owns `dockTarget` and the
dock shows `CitizenDetail`. The Floor overlays no selection card or footer bar.

#### Floor motion

Motion comes only from changes a snapshot **differ** can prove between
consecutive `fleet-state` polls — never a frame loop or invented events. Where a
change cannot be told from a re-derivation artefact, nothing is emitted.

Actor state (`at-desk` / `walking` / `away` / `signalling`) is keyed by citizen
id in `office/actors.ts`, outside the render pass. Reconcile keeps unchanged
actors, drops the departed, seats arrivals. Routes come from the plan (desk →
own door → corridor → target door → target), not pathfinding; the browser
interpolates CSS transforms (no `requestAnimationFrame` simulation).

`officeEvents.ts` emits only:

| event | proof |
|---|---|
| `delivery-landed` | a `created_at` newer than the last seen |
| `handoff` | an unseen handoff id with both ends seated |
| `task-routed` | a work item no citizen held last snapshot |
| `blocked` / `unblocked` | an attention item for a citizen appeared/disappeared |
| `work-started` / `work-ended` | vibe crossed into/out of `working` |
| `social-talk` | an unseen public-feed `post_id`, author seated, social section visible on both snapshots |

Reordered rosters, identical re-derives, unavailable sections and the first
poll after mount emit **zero** events.

`eventQueue.ts` releases events ~1.6s apart; the queue is capped and overflow
drops the oldest low-priority events (`blocked` > `handoff` > `task-routed` >
`social-talk` > `delivery-landed` > rest), logging drops. Spectacle: a task
token walks from the Task router (intake desk) to the assignee; a handoff pair
meets in the meeting room; a social talk walks the author (and a seated parent
author) to a venue hashed from the thread root and back; a delivery lands on the
shipper's desk; `blocked` stands the person with a signal that opens the HITL
prompt. Social talks and ambient never pre-empt a blocked signal or in-flight
handoff; `work-started` sits an idle walker back down. The social feed lives at
`/square?tab=social` (`/town-square` redirects there).

The Floor subscribes to `v2Events` (task/execution/delegation) and debounces a
refresh ~1.5s later; the 30s poll is the backstop. Ambient milling is a function
of vibe and does not tick while the tab is hidden (`office/ambient.ts`).
`?office-motion-preview=1` enqueues a synthetic spectacle (a preview, not a
proved diff). `prefers-reduced-motion` applies state changes instantly.

### World (Campus)

`FleetWorld.svelte` maps the fleet snapshot and live `AgentSummary` records into
`CitizenVM` / `GuildVM`, owns the Three.js engine lifecycle, and keeps the
renderer independent of Svelte stores. The world shows crew moving by
active/blocked/ready/offline state, program buildings with task signals, the
Task router, Capabilities and Delegation landmarks (on opposite sides of the
quad, each in its own isometric sightline; Delegation also has a HUD entry),
handoff beams, real local time and weather, minimap, district status rings and
delivery merit markers.

#### Layout

`engine/grid.ts` is an orthogonal campus lattice: avenues every eight tiles on
both axes, leaving 7×7 blocks, phase-shifted so the map centre is a block — the
paved **quad** with the Task router at its centre (paved, not lawn: paving
carries walkability and the palette has no spare value step).

- Every structure takes a 2×2 lot in a block's north-west corner, fronting two
  avenues, door on the avenue tile due north — all doors face one way.
  Structures rotate to the nearest right angle (the raw door bearing is ~18° off).
- Lots are assigned nearest-the-quad first: Capabilities, Delegation, then
  programs, so small crews build a tight core.
- **Capacity:** a 48-tile grid holds 35 lots (two landmarks + 33 programs).
  Beyond that a program gets no building, pick mesh or centre entry (HUD focus
  does nothing) and its crew parks on the first program's benches. Housed
  programs are a roster prefix, so gaining one never evicts another. More
  programs need more lots, not a softer fallback.
- Avenues are paved first and never built on, so every door and work spot is on
  one connected network by construction; work spots come only from paved tiles.
  `engine/grid.campus.test.ts` pins these invariants.
- Desks are placed by the renderer only where they back onto their own lot,
  pushed to the kerb; two of six spots per lot (corner crossing, mid-avenue) stay
  desk-free but walkable.

#### Idle wander

`startWander` (`engine/citizens.ts`) picks a walkable tile within 8 tiles
(Manhattan) of the member's work spot, 15% of trips within 20 (keeps traffic
between blocks). Walk speed 1.7 tiles/s. Walkable tiles form one component.
Members of an unhoused program anchor on the fallback benches; unseen ones on
the quad.

#### Model assets

One style (modern campus), one pack: `static/fleet/<pack>/models` with licences
at the pack root; `city` is the only pack. Characters come from
`static/fleet/chars`; shared assets (clouds) from `static/fleet/shared`, never a
pack. Every model `uri` names a sibling file. `worldStyle.ts` idiom fields carry
one member each — a second look means writing geometry, not widening a union.

Loading is best-effort: a 404 keeps the procedural campus (slab structures,
round trees, hedge ring, desks, capsule citizens). When the wardrobe loads, the
capsule stays as the raycast pick proxy hidden with `colorWrite = false` —
**not** `transparent`, which is part of three's program cache key; flipping
`transparent` on a rendered material requires `needsUpdate`.

The Task router uses the pack's tallest generic building, held out of the
program pool and rendered at a larger footprint, so it has no twin and reads as
the focal point.

#### World palette

`fleet-theme.css` has one `--fleet-*` block on `:root` (sky, terrain, paths,
plaza, foliage, citizen status); the hero carries a fixed `data-game-theme` so
the engine can read and observe tokens. The world ignores the app light/dark
theme except for the five status colours, which the HUD aliases into
`--game-state-*` (retuning one repaints HUD chrome and the leaderboard).
`--fleet-needs` stays amber because the HUD pairs it with `--color-error` (red).
`--fleet-handoff` is the only world-only token.

Toon-ramp shading plus downsampling means **separation must come from value,
not hue**:

- The four terrain tokens are instance colours on one `InstancedMesh` in a
  ~10 L\* ladder; path and plaza share one paving value, told apart by shape.
- Status rings are unlit semi-transparent overlays, so they keep only their
  alpha's share of separation; every ring sits below the lawn in value.
- The delegation beam is the only overlay not co-planar with the ground.
- Contrast is judged with alpha on both sides (ring over lawn in a toon band,
  beam over a composited ring, HUD label over translucent panel over canvas).
- Reachability follows the asset manifest, not `useModels` guards: the pack's
  baked atlas colours buildings and trees, so `--fleet-wall` /
  `--fleet-roof-hall` only reach the procedural fallback; `--fleet-roof` reaches
  nothing (read by `palette.ts`, consumed by no builder); the city pack has no
  rock models.

Every `var(--fleet-*, …)` fallback equals its canonical token.

#### Flat-shaded render contract

Orthographic world, CSS-pixel interaction coordinates, drawn sharp:
antialiased, DPR capped at 2, filtered shadows, no fog. `flatLook.ts` converts
standard materials to shared stepped-toon twins after construction and returns
an old→new map so occluder fade follows the rendered materials; cached glTF
materials are never mutated or disposed. The sun follows the real local arc but
the key light keeps a night floor and the sky stops at a legible blue hour (art
direction only). Portraits use the same conversion at half resolution.

**Plaques** are placed by the engine in priority order (selected, needs-you,
working, owner's agent, paused, rest) and any that would overlap stands down for
that frame — disclosure is the room the camera gives, not a zoom threshold.
Needs-you markers, health bars and the nap indicator never collide-cull. Two
load-bearing properties: the order is owned by the HUD and derived from crew
state + anchor key, **never screen position** (else survivors reshuffle as the
camera drifts); and the overlap test is **asymmetric** — standing plaques stay
until they touch, stood-down ones need clear air to return. Collapsing the two
paddings reintroduces flicker.

The role chip holds one whole word: a multi-word title collapses to its head
noun; a too-long head noun or one repeating the name falls back to live status.

### Command HUD

- `hud/SquareWorldChrome.svelte` — shared world chrome on both views: Create
  task, Attention (same badge count as the app top bars), compact
  online/active/needs-you/cost bar, roster strip.
- `hud/CommandDock.svelte` — the docked command panel (tabs above).
- `hud/FleetHud.svelte` — campus-only: needs-command markers, plaque priority,
  hover nameplates, minimap; the hall task composer (regular or CEO-decomposed);
  program/landmark selection routing; God-Hand redirects into a member's live
  execution.

Shared stroke icons, stable dimensions, no emoji as chrome, no raw personas as
names.

### Inspectors

- `CitizenDetail.svelte` (in the dock): identity, live portrait,
  status/program, top needs-you encounter, compact execution controls, and a
  work drill-down; **Capabilities** and **Delegation** tabs are
  `AgentKit.svelte` (bounded tool list, standing targets). Full role prose,
  history and responsibilities stay in the crew record.
- `GuildInspector.svelte`: program status, counts, goals, assigned crew,
  reversible goal history, bounded current/recent tasks of that crew. It infers
  no new task-to-program ownership contract.
- `LandmarkInspector.svelte`: Task router readiness and outcomes (title, tone,
  recency, at most one summary line — no evidence, artifacts, criteria or
  history), or Capability categories and equipped crew.

`fleetWorkDrilldown.ts` derives citizen and program summaries from the task rows
already in `fleet_state.v1alpha1` — no fetch, persistence or game workflow. Hard
bounds: one focused task, two more current tasks, one latest outcome, four
artifact names; opening any task goes to the canonical Tasks workspace.

### Workspaces

Bounded, dismissible overlays over the world, hosting canonical components
rather than reproducing pages:

- `QuestJournal.svelte` — hosts `TasksWorkspace` with overlay navigation,
  optional initial selection, and a path to the full Tasks page.
- `CrewBoardSheet.svelte` — hosts `/crew`'s `CrewLeaderboard.svelte` with Needs
  you / Working / Available / Whole crew filters; same normalized rows as
  `/crew`, which merges fleet-state work with the global Attention identity set.
- `CouncilSheet.svelte` — source-to-recipient delegation with observed handoffs
  and configured routes.
- `TreasurySheet.svelte` — fleet-reported seven-day spend plus `/llm`'s
  lightweight usage overview, with links to Budget and LLM usage.
- `CrewMemberOverviewOverlay.svelte` — hosts `/crew/[id]`'s
  `CrewMemberOverview.svelte` with a link to the full route.

Task-backed result review selects the task in the Tasks overlay; non-task
results link to Today's Delivered view (no synthetic task). A prefilled
follow-up appears only when the result has follow-up context. None of these add
polling.

Loading is demand-driven: the LLM overview mounts only when opened (one bounded
seven-day query); citizen projections show only the labeled fleet-reported spend
and report-gap signal; the crew overview loads on its command and tears down on
close. `/crew/[id]` keeps ownership of tabs, memory/history/config loading,
execution state and record actions.

### Scroll body

An intersection observer pauses world rendering and restores normal app chrome
while the user is scrolled into Today. The body holds the full Today header,
pulse, digest and section tabs, with backend pagination for sections, message
follow-ups, digest entries and Worth a look.

## Shared game UI

`src/lib/magician/square/ui/`: `GameHudButton`, `GameStatusInstrument`,
`GameInspector`, `GameWorkspace`, `GameSection`, `GameStat`, `GameObjectiveRow`,
`GameCommandBar`, `GameEncounter`, `GameTooltip`.

`game-chrome.css` defines type scale, spacing, materials, semantic state colours,
focus rings, motion, layers and inspector/workspace geometry. HUD materials
resolve through app `--bg-*`, `--text-*`, border, focus and status tokens (so
chrome follows the app theme), scoped at the Town Square root; bodies set surface
and text colour explicitly so a live theme switch cannot mix two themes.

A semantic tone has two forms: `--game-tone` (fills, rules, dots) and
`--game-tone-text` (mixed toward `--text-primary` for legibility). Any rule
setting `--game-tone` must set `--game-tone-text` too — custom properties
substitute where declared and cannot be derived once at the root.

### Crew skin

`data-game-skin="pixel"` is set by `CitizenInspector.svelte`,
`RosterStrip.svelte`, `CrewBoardSheet.svelte`; `GameInspector`, `GameWorkspace`,
`GameSection` spread rest props so nested surfaces follow, and `GameTooltip`
copies it to its `<body>` bubble. Quest, treasury, council, guild and landmark
panels do not opt in. The skin changes structure and type, never palette:

- **Display face:** Departure Mono (SIL OFL 1.1) at
  `static/fonts/departure-mono/`, declared once. 11px grid, so
  `--game-display-sm`/`-lg` are 11px/22px with whole-pixel tracking; only
  Regular ships, so rules pin `font-weight: 400` and `font-synthesis: none`.
  Labels, headings, chips only; body copy stays in the theme face.
- **Flat plates:** square corners, hard borders, no blur/shadow/gradient,
  banded title bars; **opaque** (translucent HUD over the WebGL canvas can drop
  to ~4:1).
- **Hard selection:** the plate inverts (fill `--game-text`, paint with the
  surface background) — contrast is symmetric, so no per-theme table.

The skin stays theme-following because it hosts non-square components
(`ExecutionControls`, `CrewLeaderboard`). Every text pair it introduces must clear
4.5:1 in all shipped themes.

`gameAudio.ts` synthesizes short cues through Web Audio; no assets, no audio
context before user interaction.

Layout rules: shared world chrome never overlaps the dock or the view strip;
inspectors keep world context and overlays use a dismissible scrim; dynamic text
is ellipsized or scroll-contained and never resizes fixed controls.

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
- crew-health analytics unavailable: keep runtime/task health as partial and
  show LLM metrics as unreported, never zero;
- handoffs unavailable: omit beams rather than synthesize them;
- weather or model assets unavailable: deterministic/procedural fallback.

Social mood and recent activity are bounded ambient inputs from the
[Fleet Social Network](../magician/fleet-social-network.md); they never create
quests or outrank Attention. The social feed polls its v2 health/read surface
every 30s with a non-overlapping cancellable request and renders unavailable,
paused, disabled and degraded states explicitly (a failed read is not an empty
square). Agent chatter is a durable per-scope switch on `/square?tab=social`,
default off, persisted via `PUT /social/policy` (which wakes the worker); roster
and daily budgets stay on each agent's `social_persona`.

## Identity and task semantics

- Display name: explicit name, then stable agent ID; persona text is never
  identity.
- Program membership: authoritative program refs first, focus-area fallback.
- Current work: non-terminal task records (step, substep, blocker, update time);
  a stale active-root pointer never revives terminal work.
- Working: a live roster execution or an executing/planning task. Ready and
  terminal tasks are not working; waiting/blocked work is paused unless a
  canonical Attention item needs the operator.
- Needs you: the shared global Attention count — same store and Attention
  centre as the app top bars.
- Task states: planning, ready, active, awaiting input, blocked, delivering,
  succeeded, failed, cancelled.
- Deliveries: task-backed (review in Tasks) or feed-only (review in Today's
  Delivered); completed counts also produce bounded district merit markers.

## Controls and accessibility

- `1`–`9`: select crew member; `F`: focus selected target; `Esc`: close the
  topmost surface or clear selection.
- Mouse: pan/orbit/zoom, select landmarks and districts, drag a citizen for a
  live redirect.

Icon-only commands have names/tooltips, selection controls expose pressed state,
focus treatment comes from the shared game tokens.

## Verification

Tests: `src/lib/magician/square/{derive,fleetQuests,fleetObjectives,fleetState}.test.ts`,
`office/floorPlan.test.ts`, `engine/grid.campus.test.ts`,
`src/lib/magician/llm/overview.test.ts`. Visual QA of the Floor needs both
`1440x900` and `1920x1080` (the plan is solved per aspect): shell reaches the
inset on all sides with no letterbox or scrollbar, crew read as small tokens, no
two visible plaques overlap. Hero fit is checked with the JS `--hero-h`
measurement disabled, and the plaque cull after `document.fonts.ready`.
