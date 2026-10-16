# Personal Tutor Runtime

Two rails share one runtime (screen capture, chat-runtime tools, `screen-draw`,
rolling run store) with separate product contracts:

- **Personal Tutor** (`@tutor`, `@tutur`, `hey tutor`, `hey tutur`) teaches:
  blackboard mode without a source, screen-overlay concept tutoring with one.
  Never mutates apps.
- **App Copilot** (`@copilot`, `@app-copilot`, `@appcopilot`, `hey copilot`,
  `hey app copilot`) operates apps: always screen-overlay, reversible
  workflows. Each UI-changing step is drawn and narrated, checked once for an
  already-reported user click, then delegated at once; a user action while
  automation is queued or running cancels the delegated child.

An invoke word on any chat thread (HUD or ordinary) injects the runtime
contract, independent of session id.

PromptManager parts are the policy source of truth (Rust keeps fallbacks;
`personal_tutor_chat_instructions_v1.0.0.json` is a compatibility stub):

- `visual_storyboard_runtime_v1.2.0.json`: shared storyboard, narration,
  timing, replay and recap contract.
- `personal_tutor_policy_v1.1.0.json`: Tutor only. Normal lessons teach the
  full shape; Quick and quicker requests stay on the spine.
- `app_copilot_policy_v1.1.0.json`: App Copilot only.

## Runtime Contract

```text
observe -> resolve target -> draw/explain -> optionally act -> observe -> verify -> continue/complete
```

`magician_v2::tutor` owns the in-process run model:

| Type | Owns |
|---|---|
| `TutorRun` | mode, status, step history, pending action, retries, created objects, failure reason |
| `TutorRunStore` | active runs per principal/workspace/chat-session; keeps terminal-run ownership; releases scope atomically on complete or fail |
| `TutorStep` | observe/resolve/draw/action/verify ordering |
| `TutorActionEnvelope` | validates delegated `mac-operator` UI actions before spawn |
| `tutor::copilot_rail` | `TutorProductRail`, `TutorRun::is_app_copilot`, `CopilotActionCheckReceipt` / `record_copilot_action_check`, preemption (`record_user_action_event_and_preempt_pending`, `finalize_user_action_preemption`), demo-and-cleanup guards |

**Preseeded start.** On an explicit invoke `ChatService` starts or reuses the
run before the first model call and injects a synthetic `start_tutor_run`
result (a redundant call is idempotent). A `screen-draw` before any start begins
an implicit run with the same invoke split. Shared chat-runtime tools start
runs, propose steps, record failures/created objects and complete runs.

### App Copilot steps

`@copilot` defaults to `guided_action`; cleanup/demo wording selects
`demo_and_cleanup` (recorded in the synthetic `mode_selection` and
`tutor.run.started`).

- The draw is a preview. The model calls `check_for_copilot_user_action` once,
  without waiting: a user click inside the highlight advances the step without
  delegating (through observation and verification); otherwise a single-use,
  short-lived receipt is minted and the model delegates immediately.
- UI-changing envelopes copy the receipt's storyboard identity and bind to the
  immediately preceding step id/label; skipped, stale, reused or mismatched
  checks are rejected.
- Delegated `click`, `type_text`, `hotkey`, `scroll` use `macos-ui-automation`
  first; shell/CuaDriver is a bounded fallback only after that tool is
  unavailable or failed concretely. `type_text` carries exact literal
  text; missing content is a blocker.
- A user click while delegation is queued or running cancels the child before
  verification is recorded, then resumes the same rail.
- App Copilot and `#quick` identity survive delegation, so continuations never
  become Tutor. A child ending without a valid `TUTOR_ACTION_RESULT` is a
  recoverable action failure.
- Demo-and-cleanup cannot complete until every run-owned object is removed.

### Lesson contract (Tutor)

Tutor runs get a runtime-enforced `lesson_contract` at start and `lesson_progress` after each
draw. Completion counts milestones covered by distinct storyboard ids on
successful narrated drawings, not turns, draw or primitive counts, or speech. The first successful
draw carries the full `tutor_lesson_plan` (stable `milestone_id`, substantive
`objective`); draws use that id as `storyboard_step_id`/`reveal_id`.

Milestone roles: `setup`, `core`, `example`, `check`, `conclusion`. Unroled
milestones satisfy nothing; `conclusion` is not an example or check.
`plan_ready` requires the shape, not just the count.

| Pacing | Steps | Shape |
|---|---|---|
| Highlight, "where is it", summary, single marker | exactly 1 | one mark, even if more is asked |
| Normal, ordinary explanation | floor 3 | `setup`, `core`, `example` |
| Normal, concept / guided solution / why, proof, derivation | floor 5 | `setup`, ≥ 2 `core`, `example`, `check`; extras (setup, examples, checks, conclusions, unroled) may not outnumber `core` |
| Quick (`#quick`), ordinary | 1–2 | smallest complete spine |
| Quick, concept / why / proof / guided solution | 2–6 | ≥ 2 `core`, and `core` ≥ all others combined |

