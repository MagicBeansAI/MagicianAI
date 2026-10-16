# Personal Tutor And App Copilot Unified UI Surfaces

Unified UI has three Personal Tutor/App Copilot routes:

- `/hud` for the chat HUD and screen-capture ask flow.
- `/draw-overlay` for the inert transparent drawing layer.
- `/screen-region-picker` for desktop-owned rect selection before a region
  capture is staged.

The native iOS companion does not load these routes. Its screenshot Tutor view
consumes the `tutor.draw.shape` realtime event (kept in the generated
event-taxonomy mirror; desktop `/draw-overlay` delivery still goes through the
Tauri host) and mirrors the playback lifecycle below, with Ask Again and a
Dismiss that calls the scoped Tutor cancel endpoint on an active run. Explain
Deeper appears only once the latest live step has finished drawing and
speaking (also after completion and during replay).

## HUD Progress

`ChatTurnProgress.svelte` renders `tutor.*` realtime events as progress rows
(observing, drawing, delegating, verifying, recovering, completing); the
per-turn activity card renders the same lifecycle from the persisted chat-turn
projection, so it survives refresh. Helper tasks show task cards with a backend
display label (for example `Tutor observation`), not raw helper pack names. On `tutor.run.completed` or
`tutor.run.failed`, both surfaces mark the run's open step rows terminal, so no
stale row keeps the `Steps` spinner alive. Rows are display-only; user gates use
Attention/HITL.

The `/hud` composer stays editable in its compact floating layout; only an
explicit icon expands or collapses the HUD. The HUD passes
`fireAndForget="tutor"` to `ChatPanel`, so tutor invokes (`@tutor`, `@tutur`,
`hey tutor`) and App Copilot invokes (`@copilot`, `@app-copilot`, `hey copilot`)
hide the HUD right after submit while the turn continues in the chat runtime;
other HUD asks stay open.

**App Copilot** (`@copilot`) is the hybrid app-action rail: the overlay
observes, highlights, labels, narrates and shows the next step. A native
pass-through click watcher reports clicks inside the active highlight. The
runtime does not pause before automation: it checks once for an
already-reported click, then delegates the step to `mac-operator`. A user click
while automation is queued or running is recorded and the delegated execution
is cancelled when possible.

## Draw Overlay

In a Vite dev server, `/draw-overlay?fixture=tutor` (or `fixture=working`)
skips sign-in and paints a sample stack for checking controls and labels. The
packaged app has no fixture.

`/draw-overlay` subscribes to `overlay-draw-shape` from the Tauri host and also
drains `take_overlay_draw_shapes` on mount, since the host can emit the first
draw before the listener exists. It pulls the latest status on mount the same
way.

The renderer supports V1 (`highlight`, `arrow`, `clear`) and the V2 educational
primitive envelope used by Live Concept Tutor (labels, callouts, lines,
vectors, rectangles, polygons, circles, angle markers, side labels,
square-on-segment, area fills, formulas, handwriting/cursive text, code
highlights, stack/heap boxes, pointer arrows, flow edges, timeline ticks,
memory cells). Instructional `cursive_text` uses bundled Playwrite USA
Traditional (macOS Brush Script fallback); casual `handwriting` keeps the
decorative stack; review route `/debug/tutor-cursive`. Unless the build sets
`VITE_USE_RECIPE_INTERPRETER=false` (`USE_RECIPE_INTERPRETER` defaults on),
`src/lib/tutor/` fetches the shared recipe set from
`GET /api/magician/v2/tutor/primitives` and a generic interpreter (mirroring
iOS) renders any recipe with native-path geometry; see
[tutor-primitive-recipes.md](../magician/tutor-primitive-recipes.md).

It maps the model's screen coordinates onto a full-window SVG layer, orders
marks by `z_index`, animates strokes, and expires non-persistent diagrams **as
one overlay**. Timed reveals may carry `persist_until_step` (kept until that
later step, still bounded by `ttl_ms` unless `persist:true`).

While marks are visible the overlay dims the screen and draws a
pointer-transparent aurora frame. Colours map to a neon high-contrast palette;
`duration_ms` is clamped to 1.3–3.6 s. Labels are sized in CSS pixels
(17px, formulas 22px) so a 2048-unit viewBox stays readable on
Retina. A spacing pass hides an overlapping earlier copy of the same string and
separates the rest while keeping each label near its anchor.

