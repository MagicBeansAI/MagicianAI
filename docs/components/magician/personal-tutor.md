# Personal Tutor Runtime

Two rails share one runtime (screen capture, chat-runtime tools, `screen-draw`,
rolling run store) but have separate product contracts:

- **Personal Tutor** (`@tutor`, `@tutur`, `hey tutor`, `hey tutur`) teaches
  concepts. Source-free turns use blackboard mode; source-backed turns use
  screen-overlay concept tutoring. It must not mutate apps.
- **App Copilot** (`@copilot`, `@app-copilot`, `@appcopilot`, `hey copilot`,
  `hey app copilot`) helps operate apps. Always screen-overlay; may perform
  reversible app workflows. Per UI-changing step it draws and narrates the
  target, checks once for an already-reported user click, then delegates
  automation immediately. A user action while automation is queued or running
  is recorded and preempts the delegated execution when possible.

The invoke words are the gate: once present, the backend injects the runtime
contract on any chat thread (HUD or ordinary), independent of session id.

Prompt policy is composed from PromptManager-managed parts
(`personal_tutor_chat_instructions_v1.0.0.json` is only a compatibility stub):

- `visual_storyboard_runtime_v1.1.0.json` — shared; storyboard, narration,
  timing, replay and final-response recap contract.
- `personal_tutor_policy_v1.0.0.json` — Tutor turns only.
- `app_copilot_policy_v1.1.0.json` — App Copilot turns only.

Rust keeps compact compiled fallbacks so a missing prompt file degrades
gracefully; the prompt store is the source of truth.

## Runtime Contract

```text
observe -> resolve target -> draw/explain -> optionally act -> observe -> verify -> continue/complete
```

`magician_v2::tutor` owns the in-process run model:

- `TutorRun` — mode, status, step history, pending action, retry count,
  session-created objects, terminal failure reason.
- `TutorRunStore` — active runs by principal/workspace/chat-session scope;
  keeps historical ownership for terminal runs; releases the active scope
  atomically on complete or fail.
- `TutorStep` — validates observe/resolve/draw/action/verify ordering.
- `TutorActionEnvelope` — validates delegated `mac-operator` UI actions before
  spawn.
- `tutor::copilot_rail` — App Copilot product logic: `TutorProductRail`,
  `TutorRun::is_app_copilot`, `CopilotActionCheckReceipt` and
  `record_copilot_action_check`, user-action preemption
  (`record_user_action_event_and_preempt_pending`,
  `finalize_user_action_preemption`), and demo-and-cleanup guards.

**Preseeded start.** For an explicit invoke, `ChatService` starts or reuses the
scoped run before the first model call and injects a synthetic
`start_tutor_run` result, so the model continues an active runtime instead of
deciding whether to start one. A redundant `start_tutor_run` is idempotent. A
`screen-draw` before any start begins an implicit run with the same invoke
split. The chat runtime exposes shared tools to start runs, propose steps,
record failures/created objects and complete runs.

### App Copilot steps

`@copilot` defaults to `guided_action`; cleanup/demo wording selects
`demo_and_cleanup` (recorded in the synthetic `mode_selection` and the
`tutor.run.started` note).

- The draw step is only a preview. The model then calls
  `check_for_copilot_user_action` once, without waiting. If the user already
  clicked inside the highlighted region, the runtime advances that step through
  observation and verification without delegating. Otherwise it mints a
  single-use, short-lived receipt and the model delegates immediately.
- UI-changing envelopes must copy the receipt's storyboard identity and bind to
  the immediately preceding storyboard step id/label; skipped, stale, reused or
  mismatched checks are rejected.
- Delegated `click`, `type_text`, `hotkey`, `scroll` instruct `mac-operator` to
  use `macos-ui-automation` first; shell/CuaDriver is a bounded fallback only
  after that tool is unavailable or failed concretely. `type_text` carries the
  exact literal text; missing real content is surfaced as a blocker.
- If the user clicks while delegation is queued or running, the backend cancels
  the child before recording verification and resumes the same rail.
- App Copilot product and `#quick` identity survive delegated task boundaries,
  so hidden continuations cannot be reinterpreted as Tutor. A terminal child
  missing a valid `TUTOR_ACTION_RESULT` becomes a recoverable action failure.
- Demo-and-cleanup runs cannot complete until every run-owned created object has
  been removed.