Normal floors are minimums (`maximum_storyboard_steps` is `None`; 64-milestone
safety cap; larger curricula continue as a follow-up lesson). Quick ranges are
hard, even for revisions; the ceiling leaves room for a transformation (before,
cut, mid, after, preserved quantity ≈ five frames). Condensing drops example
and check, never the hard part; the model picks the smallest complete spine,
never splits one idea to inflate it or adds an example or check to lengthen it,
and a deep Quick lesson cannot complete after one draw. Plans
grow by resending the full plan plus new milestones, never deleting or
reordering; a resent `role` is stored, an omitted one kept.

**`pacing_request`** comes from the user's words:

- `quicker` ("briefly", "the gist") applies Quick ranges and the 240-character
  narration cap without `#quick`, but not its history trim, thinking
  suppression, tool narrowing or first-draw limit.
- `exhaustive` ("in more detail", "don't skip") applies normal floors and shape,
  lifts Quick maxima and the 240-character cap, and does not push completion
  once a short spine is covered.

When both appear, exhaustive wins. `@tutor go deeper on the step "…"` is a new
turn about that step, not a whole-lesson override; bare "deeper" or "quick" is not a pacing request.

**Narration.** Normal and exhaustive: a paragraph per step (≤ 1,200
characters), and the final reply walks the milestones. Quick and `quicker`:
≤ 240 characters and a one- or two-sentence recap.

**Coverage.** Only a figure-backed draw counts (not `formula`/`callout`/`label`
alone or a text-only `say`). A group-level id is one step; separately
identified child reveals may cover several. Repeated, case-shifted or unplanned
ids, or a new id on the same narration, add nothing. Coverage survives clears;
completion needs `plan_ready`, full coverage and a draw after the latest clear.
The gate withholds `plan_ready` rather than rejecting (avoids `max_retries`
loops); `validate_tutor_lesson_milestones` rebuilds milestones from trimmed
parts and must thread the role, or the gate becomes unsatisfiable.

It applies to screen-overlay and blackboard Tutor alike, not App Copilot.

### Completion and recovery

- Explicit invokes cannot settle early as text. A response without tool calls
  while milestones remain gets a deterministic continuation (counts, remaining
  objectives, guidance) and a retry; a draw alone does not release the guard.
  Quick gets condensed guidance; App Copilot keeps its own continuation rules.
  Budget ≥ max(planned, floor) + 1; exhaustion records a failed `recover` step
  and fails and releases the run.
- A `record_tutor_step_failure` blocker fails the run after the model explains
  and never counts as coverage; a later successful step restores the guard.
- Auto-complete on final text (and `complete_tutor_run`) needs a successful
  storyboard-bound draw, no pending action and a satisfied contract; narration,
  failure and confirmation records are not terminal evidence.
- Overlay or helper failures fail the run (no stale running tutor).

### Quick path

`#quick` on an explicit `@tutor`/`@copilot` turn still preflights the run but
trims older history, suppresses adaptive thinking and narrows tools so the model
draws first. Tutor Quick sends the full condensed plan and first milestone in one
`screen-draw`: one semantic step, ≤4 drawable primitives, no positive
`delay_ms`, even when `pacing_request` is `exhaustive`. App Copilot Quick keeps
`delegate_to_agent` after the first storyboard-bound draw.

## Rendering and surfaces

- **Activity**: `tutor.*` rows with stable ids in the turn activity log.
  `screen-draw` is a direct renderer call, not a task: desktop validates the
  draw/clear step and commits only after the host overlay succeeds;
  `screen-observation` may run as an internal `Tutor observation` task.
- **iOS overlay**: `source_surface=ios_tutor_overlay` emits `tutor.draw.shape`
  (shape + capture size) on the scoped realtime bus instead of the desktop
  gateway. A `@tutor` composer turn opens it (ported `isTutorInvokeText`;
  `@copilot` is not intercepted): a staged image runs `screen_overlay`, none
  runs `blackboard` in the 2048-clamped model space. Picker and Share
  extension stay `screen_overlay`.
- **Desktop overlay windows** load `/hud` and `/draw-overlay` from the bundle
  (Vite only in debug builds).
- **Draw generations**: a later clear, UI action or batch advances the
  generation and drops delayed reveals and retries, so stale marks never return.
- **Semantic blackboard primitives** (flow nodes, state boxes, stack frames,
  heap objects, memory cells, free-body objects) keep `text`/`formula`/`label`
  on web and iOS, centered and wrapped.

