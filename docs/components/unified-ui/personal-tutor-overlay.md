# Personal Tutor And App Copilot Unified UI Surfaces

Unified UI has Personal Tutor/App Copilot surfaces:

- `/hud` for the chat HUD and screen-capture ask flow.
- `/draw-overlay` for the inert transparent drawing layer.
- `/screen-region-picker` for desktop-owned rect selection before a region
  capture is staged.

The native iOS companion does not load these routes; its static screenshot
Tutor view consumes the additive `tutor.draw.shape` realtime event (kept in the
generated event-taxonomy mirror so drift checks agree with Rust; desktop
`/draw-overlay` delivery still goes through the Tauri host). iOS mirrors the
playback lifecycle: drawing starts from the real TTS playback-start callback,
narration and drawing are serialized, Replay repeats narration, Keep
Showing/expiry and Ask Again are exposed, and Dismiss of an active run calls the
scoped Tutor cancel endpoint. Explain Deeper appears once the latest live step
has finished drawing and speaking (also after completion and during replay),
never while a step is moving or narrating.

## HUD Progress

`ChatTurnProgress.svelte` renders `tutor.*` realtime events as compact progress
rows (observing, drawing, delegating, verifying, recovering, completing). The
durable per-turn request activity card renders the same lifecycle from the
persisted chat-turn event projection, so it survives refresh. Helper tasks may
produce normal task status cards, with a backend display label (e.g. `Tutor
observation`) instead of raw helper pack names. On `tutor.run.completed` or
`tutor.run.failed`, both surfaces mark any still-running tutor step rows for that
run terminal, so stale rows cannot keep the `Steps` spinner alive. Rows are
display-only; user gates use the normal Attention/HITL surfaces.

The `/hud` composer stays editable in its compact floating layout; focusing does
not expand the HUD (an explicit expand/collapse icon does). The HUD passes
`fireAndForget="tutor"` to `ChatPanel`, so tutor invokes (`@tutor`, `@tutur`,
`hey tutor`) and App Copilot invokes (`@copilot`, `@app-copilot`, `hey copilot`)
hide the HUD immediately after submit while the turn continues in the normal
chat runtime; other HUD asks stay open.

**App Copilot** (`@copilot`) is the hybrid app-action rail: the overlay observes,
highlights, labels, narrates and shows what to do next. A native pass-through
click watcher reports clicks inside the active highlight. The runtime does not
pause before automation: it checks once for an already-reported click, then
delegates the storyboard step to `mac-operator` immediately; a user click while
automation is queued or running is recorded and the delegated execution is
cancelled when possible.

## Draw Overlay

`/draw-overlay` subscribes to `overlay-draw-shape` from the Tauri host and also
drains `take_overlay_draw_shapes` on mount — the host can emit the first draw
before the webview listener exists. The latest status is pulled on mount the
same way.

The renderer supports V1 (`highlight`, `arrow`, `clear`) and the V2 educational
primitive envelope used by Live Concept Tutor (labels, callouts, lines, vectors,
rectangles, polygons, circles, angle markers, side labels, square-on-segment,
area fills, formulas, handwriting/cursive text, code highlights, stack/heap
boxes, pointer arrows, flow edges, timeline ticks, memory cells). Instructional
`cursive_text` uses bundled Playwrite USA Traditional (macOS Brush Script
fallback); casual `handwriting` keeps the decorative stack; review route
`/debug/tutor-cursive`. Behind `USE_RECIPE_INTERPRETER`, `src/lib/tutor/` fetches
the shared recipe set from `GET /api/magician/v2/tutor/primitives` and a generic
interpreter (mirroring iOS) renders any recipe with native-path geometry — see
[tutor-primitive-recipes.md](../magician/tutor-primitive-recipes.md).

It maps the model's screen-coordinate space onto a full-window SVG layer, orders
marks by `z_index`, animates strokes, and auto-expires non-persistent diagrams
**as one overlay**. Timed reveals may carry `persist_until_step` (kept until the
named later step, still bounded by `ttl_ms` unless `persist:true`).

While marks are visible the overlay dims the screen and draws a subtle
pointer-transparent aurora frame. Rendering uses slower animations and a neon
high-contrast palette (semantic colours and dark hex values mapped to neon
equivalents, matched glow, dark text outlines); very short `duration_ms` values
are bounded upward. A same-row spacing pass keeps adjacent labels apart.

**Working state.** When a HUD turn includes a tutor/copilot invoke word,
`ChatPanel` asks the host to show the overlay in `working` status before the
first draw: dim screen plus a bottom-right `Please wait, creating explanation`
bubble. No frontend timeout: it clears when shapes arrive, an error/cancel marks
the status idle, or the user dismisses. A successful assistant response does
**not** mark it idle (storyboard playback and narration can outlast the chat
text). The status carries the owning chat session id (kept until idle so Dismiss
can cancel) and the active rail (`tutor` or `copilot`). For App Copilot screen
runs, spotlight cutouts are derived from visible `highlight`, `rect`,
`spotlight`, `mask`, `code_highlight` and `polygon` shapes (dim outside the
target); Personal Tutor keeps the normal dim, blackboard mode its stronger board.