### Lesson contract (Tutor)

Tutor runs get a runtime-enforced `lesson_contract` at start and live
`lesson_progress` after each draw. Completion is based on a milestone plan plus
distinct semantic storyboard ids on successful narrated drawings — not LLM
turns, draw count, primitive count or speech. The model sends the complete
`tutor_lesson_plan` with the first successful draw; each entry has a stable
`milestone_id` and a substantive `objective`, and the draw uses that id as its
`storyboard_step_id`/`reveal_id`.

**Normal pacing** — minimum floors, not fixed lengths: summary/overview/single
marker/highlight → 1 semantic step; ordinary explanation → 2; concept, guided
solution, or why/logic/proof/derivation → ≥3. No maximum
(`maximum_storyboard_steps` is `None`) beyond a 64-milestone safety cap; larger
curricula continue as a follow-up lesson. A plan may grow by resending the full
plan plus the new milestone; existing scope cannot be deleted or reordered.

**Quick pacing** (`#quick`) — a runtime-enforced range that revisions cannot
exceed: focused marker/highlight/summary → exactly 1; ordinary → 1–2;
concept/why/proof/guided solution → 2–6. The ceiling must leave room for a
transformation (before, cut, mid, after, preserved quantity ≈ five frames):
condensing removes setup and worked examples, never the hard part. The model
picks the smallest complete spine and must not split one idea to inflate it; a
deep Quick lesson cannot complete after one draw.

**Depth goes where the difficulty is.** Each milestone has a `role`: `setup`,
`core` or `conclusion`. A deep lesson is not `plan_ready` until ≥2 milestones
are `core` and `core` ≥ setup + conclusion.

- The gate withholds `plan_ready` rather than rejecting, avoiding a retry loop
  against `max_retries`. Revisions may add roles (scope preservation compares
  only `milestone_id` and `objective`).
- An unroled milestone counts as scaffolding, never `core`.
- `validate_tutor_lesson_milestones` rebuilds milestones from trimmed parts and
  must thread the role explicitly, or the gate becomes unsatisfiable.

**Coverage rules.**

- Only a **figure-backed** draw covers a milestone; `formula`/`callout`/`label`
  only, or a text-only `say`, is prose and does not count.
- Several primitives sharing one group-level id are one step; independently
  identified child reveals may cover several steps in one call.
- Repeating or case-shifting an id, a new id on the same normalized narration,
  or an unplanned id adds no coverage.
- Coverage survives clears, but completion needs a successful storyboard-bound
  draw after the most recent clear.
- `plan_ready` and full milestone coverage are required before completion.

These contracts apply identically to screen-overlay and blackboard Tutor. App
Copilot has its own action policy and no lesson contract.

### Completion and recovery

- Explicit invokes may not settle as text-only answers early. If a model
  response has no tool calls while milestones remain uncovered, `ChatService`
  appends a deterministic continuation (completed/planned counts, remaining
  objectives, plan guidance) and retries the turn; a successful draw alone does
  not release the guard. Quick gets condensed guidance; App Copilot keeps its
  own action continuation rules. Recovery budget is at least
  max(planned count, floor) + 1. Exhaustion records a failed `recover` step and
  fails/releases the run.
- A `record_tutor_step_failure` blocker terminalizes the run as failed once the
  model returns its explanation; it never counts as coverage. A later successful
  step is recovery evidence and restores the guard.
- On final text, `ChatService` auto-completes a running run only with a
  successful storyboard-bound draw, no pending action, and a satisfied lesson
  contract. `complete_tutor_run` enforces the same contract. Narration,
  failure/recovery and confirmation records are not terminal evidence.
- Overlay or helper failures record failed steps and fail the run so the
  activity card never shows a stale running tutor.

### Quick path

`#quick` on an explicit `@tutor`/`@copilot` turn opts into a fast first-overlay
path for that turn: the run is still preflighted, but older in-memory history is
trimmed, adaptive thinking escalation is suppressed, and tools are narrowed so
the model draws first. Tutor Quick sends the full condensed plan and its first
milestone in one `screen-draw` call. That first draw is limited to one semantic
step, ≤4 drawable primitives and no positive `delay_ms`; every Quick narration
≤240 characters. App Copilot Quick keeps `delegate_to_agent` after the first
storyboard-bound draw.

## Rendering and surfaces