**Working state.** When a HUD turn includes a tutor/copilot invoke word,
`ChatPanel` asks the host to show the overlay in `working` status before the
first draw: dim screen plus a top-left `Please wait, creating explanation`
bubble below the menu bar. There is no frontend timeout: it clears when shapes
arrive, when an error or cancel marks the status idle, or on dismiss. A
successful assistant response does **not** mark it idle (playback and narration
can outlast the chat text). The status carries the owning chat session id (kept
until idle so Dismiss can cancel) and the active rail (`tutor` or `copilot`).
For App Copilot screen runs, spotlight cutouts come from visible `highlight`,
`rect`, `spotlight`, `mask`, `code_highlight` and `polygon` shapes; Personal
Tutor keeps the normal dim, blackboard mode its stronger board.

### TTL, Keep showing, Replay, Dismiss

Non-persistent diagrams stay for `DEFAULT_SHAPE_TTL_MS = 60000` after the later
of draw-animation and speech completion; marks never expire one by one. On
expiry, marks, controls, dim layer and Dismiss clear together unless **Keep
showing** was clicked.

Dismiss appears as soon as the flow enters `working`. Keep showing and Replay
appear once the batch finishes animating: for Personal Tutor when narration and
drawing are done, for App Copilot only after `tutor.run.completed` /
`tutor.run.failed`.

- **Dismiss** clears the diagram, cancels narration, marks the overlay idle,
  and calls `DELETE /api/magician/v2/chat/sessions/{id}/run` if the flow is
  running.
- **Keep showing** (`overlay-keep-showing-request`) cancels the expiry timer.
- **Replay** (`overlay-replay-request`) clears, restarts animations, re-queues
  narrated steps on the playback-start gated path, and refreshes the lifetime.

The overlay window is click-through. Explain deeper, Keep showing, Dismiss and
Replay stack at the top right, about 40px below the window top. `/draw-overlay`
publishes the exact model-space rectangles of its controls to the host, whose
global mouse monitor consumes clicks inside them (small padding, Retina
mapping) and passes every other click through. Until rects arrive, Dismiss and
Replay fall back to the same stack; Explain deeper has no fallback.

### Narration

Tutor/Copilot narration is separate from chat auto-speak: a revealed shape with
`narration` (or `wait_for_voice` with only a label) is spoken with the selected
TTS provider, voice and rate even when `autoSpeak` is off, one storyboard step
at a time. While it runs the frontend holds tutor audio focus and suppresses
chat auto-speak, task read-outs and manual speak buttons.

Every meaningful draw must carry narration; final chat text only recaps. The
backend enforces this for tutor/copilot `screen-draw`: each payload must yield
at least one step with explicit `tutor_step_label`/`step_label` plus
`narration` **on the `shape_json` group or reveal shape** (top-level tool
metadata does not count).

A step's animation restarts from the real TTS playback-start callback, and
narrated shapes stay queued until it fires, so marks never appear while remote
TTS is still synthesizing. If speech is unavailable or fails, the step reveals
anyway so the tutor cannot hang.

### Geometry and labels

- **Paint order, not the model, decides occlusion.** `sortShapesForDisplay`
  breaks ties (same `z_index` and `reveal_order`) by layer: fills, strokes,
  then text. Explicit `z_index` wins.
- `toRecipeShape` is a **hand-copied field whitelist**: a field missing there
  is silently dropped before the interpreter.
- Whole ellipses are two half-turns (a full-turn arc between identical
  endpoints renders nothing). `arc` may carry `rx`/`ry` (recipe coalesces
  `rx|r|size`). `cone` and `sector` take real measurements (`r`, `h`; sector
  `r` = slant, `size` = base radius) and the recipe derives the rest so
  `θ·l = 2πr` holds by construction. Sampling scales with sweep (a 90° arc is
  24 segments).
- `measureLabelText` measures on a canvas with `.draw-label`'s font and serves
  the interpreter (`renderRecipe({ measureText })`) and the spacing pass;
  `semanticBoxLabelLayout` takes a measurer (default `fontSize * 0.6` per
  char). Formula and handwriting shapes keep the ratio estimate.

## "Explain this deeper" — the correction channel

The tutor guesses where a learner will struggle; the runtime can force that
step to be declared and decomposed (`TutorMilestoneRole`) but cannot know which
step was hard. `deeperRequest.ts` lets the user point at it.

- **Eligibility needs a session**, not a named step (the control still waits
  for visible content and a settled draw/narration boundary). With no step
  named, it deepens "the part you just explained".
- It sends a **new chat turn** instead of changing the run:
  `/tutor/user-action` means "the user did an action Copilot was about to
  automate", and reopening a completed run would break completion invariants. A
  fresh `@tutor` turn works live and in replay.