### TTL, Keep showing, Replay, Dismiss

Non-persistent diagrams stay for `DEFAULT_SHAPE_TTL_MS = 60000` after the later
of draw-animation completion and speech completion; marks never expire
individually. On expiry, marks, controls, dim layer and Dismiss clear together
unless **Keep showing** was clicked (then everything stays until explicit
dismiss/clear).

Dismiss appears as soon as the flow enters `working`. Keep showing and Replay
appear once the batch finishes animating and the TTL hold begins — for Personal
Tutor as soon as narration/drawing is done; App Copilot keeps the stricter gate
of `tutor.run.completed` / `tutor.run.failed`.

- **Dismiss** clears the diagram, cancels narration, marks the overlay idle, and
  calls `DELETE /api/magician/v2/chat/sessions/{id}/run` if the flow is running.
- **Keep showing** (`overlay-keep-showing-request`) cancels the expiry timer.
- **Replay** (`overlay-replay-request`) clears, restarts animations, re-queues
  narrated steps on the playback-start gated path, and refreshes the lifetime.

The overlay window is click-through. `/draw-overlay` publishes the exact
model-space rectangles of its controls to the host; the host's global mouse
monitor consumes clicks inside them (small native padding, physical-pixel and
logical-point mapping for Retina) and passes every other click through.

### Narration

Tutor/Copilot narration is separate from chat auto-speak: a revealed shape with
`narration` (or `wait_for_voice` with only a label) is spoken through the
selected TTS provider/voice/rate even when `autoSpeak` is off, serialized per
storyboard step. While narration runs, the frontend holds tutor audio focus:
chat auto-speak, task read-outs and manual speak buttons are suppressed.

Every meaningful draw/reveal must carry narration; final chat text only recaps
the storyboard. The backend enforces this for tutor/copilot `screen-draw` before
the overlay sees it: each payload must yield at least one step with explicit
`tutor_step_label`/`step_label` plus `narration` **on the `shape_json` group or
reveal shape** (top-level tool metadata does not count).

A step's draw animation restarts from the real TTS playback-start callback, and
narrated shapes stay queued until it fires, so marks never appear while remote
TTS is still synthesizing; if speech is unavailable or errors, the step reveals
anyway so the tutor cannot hang.

### Geometry and labels

- **Occlusion is decided by paint order, not the model.** `sortShapesForDisplay`
  breaks ties (same `z_index` and `reveal_order`) by layer: fills, strokes, then
  text. Explicit `z_index` wins.
- `toRecipeShape` is a **hand-copied field whitelist** — a field missing there is
  silently dropped on the way into the interpreter.
- Whole ellipses are two half-turns (a full-turn arc between identical endpoints
  renders nothing). `arc` may carry `rx`/`ry` (recipe coalesces `rx|r|size`).
  `cone` and `sector` take real measurements (`r`, `h`; sector `r` = slant,
  `size` = base radius) and the recipe derives base ellipse, slants, apex and
  sweep so `θ·l = 2πr` holds by construction. Sampling scales with sweep (a 90°
  arc is 24 segments).
- `measureLabelText` measures on a canvas using `.draw-label`'s font and serves
  both the interpreter (`renderRecipe({ measureText })`) and the spacing pass;
  `semanticBoxLabelLayout` takes a measurer (default `fontSize * 0.6` per char).
  Formula and handwriting shapes keep the ratio estimate (other fonts). The
  spacing pass's displacement clamps are separate from width.

## "Explain this deeper" — the correction channel

The tutor guesses where a learner will struggle; the runtime can force that step
to be declared and decomposed (`TutorMilestoneRole`) but cannot know which step
was actually hard. `deeperRequest.ts` lets the user point at it.

- **Eligibility requires a session**, not a nameable step (the control still
  waits for visible content and a settled draw/narration boundary). With no step
  named, it deepens "the part you just explained".
- It composes a **new chat turn** rather than mutating a run: `/tutor/user-action`
  means "the user performed an action Copilot was about to automate", and
  reopening a completed run would unwind completion invariants. A fresh `@tutor`
  turn reuses the full pipeline and works live and in replay.
- The prompt carries `@tutor` (the rail is chosen by invoke word), the step's own
  label (decompose that milestone, not re-teach), and the narration already
  spoken (a second explanation must change representation, not volume).

**One sender, two triggers.** On desktop the Tauri host hit-tests the control and
emits `overlay-explain-deeper-request`; in a browser the same handler is a real
`onclick` with `pointer-events: auto` on that one control. The sender posts
`{ text, source_surface }` and treats any non-2xx as failure.