- **Activity**: tutor events appear in the chat turn activity log as `tutor.*`
  rows with stable ids. `screen-draw` is a direct renderer call, not a task.
  Desktop validates the draw/clear step, calls the host overlay, and commits
  only after it succeeds. `screen-observation` may run as an internal task,
  labeled `Tutor observation` in status projection.
- **iOS overlay**: a turn with `source_surface=ios_tutor_overlay` emits
  `tutor.draw.shape` (shape + capture dimensions) on the scoped realtime bus
  instead of the desktop gateway, then commits. A `@tutor` composer turn opens
  the overlay (ported `isTutorInvokeText`; `@copilot` is not intercepted). A
  staged image runs `screen_overlay`; no image runs `blackboard` in the
  2048-clamped model coordinate space. The picker and Share extension stay
  `screen_overlay`.
- **Desktop overlay windows** use the Vite dev server only in debug builds;
  packaged builds load `/hud` and `/draw-overlay` via the bundled app protocol.
- **Draw generations**: the desktop host drops delayed reveals and retry emits
  once a later clear, UI-changing action or newer batch advances the
  generation, so stale marks never reappear over changed app state.
- **Semantic blackboard primitives** (flow nodes, state boxes, stack frames,
  heap objects, memory cells, free-body objects) keep `text`/`formula`/`label`
  across web and iOS; labels render centered and wrap inside the shape.

### Region coordinate contract

A region staged with `region_rect: {x,y,width,height}` stores both crop size and
the real screen rect; the model is told to use `coordinate_space: "capture"`,
and the host maps crop-local points into the overlay's 2048-max-side space. The
desktop region shortcut uses a Magician-owned picker that supplies the rect. A
capture without `region_rect` (native `screencapture -i` returns no origin) is
marked crop-local and `screen-draw` blocks drawing from it rather than guessing;
clears are allowed, and drawing is allowed only with explicit
`coordinate_space: "screen"`, which every non-clear primitive in a group must
resolve to.

## Data-driven drawing primitives

Shape `type`s are not hardcoded in clients. Each primitive is a JSON recipe
(`{type, aliases?, defaults?, draw:[…ops]}`) loaded from
`MAGICIAN_ROOT_DIR/tutor_primitives/` (built-ins, seeded from
`magician_data_v3/system/tutor_primitives/`) overlaid by
`scopes/<principal>/<workspace>/tutor_primitives/` (override by `type`).
`GET /api/magician/v2/tutor/primitives` (workspace-bound bearer) returns the
merged, validated `{primitives, etag}`; iOS and web cache it and render through
a generic interpreter, so a new primitive is a new file with no client rebuild.
Authoring reference: [tutor-primitive-recipes.md](tutor-primitive-recipes.md).

## Cancellation

`POST /api/magician/v2/chat/sessions/{id}/tutor/cancel` is idempotent and
scope-checked; it works even if the chat row is gone, marks the run failed
(supplied or default reason), releases the scope, and best-effort cancels a
pending delegated `mac-operator` execution. Deleting a chat session runs the
same cleanup first.

## Voice

Live voice uses the same runtime. When a finalized transcript matches an invoke,
the voice control actor suppresses the realtime answer for that utterance and
submits the text through the active voice chat session as a normal turn — a
deterministic command takeover, not a mode switch. Narration uses the configured
TTS provider and the overlay audio-focus path.

The boundary is deliberately hybrid: admission (explicit command present, visual
source authorized, client capability, OS lock state) is a deterministic control
plane; after admission Tutor and App Copilot are agentic. Screen lock,
provenance, cancellation, dedupe and permission checks stay deterministic
because they are enforcement, not reasoning.

Web Dictation/Hands-free/Live and Magios Dictation/Hands-free/Live recognize
the same utterance-initial grammar:

| Spoken form | Result |
|---|---|
| `Tutor …` or `Tutor blackboard …` | Source-free blackboard Tutor |
| `Tutor Quick blackboard …` | Condensed blackboard Tutor with canonical `#quick` semantics |
| `Tutor screen …` | Capture the current display, then start screen-overlay Tutor against that image |
| `Tutor Quick screen …` | Capture the display, then start the condensed screen-overlay Tutor path |
| `App Copilot …` | Capture the display, then start the action-capable App Copilot lane |