UI details: [personal-tutor-overlay](../unified-ui/personal-tutor-overlay.md).

### Region coordinate contract

A capture staged with `region_rect: {x,y,width,height}` stores crop size and
real screen rect; the model draws in `coordinate_space: "capture"` and the host
maps into the overlay's 2048-max-side space (the desktop region picker
supplies the rect). A capture without `region_rect`
(`screencapture -i` gives no origin) is crop-local: `screen-draw` refuses it
except for clears or groups whose every non-clear primitive uses
`coordinate_space: "screen"`.

## Data-driven drawing primitives

Shape `type`s are JSON recipes (`{type, aliases?, defaults?, draw:[…ops]}`):
built-ins in `MAGICIAN_ROOT_DIR/tutor_primitives/` (seeded at boot from
`magician_data_v3/system/tutor_primitives/`: created if missing, overwritten
only by a higher `version`, never deleted) overlaid by
`scopes/<principal>/<workspace>/tutor_primitives/` by `type`.
`GET /api/magician/v2/tutor/primitives` (workspace-bound bearer) returns the
merged, validated `{primitives, etag}`; iOS and web cache and interpret it, so a new
primitive needs no client rebuild. See
[tutor-primitive-recipes.md](tutor-primitive-recipes.md).

## Cancellation

`POST /api/magician/v2/chat/sessions/{id}/tutor/cancel` is idempotent and
scope-checked, works without the chat row, fails the run (given or default
reason), releases the scope and best-effort cancels a pending `mac-operator`
delegation. Deleting a chat session does the same first.

## Voice

A finalized transcript matching an invoke suppresses the realtime answer and
runs as a normal turn in the voice chat session (takeover, not a mode switch),
narrated through the configured TTS and the overlay audio-focus path.
Admission (explicit command, authorized visual source, client capability, OS
lock state), provenance, cancellation, dedupe and permission checks are
deterministic; teaching and acting are agentic.

Web and Magios Dictation/Hands-free/Live share this utterance-initial grammar:

| Spoken form | Result |
|---|---|
| `Tutor …` or `Tutor blackboard …` | Source-free blackboard Tutor |
| `Tutor Quick blackboard …` | Condensed blackboard Tutor (`#quick`) |
| `Tutor screen …` | Capture the display, then screen-overlay Tutor |
| `Tutor Quick screen …` | Capture, then condensed screen-overlay Tutor |
| `App Copilot …` | Capture, then App Copilot |

`Hey`/`Start`/`Open`/`Launch`/`Use` may precede the name. The selector must
directly follow `Tutor [Quick]`; later words never infer capture or override it.
Tutor defaults to blackboard.

- **Web**: Dictation stages the screenshot before auto-send and rewrites
  `Quick` to `#quick`; Live/Hands-free do the same through the authenticated
  voice bridge with server-owned capture. Missing or denied Screen Recording
  fails closed with a visible or spoken error.
- **Magios** negotiates grammar separately from capture ability and opens
  source-free commands in the native blackboard overlay (the call closes its
  audio graph first; the overlay auto-starts the `@tutor` turn). It never
  captures the server Mac, so screen rows and App Copilot are spoken as
  unsupported.

**Secure-screen gate.** Magios reports protected-data availability in
`session.start` and on lock changes, with a one-shot lock admission token per
presentation; Web reports Chromium Idle Detection `screenState` (details in
[personal-tutor-overlay](../unified-ui/personal-tutor-overlay.md)). A locked
command interrupts the realtime answer, starts nothing and speaks "Please unlock
your screen to use Tutor" (or the App Copilot equivalent); unlock never
auto-starts. In Live and Hands-free a lock edge cancels the takeover's
call-local token, capture and turn, discards uncommitted capture and keeps the
key out of completed-run dedupe. iOS shows rejection guidance as a transient
notice.

**Call ownership.** Live runs takeovers on its owned lane; Hands-free claims a
cancellable child lane each. Ending the call cancels capture and chat work;
concurrent guided starts, or one while the chat tails an active task (only its
own call sentinel is bypassed), are rejected; typed turns queued behind Live
drain after release. Failure speech goes through the server control channel
(proxied providers) or the browser adapter (direct), the envelope marking which
side announced it. The interrupted answer is fenced by provider response id
(Direct Web forwards it at response creation and on captions), so late answers,
tool calls or swallowed failure announcements never leak, even racing
playback.

**Capture discard.** If the session changed by the time capture returns, Web
calls `POST /screen/capture/discard`
([screen-capture-and-ask](screen-capture-and-ask.md)): it removes only an
unreferenced server-attested screen attachment (never ordinary uploads or
referenced, queued or active ones), carries the original principal/workspace,
and is serialized with every active-run claim by a short per-session lock; the
voice owner cleans only a capture reserved by its exact call identity.