Host controls are inert SVG visuals; the host dispatches a
`DrawOverlayClickAction` against reported rectangles. Explain Deeper is dynamic,
so there is no constant fallback — no reported rectangle, no native action.
Exact rectangle ownership resolves before padded targets so neighbours cannot
steal clicks. A new overlay control is a coordinated Rust + TS change; a
chat-turn-level control in `ChatPanel` needs no Tauri work at the cost of
per-step precision.

On iOS the composer owns the full `@tutor` prompt; each deeper request gets a new
`chat_turn_id`, and the overlay switches correlation before sending and ignores
late events from the replaced turn.

## Voice takeover, idle lock, capture

**Takeover.** The voice websocket sends `tutor.takeover.started` / `.completed` /
`.failed` when a live transcript like `hey tutor …` is routed to the tutor chat
runtime. On `started` the frontend asks the active realtime adapter to interrupt
the in-progress answer, fences its callbacks, removes any racing answer rendered
after the guided user turn, and clears the speaking flag so tutor narration gets
audio focus. Provider-neutral: OpenAI direct maps to `response.cancel`;
backend-proxied providers clear local playback and get a backend interrupt. The
realtime session stays open.

**Capture.** The Web voice menu advertises the shared guided-flow commands.
`Tutor screen` / `Tutor Quick screen` request a fresh server-attested display
capture before takeover; `Tutor`/`Tutor blackboard` (and Quick variants) start
without an attachment (blackboard canvas); `App Copilot` always captures.
Dictation captures before its auto-send countdown; Live and Hands-free use the
authenticated voice-control bridge. **A capture failure never degrades into an
ungrounded turn.** The composer is read-only during capture and revalidates the
owning session and exact draft before send (a navigation or edit preserves the
newer draft). Navigation asks the backend to discard the now-unowned staged
screenshot via a provenance- and reference-checked endpoint that sends the
capture-time principal/workspace explicitly; it cannot delete a capture already
committed or queued. Failure envelopes always show a visible error and carry a
backend-announced flag so proxied speech is not duplicated.

The canvas word is a **command-prefix selector**: it must immediately follow
`Tutor [Quick]`; later phrases ("my app", "screenshot", "blackboard controls")
are subject matter and cannot authorize capture. This parser is only the
permission-bearing ingress — once it admits a structured feature/source choice,
the agentic Tutor/Copilot workflow owns interpretation, teaching, drawing and
action planning. Lock state, capture authorization, provenance, cancellation and
retry stay outside the LLM so they are auditable.

**Idle lock.** Before any typed or dictated Tutor/Copilot send the composer tries
to activate Chromium Idle Detection from the current gesture (guarded before its
first await, upgraded from a passive probe on a real gesture, invalidated on
chat-scope change). An exact `screenState: "locked"` stops before capture,
attachment mutation or send. Voice-origin commands speak the feature-specific
unlock instruction and clear the command; typed drafts remain for manual resend.
Live and Hands-free send the state in `session.start` and `screen.state` so the
backend can interrupt and announce the same message; their takeovers carry a
cancellation token so a lock transition cancels an in-flight server capture/run
and discards an uncommitted image. Hidden tabs are not "locked"; unsupported or
unpermitted browsers stay unknown. No rejected command auto-launches after
unlock. Screen flows re-check lock after capture and before send (lock-rejected
captures are discarded); a staged attested image is reused on manual retry
rather than recaptured.

Text-bearing attachment sends are optimistic in the chat store, so the HUD shows
the submitted text and the live activity card immediately; for the latest
no-response user turn the panel keeps that card live even before global sending
state catches up.

## Region Picker

`/screen-region-picker` is a transparent Tauri-only, client-only (`ssr=false`)
route for the tray/shortcut region-capture flow. It collects a viewport-local
drag rectangle and calls `complete_screen_region_selection` with it plus viewport
dimensions; Rust maps it to the window's physical/global rect and stages
`/screen/capture { mode: "region", region_rect }`. Escape or a too-small
selection cancels without opening the HUD.

## Debug SOTA Tutor Runs

`/debug?mode=sota-tests` Live Concept Tutor and Desktop App Copilot cards run
through the same chat runtime as real HUD prompts: each creates a fresh
screen-scoped session under `screen-debug-sota`, opens the fixture or app, stages
visual context into that session when a browser fixture exists, sends the
fixture's `@tutor` / plain `@copilot` prompt, and shows session and turn ids. The
ad hoc Run Agentic control is separate. Cropped Capture Visual fixtures declare a
same-origin `data-sota-crop-target`; the runner resolves its screen rect and calls
`/screen/capture` with `mode:"region"`, `region_rect` and the debug `session_id`
so returned `attachment_ids` belong to that session (non-cropped fixtures use
`mode:"screenshot"`). Result cards show attachment count and crop rect.