- The prompt carries `@tutor`, the step's own label (decompose that milestone),
  and the narration already spoken (a second explanation must change
  representation, not volume).

**One sender, two triggers.** On desktop the Tauri host hit-tests the control
and emits `overlay-explain-deeper-request`; in a browser the same handler is a
real `onclick` with `pointer-events: auto` on that one control. The sender
posts `{ text, source_surface }` and treats any non-2xx as failure.

Host controls are inert SVG visuals; the host dispatches a
`DrawOverlayClickAction` against reported rectangles, exact ones before padded
targets so neighbours cannot steal clicks. A new overlay control is a
coordinated Rust + TS change; a chat-turn-level control in `ChatPanel` needs no
Tauri work but loses per-step precision.

On iOS the composer owns the full `@tutor` prompt; each deeper request gets a
new `chat_turn_id`, and the overlay switches correlation before sending and
ignores late events from the replaced turn.

## Voice takeover, idle lock, capture

**Takeover.** The voice websocket sends `tutor.takeover.started` /
`.completed` / `.failed` when a live transcript like `hey tutor …` is routed to
the tutor chat runtime. On `started` the frontend interrupts the active
realtime answer, fences its callbacks, removes any racing answer rendered after
the guided user turn, and clears the speaking flag so narration gets audio
focus. OpenAI direct maps to `response.cancel`; backend-proxied providers clear
local playback and get a backend interrupt. The realtime session stays open.

**Capture.** The Web voice menu lists the shared guided-flow commands.
`Tutor screen` / `Tutor Quick screen` request a fresh server-attested display
capture before takeover; `Tutor`/`Tutor blackboard` (and Quick variants) start
without an attachment; `App Copilot` always captures. Dictation captures before
its auto-send countdown; Live and Hands-free use the authenticated
voice-control bridge. **A capture failure never becomes an ungrounded turn.**
The composer is read-only during capture and rechecks the owning session and
exact draft before send (a navigation or edit keeps the newer draft).
Navigating away discards the unowned staged screenshot through a provenance-
and reference-checked endpoint that sends the capture-time principal and
workspace explicitly (never a capture already committed or queued). Failures always show an error and carry
a backend-announced flag so proxied speech is not duplicated.

The canvas word is a **command-prefix selector**: it must directly follow
`Tutor [Quick]`; later phrases ("my app", "screenshot", "blackboard controls")
are subject matter and cannot authorize capture. This parser is only the
permission-bearing ingress; once it admits a feature and source choice, the
agentic workflow owns teaching, drawing and action planning. Lock state,
capture authorization, provenance, cancellation and retry stay outside the LLM
so they are auditable.

**Idle lock.** Before any typed or dictated Tutor/Copilot send, the composer
tries to activate Chromium Idle Detection from the current gesture. An exact
`screenState: "locked"` stops before capture, attachment change or send; voice
commands speak the unlock instruction, typed drafts stay for resend. Live and
Hands-free send the state in `session.start` and `screen.state`, and their
takeovers carry a cancellation token so a lock cancels an in-flight capture or
run and discards an uncommitted image. Hidden tabs are not "locked";
unsupported or unpermitted browsers stay unknown. No rejected command auto-launches after
unlock. Screen flows recheck the lock after capture and before send (lock-rejected
captures are discarded); a staged
attested image is reused on manual retry.

Attachment sends with text are optimistic in the chat store, so the HUD shows
the text and live activity card at once, even before the global sending state
catches up.

## Region Picker

`/screen-region-picker` is a transparent Tauri-only, client-only (`ssr=false`)
route for the tray/shortcut region capture. It collects a viewport-local drag
rectangle and calls `complete_screen_region_selection` with it and the viewport
size; Rust maps it to the window's physical/global rect and stages
`/screen/capture { mode: "region", region_rect }`. Escape or a too-small
selection cancels without opening the HUD.

## Debug SOTA Tutor Runs

`/debug?mode=sota-tests` Live Concept Tutor and Desktop App Copilot cards run
through the same chat runtime as real HUD prompts: each creates a fresh
screen-scoped session under `screen-debug-sota`, opens the fixture or app,
stages visual context when a browser fixture exists, sends the fixture's
`@tutor` / plain `@copilot` prompt, and shows session and turn ids. The ad hoc
Run Agentic control is separate. Cropped Capture Visual fixtures declare a
same-origin `data-sota-crop-target`; the runner resolves its screen rect and
calls `/screen/capture` with `mode:"region"`, `region_rect` and the debug
`session_id` so the returned `attachment_ids` belong to that session
(non-cropped fixtures use `mode:"screenshot"`). Result cards show attachment
count and crop rect.