`Hey`/`Start`/`Open`/`Launch`/`Use` may precede the product name. The source
selector must immediately follow `Tutor [Quick]`; capture is never inferred from
later wording, and topic words cannot override an explicit selector. Tutor
defaults to blackboard.

- **Web**: Dictation stages the screenshot into the session before auto-send and
  rewrites `Quick` to `#quick`; Live/Hands-free do the same via the
  authenticated voice bridge with server-owned capture. Missing or denied Screen
  Recording fails closed with a visible or spoken error.
- **Magios**: negotiates the grammar separately from capture ability. Source-free
  commands hand off to the native blackboard overlay (the call closes its audio
  graph first; the overlay auto-starts the `@tutor` turn). iOS never captures
  the server Mac: screen rows and App Copilot are spoken as unsupported.

**Secure-screen gate.** Magios reports protected-data availability in
`session.start` and on lock/unlock; Web reports Chromium Idle Detection
`screenState` after its permission gate (tab visibility is not a lock signal;
unsupported/unpermitted ⇒ unknown, not guessed). A locked command interrupts the
realtime answer, starts nothing, and speaks "Please unlock your screen to use
Tutor" (or the App Copilot equivalent). Unlock never queues an automatic start.
Web rechecks after async capture and reuses a staged attested image on retry.
Magios gives each voice presentation a one-shot lock admission token that a lock
invalidates. For Live and Hands-free, each takeover owns a call-local
cancellation token: a later lock edge cancels capture and the owned turn,
discards uncommitted capture, and the key is not added to completed-run dedupe.
iOS shows rejection guidance as a transient notice.

**Call ownership.** The call's active chat token bounds a guided takeover: Live
executes through its owned lane; Hands-free claims a cancellable child lane per
takeover. Ending the call cancels capture and chat work; concurrent guided
starts are rejected; typed turns queued behind a Live call drain after release.
A guided voice turn is rejected while the chat tails an active task (it bypasses
only its own call sentinel). Failure speech goes through the server control
channel (proxied providers) or the browser adapter (direct), with the envelope
marking which side announced it. The interrupted answer is fenced by provider
response id (Direct Web forwards identity at response creation and on captions),
so late cancelled answers, tool calls and swallowed failure announcements cannot
leak through even when takeover races playback.

**Capture discard.** Session ownership is rechecked when capture returns; if the
user navigated away, Web calls `POST /screen/capture/discard`, which removes
only an unreferenced server-attested screen attachment (never ordinary uploads,
referenced, queued or active ones). Capture and cleanup carry the original
principal/workspace. A short per-session admission lock serializes deletion with
every active-run claim; the voice owner may clean only a capture reserved by its
exact call identity.

## Mode Selection

- `@tutor` never grants app mutation: blackboard without a visual source,
  screen-overlay concept tutoring with one.
- `@copilot` maps to `guided_action` / `demo_and_cleanup` in screen-overlay
  mode: observe, draw the next step, then act only if the user has not.
- Concept modes may record `verify` against a fresh observation with no pending
  action; `guided_action` and `demo_and_cleanup` require a pending delegated
  action before `verify`.

## Concept Tutor Modes

- `concept_explainer` — visible concepts, diagrams, formulas, code, paused
  frames, question papers.
- `guided_solution` — a visible problem step by step.
- `concept_demo` — temporary overlays for visual intuition.

Screen observation is the source of truth; PDFs, images, videos, pages, IDEs and
native apps are visible content first (OCR/DOM/AX/PDF text may help but are not
required rails). Concept modes can observe, resolve, draw, say, wait, verify,
recover, clear and complete, but never click, type, hotkey, scroll or mutate.

**`screen-draw` V2 envelope.** Grouped overlays with labels, formulas, geometry
markers, vectors, code highlights, stack/heap boxes and other subject
primitives, alongside the older highlight/arrow/clear contract. Each primitive
must carry its geometry (segments: start/end; rect-like: `x/y/w/h`; text: anchor;
circles: center + radius; polygon/fill: points or rect). `cursive_text` renders
with Playwrite USA Traditional (Brush Script fallback); `handwriting` for casual
labels; raw `path`/`curve`/`freehand` for strokes.

**`VisualEntityMap`.** An observe step may attach a per-observation map with
stable entity ids, coordinate-space metadata, geometry, confidence and evidence.
Later steps cite ids via `source_entity_ids`. Ids are valid only for the latest
fresh observation; UI-changing actions and recovery clear the map and force
re-observe.