## Mode Selection

- `@tutor` never grants app mutation; `@copilot` maps to `guided_action` /
  `demo_and_cleanup` in screen-overlay.
- Concept modes may `verify` against a fresh observation with no pending
  action; `guided_action` and `demo_and_cleanup` require a pending delegated
  action first.

## Concept Tutor Modes

`concept_explainer` (visible concepts, diagrams, formulas, code, frames,
question papers), `guided_solution` (a visible problem step by step),
`concept_demo` (temporary overlays for intuition). Screen observation is the
source of truth: PDFs, images, videos, pages, IDEs and native apps are visible
content first (OCR/DOM/AX/PDF text optional). Concept modes observe, resolve,
draw, say, wait, verify, recover, clear and complete; never click, type,
hotkey, scroll or mutate.

**`screen-draw` V2 envelope.** Grouped overlays (labels, formulas, vectors,
code highlights, stack/heap boxes, subject primitives) beside the older
highlight/arrow/clear contract; each primitive carries its geometry (segments:
start/end; rect-like: `x/y/w/h`; text: anchor; circles: center + radius;
polygon/fill: points or rect). `cursive_text` uses Playwrite USA Traditional (Brush
Script fallback); also `handwriting` and raw `path`/`curve`/`freehand`.

**`VisualEntityMap`.** An observe step may attach stable entity ids with
coordinate space, geometry, confidence and evidence; later steps cite them in
`source_entity_ids`. Ids are valid only for the latest fresh observation; UI
actions and recovery clear the map and force re-observe.

**Subjects stay generic.** No per-subject builders: the model grounds entities
and emits V2 groups (e.g. `area_fill`, `side_label`; `free_body_body`,
`force_arrow`, `component_vector`, `axis`, `unit_label`; `code_highlight`,
`stack_frame`, `heap_object`, `pointer_arrow`, `flow_node`, `flow_edge`); the
runtime validates primitives, groups, finite geometry and freshness.

**Timed reveal.** Fields: `reveal_id`, `reveal_order`,
`tutor_step_label`/`step_label`, `narration`, `delay_ms`, `duration_ms`,
`clear_previous`, `wait_for_voice` (advisory; delays stay bounded),
`persist_until_step` (removed at that step, else `ttl_ms` unless
`persist:true`). The host schedules delayed shapes; the overlay shows the
current label/narration.

**Storyboard.** One per turn, each step with a label and short `narration`.
Tutor/copilot `screen-draw` payloads need at least one explicit storyboard
step inside `shape_json` (top-level metadata does not count). Narration drives
bubbles and speech; the model's final reply uses the returned
`storyboard.recommended_final_response` (deterministic) unless reporting a later
failure, so drawn, spoken and returned text do not drift.

## Delegated Actions

After an empty `check_for_copilot_user_action`, the model delegates to
`mac-operator` with a `tutor_action` envelope (the preview's
`storyboard_step_id`/`storyboard_step_label` and receipt), validated against the
run and injected as a `Personal Tutor Action Envelope` block. It must return:

```text
TUTOR_ACTION_RESULT: {"run_id":"...","status":"succeeded|failed","evidence":"..."}
```

The reader consumes it idempotently, records observe/verify or failure, clears
stale marks and schedules a bounded hidden continuation; a missing sentinel
enters recovery.

## Safety

Explain-only runs cannot mutate the UI. Destructive cleanup is automatic only
for objects created in the same run; preexisting content needs normal
confirmation/HITL.

## Events

Realtime events (HUD turn bubble; kept in `/events`):

- `tutor.run.started`
- `tutor.step.drawing`
- `tutor.step.action_delegated`
- `tutor.step.verified`
- `tutor.step.recovering`
- `tutor.draw.shape` (remote screenshot-overlay payload)
- `tutor.run.completed`

## SOTA Fixtures

`/debug?mode=sota-tests` runs both groups through the real tutor tools in a
fresh `screen-debug-sota` session (not the direct execution runner). Cards
focus the fixture and stage the capture, then post to the returned `#screens`
session with a client `chat_turn_id` (correlation independent of the page's
SSE). Per-card runs send the card's goal; global Run Agentic uses the prompt
box; result cards link to the session and turn.

- **Live Concept Tutor**: `36-live-concept-tutor-math.html`, `37-…-physics.html`,
  `38-…-cs.html`, each with hidden `tutor-ground-truth` metadata (expected
  entities, primitives, acceptance checks) for scoring to consume instead of
  subject-specific scorers.
  Runbook.
- **Desktop App Copilot**: live macOS Notes, Calculator, TextEdit, Music;
  short prompts, the allowlisted app launched via
  `POST /screen/desktop-app/launch` and captured via `/screen/capture`.
  Runbook.