**Subjects stay generic.** No per-concept overlay builders for math, physics or
CS: the model grounds entities and emits V2 groups (e.g. `area_fill`,
`side_label`; `free_body_body`, `force_arrow`, `component_vector`, `axis`,
`unit_label`; `code_highlight`, `stack_frame`, `heap_object`, `pointer_arrow`,
`flow_node`, `flow_edge`); the runtime validates primitives, group structure,
finite geometry and freshness.

**Timed reveal.** Primitives and groups may carry `reveal_id`, `reveal_order`,
`tutor_step_label`/`step_label`, `narration`, `delay_ms`, `duration_ms`,
`clear_previous`, `wait_for_voice`, `persist_until_step`. The host schedules
delayed shapes; the overlay shows the current label/narration. A
`persist_until_step` mark is removed when that step arrives, and still falls back
to `ttl_ms` unless `persist:true`. `wait_for_voice` is advisory; the runtime uses
bounded delays.

**Storyboard.** One cohesive storyboard per turn, of one or more semantic reveal
steps, each with a `tutor_step_label`/`step_label` and a short `narration`. The
chat runtime rejects tutor/copilot `screen-draw` payloads without at least one
explicit storyboard step inside `shape_json` (top-level tool metadata does not
count). Narration is the source of truth for overlay bubbles and speech; the
final chat response is a recap. Successful results include a deterministic
`storyboard.recommended_final_response`, which the model uses unless it must
report a later failure — so drawn, spoken and returned text do not drift.

## Delegated Actions

Guided-action prompts delegate UI mutations to `mac-operator` with a
`tutor_action` envelope after `check_for_copilot_user_action` finds no user
action. The envelope carries the `storyboard_step_id`/`storyboard_step_label` of
the immediately preceding successful preview and matches its fresh receipt; the
chat runtime validates both against the active run before injecting a
`Personal Tutor Action Envelope` block into the delegated task.

The delegated task must return:

```text
TUTOR_ACTION_RESULT: {"run_id":"...","status":"succeeded|failed","evidence":"..."}
```

The task-output reader consumes it idempotently, records post-action
observe/verify or failure steps, clears stale marks, and schedules a bounded
hidden continuation turn so the tutor can continue or complete without a new
prompt. A terminal execution without a valid sentinel fails explicitly and
enters recovery.

## Safety

Explain-only runs cannot mutate the UI. Destructive cleanup is automatic only
for objects created inside the same tutor run; preexisting content requires
normal confirmation/HITL.

## Events

Tutor progress emits `tutor.*` realtime events such as:

- `tutor.run.started`
- `tutor.step.drawing`
- `tutor.step.action_delegated`
- `tutor.step.verified`
- `tutor.step.recovering`
- `tutor.draw.shape` (remote screenshot-overlay renderer payload)
- `tutor.run.completed`

The HUD renders these in the chat turn progress bubble; `/events` keeps them for
debugging.

## SOTA Fixtures

The debug SOTA tab (`/debug?mode=sota-tests`) has two groups, both run through a
fresh screen-scoped chat session (`screen-debug-sota`) so they exercise the real
chat-runtime tutor tools rather than the direct execution runner:

- **Live Concept Tutor** — static fixtures `36-live-concept-tutor-math.html`,
  `37-…-physics.html`, `38-…-cs.html`, each with hidden `tutor-ground-truth`
  metadata (expected entities, primitives, acceptance checks) that scoring should
  consume instead of subject-specific scorers. Runbook:
  Live Concept Tutor SOTA verification.
- **Desktop App Copilot** — live macOS cases (Notes, Calculator, TextEdit,
  Music). The runner launches the allowlisted app via
  `POST /screen/desktop-app/launch` and stages a screenshot via
  `/screen/capture`. Prompts stay short; the runtime owns the
  observe/draw/act/verify discipline. Runbook:
  Desktop App Copilot SOTA verification.

Browser cards focus the fixture and stage the capture before sending, then post
to the returned `#screens` session with a client-supplied `chat_turn_id` so long
turns keep event correlation independent of the debug page's SSE stream. Per-card
runs dispatch the card's own goal; the global Run Agentic control uses the prompt
box. Result cards link to the chat session and turn.
