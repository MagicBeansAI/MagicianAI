# VibeDev "Living Studio" cockpit

The `/vibe` surface is the coding cockpit. It is a `rail | conversation | stage`
layout where **the conversation is the spine** — you watch the coding
engine think and act in real time, diffs thread inline, and there is no setup
ceremony. It renders the magician backend's already-shipped `coding.*` event
stream (the F2 projection) over the R3 preview proxy + the vibedev API.
At viewport widths of 1280px or less, the project rail stays on the left and
spans the cockpit height; the stage moves beneath the conversation on the
right. Each panel keeps its own scroll area, so the project chooser remains
visible while browsing the stage. On short screens, the page itself scrolls
to keep the conversation and stage controls from overlapping. Below 800px,
the rail narrows while remaining a sidebar.
The new-build composer uses the primary personal agent's display name in its
placeholder, with a neutral prompt until that agent is available. The intro
copy stays neutral rather than naming a fixed agent.
A Ready Codex install appears as a named profile in the composer picker when
`coding.codex.enabled` is on; a Ready Grok Build CLI install appears as
**Grok** (`grok-default`) when `coding.grok.enabled` is on; a Ready
Claude Code install appears as **Claude** (`claude-default`) when
`coding.claude.enabled` is on, exactly one `claude` binary is found, the
CLI meets the frozen minimum, Max/OAuth is signed in, and isolation is
attested; a Ready Antigravity CLI install appears as **Antigravity**
(`agy-default`) when `coding.agy.enabled` is on, exactly one `agy`
binary is found, the CLI meets the frozen minimum, OAuth is signed in
(Magician's Gemini key is not inherited unless `coding.agy.use_api_key`
is true), and isolation is attested.
Pi stays the default. Auto is an opt-in last row and never the default;
when Auto is chosen, the coordinator may pick Grok, Claude, or
Antigravity only if that profile is Ready. Non-Ready Grok/Claude/Agy stay
on `GET /coding/profiles` with `selectable: false` and an operator
`reason` (no paths or email).
The picker locks while the in-view run is non-terminal. Chat,
composer-voice, and hands-free voice reuse the same persisted choice.
First carried by unified-ui `0.0.793` against Magician `0.6.1207`.
Claude and Antigravity picker rows are unified-ui `0.0.804` against
Magician `0.6.1273`. Live Stage 5 qualification is still owed. Grok Phases 1–5 are in tree: ACP
`grok agent stdio`, honest readiness, fail-closed isolation attestation,
Stop/resume, and usage that never invents `$0.00`. Live Ready may remain
`unqualified` until ACP emits `mcpServers` + tools — fail-closed by
design. Claude Phases C0–C5 are in tree: headless NDJSON, honest
readiness, fail-closed isolation (init-only probe, 5-minute backoff so
Max quota is not billed every 30s), Stop/resume with `continuation_lost`,
and usage that never invents `$0.00`. Antigravity Phases A0–A5 are in
tree: headless NDJSON (`agy -p --output-format stream-json`), honest
readiness, fail-closed isolation (init-only stream-json input, no user
JSONL, 5-minute backoff), Stop/resume via `--conversation` (never
`--continue`) with `continuation_lost`, and token usage that never
invents `$0.00` (`cost_known: false`). Catalog `search_web` is not
Ready-incompatible; runtime use fails closed.

This rebuild replaces the previous 4004-line `+page.svelte` monolith. See the
plan + build ledger at `docs/archive/plans/2026-06-14-vibedev-100x-plan.md` §13
(archived 2026-08-18; live checks in `docs/plans/whattotest.md`, unbuilt tail in
`docs/plans/2026-08-18-vibedev-moat-remainder.md`).

## Files

```
routes/(app)/vibe/+page.svelte         thin host: studio flag → VibeStudio | VibeLegacyStudio
lib/shell/vibe/
  VibeStudio.svelte                    cockpit shell: rail|conversation|stage grid, all reactive wiring,
                                        --vibe-* → house-token alias block, lifecycle, submit, cold start
  VibeLegacyStudio.svelte              the previous monolith, the flag-off fallback (submits through the
                                        same startVibeDevRun call — its private prose builder is gone)
  rail/StudioRail.svelte               ＋New · project switcher (attach-repo / clone-from-GitHub) ·
                                        Runs status board · Checkpoints
  conversation/
    spineModel.ts                      PURE reducer: coding.* event → ordered SpineCard[] (no caps/trim)
    submit.ts                          submit pipeline: POSTs one start request (the SERVER composes
                                        the task description — see "One creation path" below)
    ConversationSpine.svelte           the spine: typed cards, windowed, stick-to-bottom, role="log"
  stage/
    StudioStage.svelte                 Preview | Code | Diff | Tests + status-chip bar + Build/Discuss + Merge
    StageDiff.svelte                   per-file/per-hunk review (folds the old floating VibeReviewPanel)
lib/stores/
  codingSpineStore.ts                  ONE scope-level coding.* NDJSON tail → spineModel snapshot
  vibeStudioStore.ts                   cockpit UI state (stage tab / mode / device / auto-apply) + studio flag
  vibeHitlStore.ts                     HITL review rows + run-chain scoping + apply/reject/approve actions
```

## Data flow

1. `codingSpineStore.start(scope)` opens **one** live NDJSON tail of
   `GET /api/magician/v3/events?event_type=coding.&since=&limit=` (substring
   match → all `coding.*`; bounded backfill then live tail; jittered reconnect +
   connect watchdog). Each line is unwrapped + folded into a `SpineState` by
   `spineModel.applyCodingEvent` (the §3 event→card catalog) and published as a
   reactive snapshot `{ cards, meta, streamState }`.
2. `VibeStudio` scopes the cards to the **active run's task chain** client-side
   (`buildVibeRunChainIds` + `selectCardsForRun`) — the proven monolith
   behaviour — so no server-side `task_id` filter is required.
3. Diff cards are authoritative from the **HITL proposal** (`vibeHitlStore`),
   never from a truncated tool result. `vibeHitlStore` derives review rows from
   `pendingHitlEntries`, scopes/sorts them to the run chain, and owns the
   apply / reject / approve-all / auto-apply actions.
4. The composer (`VibeComposer`, reused) groups its coding-profile picker by
   engine (`groupCodingProfilesByEngine`: Pi, Codex, Claude Code, Grok,
   Antigravity, then Auto). Config profiles are all Pi and their labels name
   only the model — "Grok 4.7 coding" is Pi running a Grok model, not the
   Grok CLI engine — so the group is what says which engine runs a row.
   Engines the server reports as not selectable (`codingProfileStore.blocked`:
   sign-in required, failed isolation, not installed, disabled) are listed in
   their group as disabled options reading `<Engine> (unavailable: <reason>)`,
   so a blocked engine is visible rather than missing. They never enter
   `profiles`, so defaults, restored selections and other consumers never see
   them. The chat composer's `@vibedev` picker does the same. It
   submits through
   `submit.ts → submitCodingRun`, a thin entry that resolves the studio
   preferences from `vibeStudioStore` and hands them to `startVibeDevRun`,
   which makes **one** call —
   `POST /api/magician/v2/vibedev/runs`. The server composes the task
   description, admits the run idempotently, creates it, pins it as the
   project's `active_root_task_id` and executes it. See "One creation path".

## One creation path (handoff plan §10)

`submit.ts` used to assemble the whole task description in the browser
(`buildCodingTaskDescription`) and sequence four calls: create the task, PATCH
the project pointer, execute, and unwind all three by hand if execute threw.
That was the second of two prose assemblers — the magician backend grew its own
for the `@vibedev` chat rail — and nothing could catch them drifting.

They are one now. `POST /vibedev/runs` runs
`VibeDevRunService::start_build`, the same entry the rail uses.
`buildCodingTaskDescription` and its helpers are **deleted**.

What the client still owns, and why: the three studio settings live in
`localStorage` and the server has never been told about them, so the client
sends the **input** behind each line and the server owns the words —
`mode` + `auto_apply` (the policy line), `visual_self_correct` +
`project_is_visual` (the visual directive), `cost_budget_usd` (the budget line),
plus `coding_choice` (`{"kind":"auto"}` or
`{"kind":"profile","profile_id"}`) and, for named rows, `coding_profile_id`.
Everything else on the request
is a fact: the prompt, the project, the parent task **id**, the `@task` chips,
the attachments, the seed, the cron schedule, `save_as_task`, `threaded`.

`client_run_id` is minted per submission and is what makes the cockpit
idempotent: a re-sent submission returns the run it already started instead of
buying a second multi-hour build.

`submit.ts` keeps navigation, attachment clearing, toasts, `bareTitle` /
`stripRunTitlePrefix` (the run title the rail renders and strips) and
`resolveActiveProject`. The start call itself is the exported
`startVibeDevRun(prompt, ctx, prefs)`; `submitCodingRun` is the modern
studio's thin entry, resolving `RunPreferences` (`mode`, `autoApply`,
`visualSelfCorrect`, `costBudgetUsd`) from `vibeStudioStore` before
delegating.

**The flag-off fallback converged too (2026-08-13).** `VibeLegacyStudio.svelte`
no longer keeps its own private copy of the old builder: its `submitVibePrompt`
calls the same `startVibeDevRun` instead of creating the V3 task client-side,
PATCHing the project's `active_root_task_id` and executing as three calls. The
legacy cockpit has no studio store — no Discuss/Autopilot switch, no budget, no
visual-self-correct control — so it passes `mode: 'build'` plus its own
auto-apply toggle and omits the rest, letting the server apply its defaults; it
also sends the coding profile, structured attachments +
`attachment_session_id`, `parent_task_id` for follow-ups,
`referenceTaskIds: []` (the server merges the parent continuation ref itself)
and `saveAsTask: true`, because legacy runs were always ordinary user-visible
tasks and the legacy run rail reads the task store — an Internal run would
vanish from it. Its six prose helpers (`buildCodingTaskDescription`,
`projectContextBlock`, `followUpContextBlock`, `stagedAttachmentPromptBlock`,
`promptTitle`, `continuationReferenceTaskIds`) and four transitively dead
symbols (`formatBytes`, `projectRepoKind`, `VIBEDEV_FOLLOW_UP_TAG`,
`ENGINEERING_MANAGER_AGENT_ID`) are deleted — the server assembler is their
byte-pinned successor. Its auto-apply default also flipped **OFF** (U5) to
match the shared store: same `localStorage` key, only the no-saved-value
fallback changed. Retiring the component is still the flag's removal, not this
change.

Two consequences worth knowing when reading the cockpit's state:

- there is no longer an **optimistic** task row inserted by
  `taskStore.createTask`; the just-created run reaches the UI through
  `maybeFetchActiveTask` (already the wired fallback for exactly this case) and
  the internal-runs poll;
- `taskStore.executingTask` is no longer set by a cockpit submit, so the
  composer's "Another task is executing." gate no longer fires for the cockpit's
  own run. `taskAcceptsFollowUp` already blocks the composer while the in-view
  run is unfinished, which is the gate that mattered.

## Interactive control plane (steer / stop) + terminal cancel

While a build is live the cockpit can message the in-flight Pi turn:

- `conversation/control.ts` → `controlRun(runId, 'steer'|'follow_up'|'stop', message?)`
  hits `POST /api/magician/v2/vibedev/runs/{runId}/control`.
- `VibeStudio` derives `liveRunId` (the newest non-terminal card's task/shadow
  id) and `canSteer`. While live it shows a **Stop** button + a "Building — type
  to steer it" bar; submitting the composer **steers** the live turn instead of
  starting a new task (falling back to a follow-up task if the turn just
  settled — a `409` from the endpoint). The Stuck banner's Redirect steers too.
- The backend reaches the live turn via a scope-qualified registry, so a run is
  only steerable by its own scope, and only while the turn is actually in flight
  (see `docs/components/magician/pi-coding-engine-contract.md`).

### `■ Stop` is step-level, `✕ Cancel run` is terminal (2026-06-17)

The live bar has **two** distinct affordances; they are NOT the same action:

- **`■ Stop`** (`stopRun`) is a *steer-level* control — it aborts the **in-flight
  Pi turn** via `controlRun(stopRunId, 'stop')` so you can redirect. It does **not**
  cancel the execution tree, so a *re-delegating coordinator* (an EM that keeps
  spawning fresh engineer turns) just spins up the next turn — `Stop` becomes
  whack-a-mole on a runaway/looping run. Styled soft/neutral; toast "Stopping step".
- **`✕ Cancel run`** (`cancelRun`) is *terminal* — it calls
  `taskStore.cancelExecution(activeVibeTask.executionId)` →
  `POST /api/magician/v3/executions/{root}/cancel`, which cancels the **whole
  execution tree at the coordinator root** (`cancel_execution_tree`). A cancelled
  coordinator is in a terminal state and cannot re-delegate, so the loop actually
  stops. Styled with the strong error treatment; disabled until the task exposes a
  root `executionId`. This is the real "make it stop" for a looping run.

Prior to this fix the cockpit only wired `Stop` (turn-abort), so a user cancelling a
looping build never killed the run — matching the backend re-arm guard added in
`docs/archive/components/magician/vibedev-cockpit-blueprint.md` (cancel now sticks even
against a late child-completion race).

## Click-to-edit (U7)

Toggle **Edit** on the preview to edit the running app by pointing at it:

- The preview proxy injects a dormant overlay `<script>` into proxied HTML (see
  `api::vibedev_preview_proxy`). `VibeStudio.broadcastEditMode` toggles it via
  same-origin `postMessage` to the iframe; the overlay outlines the hovered
  element, posts the clicked element (`{tag, id, classes, text, loc, selector}`)
  back, and supports inline text editing on double-click.
- `VibeStudio.onPreviewMessage` receives those. A `select` opens the element
  panel (`.eledit`) — describe a change → `applyElementEdit` composes a precise
  instruction (tag + selector + text + `data-vibe-loc`) and routes it through
  `dispatchPrompt` (steer the live turn, else start a run). A `text-edit`
  (inline double-click) routes a targeted text-replacement automatically.
- Every edit currently routes to **Pi** (auditable diff via the normal gate).
  The instant, credit-free no-LLM source-patch path (a `data-vibe-loc` stamper +
  a source-edit→proposal endpoint) is a documented follow-up (plan §13.3 #6);
  the overlay already forwards `data-vibe-loc` so it's additive when it lands.

## Self-heal loop + project type (S1/S2/S3)

- `stage/checks.ts` → `fetchProjectInfo(projectId)` (`GET …/info`:
  `{ project_kind, previewable, checks[] }`) and `runCheck(projectId, kind?)`
  (`POST …/check`). `VibeStudio` fetches info per project, sets
  `previewAvailable` (Preview hidden for non-web), and passes the available
  checks to the stage.
- The **Tests tab** (`StudioStage`) runs build/test/lint/typecheck in the
  project shadow (Run checks + per-kind buttons), renders red→green result
  cards, and an **Attempt fix** button dispatches `attemptFix` → `VibeStudio`
  composes a prompt from the failing `output_tail` and routes it via
  `dispatchPrompt` (steer the live run or start one). Live test events from the
  Pi stream still render below.
- ＋New → **✨ SvelteKit starter** (`StudioRail` → `startStarter`) creates a
  project and seeds a SvelteKit+Vite+Tailwind scaffold prompt (pre-warmed COW
  template → plan §13.3 #7).

## Autopilot — ship while you sleep (M1)

A third mode beside Build/Discuss (`vibeStudioStore` `StudioMode`). In Autopilot:

- `setMode('autopilot')` forces the cockpit's client-side auto-apply OFF — the
  **agent** applies its own proposals, so the cockpit must not race it.
- `mode: 'autopilot'` on the start request makes the server append its
  Autopilot directive: branch-first (dugite, never main), delegate the verbatim
  run_coding_task → `apply_code_proposal` → `run_project_checks` → fix loop, then
  report via `notify_owner` (+ `delegate_to_agent(personal-assistant)` for
  WhatsApp/email). The server also tags the task `autopilot`.
- A composer bar (`VibeStudio`) explains it, with a **Run nightly** checkbox that
  threads a cron `schedule` (`0 2 * * *` UTC) onto the start request; scheduled
  runs are NOT executed immediately (they fire on the cron), and the server
  promotes them to a user-visible task because the cron scheduler never walks the
  internal tasks root.
- The run is already a durable background V3 task, so closing the tab is fine;
  review the branch (`git diff`) on return via the report + the CLI Agent.

Backend tools/grants + deferrals: `docs/components/magician/pi-coding-engine-contract.md`
(M1 section) and plan §13.3 #8–13.

**Cost/quality autopilot (M2):** the Autopilot bar also takes a **cost budget ($)**
(`vibeStudioStore.costBudgetUsd`). `costBudgetLine()` weaves it into the directive as an
agent-enforced stop-condition keyed on `session_stats.cost` (returned in every
`run_coding_task` result); the server's escalation line is informational
context about the committed pin (`coding-balanced` with a configured one-hop
to `coding-premium` on hard steps). Magician enforces the pin at dispatch.
`StudioStage`
shows a spent-vs-budget chip + progress bar (green→amber→red); `VibeStudio` also auto-stops
the live run (U8 control plane) if the meter crosses the budget while the tab is open. The
true hard resource-authority gate + per-turn model dial are deferred (plan §13.3 #14–16).

The model selector is populated from the server-owned coding catalogue. The
current set is GPT-5.6 Terra (default), GPT-5.6 Sol (premium), GPT-5.6 Luna,
DeepSeek V4.1 Flash, and MiniMax M3. DeepSeek V4.1 Flash is
text-only, so screenshot attachments remain available only when the selected
catalogue row advertises image support. Kimi K3 is intentionally absent until
a corresponding routed profile exists.

## Cross-surface ingress — seed a build (M4)

Any surface can open the cockpit seeded with context:

- `lib/stores/vibeSeedStore.ts` — `putSeed(draft)` stashes a payload in
  `sessionStorage` and returns a short id; the surface navigates to
  `/vibe?seed=<id>`; `VibeStudio` on mount `takeSeed`s it (read-and-delete →
  refresh-safe), shows a dismissible **"Seeded from …"** chip + collapsible
  preview, optionally pre-fills the ask, and strips the `?seed` param. The seed
  flows into the run only as the request's `seed_content` / `seed_label`, which
  the server renders into its own seed block (like the project block) — the
  composer ask stays clean.
- **Chat** (`ChatPanel.buildInVibe`) serializes the recent thread turns;
  **meetings** (`/meetings/[thread]` → `buildFromMeeting`) serialize the
  transcript **plus the live capture's rolling summary** (`live.latest_summary` —
  the agreed `## Decisions` / `## Action items` prose, §13.3 #19) so the EM builds
  from what was decided, not just raw turns; **voice** is the composer mic
  (already wired). Serializers (`buildChatSeedContent`/`buildMeetingSeedContent`)
  cap turn count + length so the run prose stays bounded. The summary renders as a
  labeled block via `clampBlock` (newlines preserved — NOT the whitespace-collapsing
  `clampTurn` path used for transcript turns), so its markdown structure survives.
- **Durable provenance (§13.3 #20):** the seed carries its origin id (`sourceId` =
  meeting `thread_id` / chat `session_id`); on hydrate `VibeStudio` stamps it onto the
  project the build lands in (`vibeDevProjectStore.updateProject`, **first-source-wins**,
  one-shot). The rail's project header then shows a durable **⚓ from meeting** reverse
  link (meetings deep-link to their thread; chat shows a chip — no per-session route).
  Survives reload, unlike the old ephemeral sessionStorage chip.
- Deferred: screen-observe ingress, structured decisions (plan §13.3 #17).

## Visual self-correction — the agent sees its UI (M5)

A **👁 Visual self-correct** checkbox sits in the stage mode row (shown outside
Discuss), flipping `vibeStudioStore.visualSelfCorrect` (opt-in, off by default,
persisted to `localStorage`). When on, the request carries
`visual_self_correct: true` and the server appends its visual directive after the
Autopilot block, telling the engineer to:

- after a visible change, call the backend **`screenshot_preview { project_id }`**
  to capture the running R3 preview (it returns `ok:false` if no preview is
  running — then start the dev server, or skip the loop for non-visual changes);
- pass the returned `attachment_id` + `attachment_session_id` into the next
  `run_coding_task` so **Pi sees the screenshot as an image** and critiques the
  render against the goal (layout/overflow/alignment/spacing/contrast), then
  `apply_code_proposal` + `run_project_checks` + `screenshot_preview` again to
  verify — a **HARD CAP of 3 visual passes**, skipped for non-visual work.

This is purely cockpit state + a directive + the M5 backend tool — no new vision
op (Pi is the critic via the shipped image attachment pass-through). It needs a
coding profile that supports image inputs; a run reports clearly if it does not.
Backend contract + deferrals: `docs/components/magician/pi-coding-engine-contract.md`
(M5 section) and plan §13.3 #21–25.

**Visual tab — before/after history (§13.3 #25).** A **Visual** stage tab surfaces the
screenshot history that `screenshot_preview` captures into the synthetic
`vibedev-shots-<project_id>` session (already durable on disk; previously buried). It fetches
`GET /vibedev/projects/{id}/screenshots` (`checks.ts` `fetchProjectScreenshots`) and renders the
two newest side-by-side (**Before / After**) plus a thumbnail strip of older shots; PNGs load via
the existing `/chat/sessions/{id}/outputs/{name}` route (scope passed as query so `<img>` can load
it). Fetches on tab-open per project + a ↻ Refresh button. Read-only — the capture loop is still
the M5 directive above.

## Migration flag

The cockpit ships behind `?studio=` (default **on**), persisted to
`localStorage('magician.vibedev.studio')`. `?studio=0` falls back to the
monolith (`VibeLegacyStudio`) until parity is fully verified — no longer
verbatim: since 2026-08-13 its submit goes through the same server-side
creation path (see "One creation path"). `readStudioFlag`
in `vibeStudioStore.ts` resolves it.

## Conventions

- All cockpit components use the `--vibe-*` CSS aliases defined once on the
  `VibeStudio` root, mapped to the real `app.css` house tokens (radius/color/
  spacing/motion). Do **not** reintroduce the monolith's admin-dashboard chrome
  (font-weight 800/850, `text-transform: uppercase`, 5–8px IDE radii).
- Reuse, don't reinvent: `DiffStrip` (the only diff renderer), `ChatMarkdown`,
  `CodeBlock`, `VibePreviewPanel`, `WorkbenchColumn`, `VibeComposer`.
- Respect `prefers-reduced-motion`; the spine is `role="log" aria-live="polite"`,
  the stage tabs are a `role="tablist"`.

## Persistent CLI access in the first-run stage (2026-07-15)

`StudioStage.svelte` keeps the right-panel **CLI Agent** entry point visible
before any run, preview, diff, or test content exists. The first-run narrative
still suppresses irrelevant run telemetry, Apply, deployment, and tab controls;
only the independently useful CLI control remains in the bottom status bar.

## Coordinator self-serve guardrails — UI surface (2026-06-17)

Two UI changes pair with the backend coordinator-self-serve / false-success guardrails
(`docs/archive/components/magician/vibedev-cockpit-blueprint.md` → "Coordinator self-serve … guardrails";
RCA `docs/plans/2026-06-17-vibedev-coordinator-self-serve-false-success.md`):

- **Honest empty-state** (`conversation/ConversationSpine.svelte` + `VibeStudio.svelte`): a fresh
  TERMINAL run that engaged no coding pipeline (the RCA case — now terminated `failed` by the
  server success-gate) showed the misleading "no activity … may predate the build-history window"
  copy. New `coordinatorTerminal` prop (parent derives it: `taskTerminal && !activeTaskLoading &&
  chainCards.length === 0 && status === 'failed'`) drives an accurate empty-state: "this run
  finished without engaging the build pipeline — the coordinator didn't hand the work to an
  engineer …". Distinct from the aged-out case.
- **Execution-policy relocation** (`VibeLegacyStudio.svelte buildCodingTaskDescription` — since
  deleted, see "One creation path"): the
  static "Execution policy:" contract (route through engineers / `run_coding_task` / stage
  `CodeChangeProposal`) is now injected SERVER-SIDE for every coding-coordinator Build, so it
  reaches programmatic/non-cockpit creators too. At the time, the legacy studio dropped the
  marker block and kept only the per-run profile/review prefs client-side under
  "Run preferences:"; as of 2026-08-13 no cockpit composes any goal prose, so the server's
  marker-skip guard (no double-injection when a goal already carries the marker) matters only
  for programmatic creators that write their own goal.

## Self-advancing "starting" stepper (2026-06-18)

The pre-coding window (workspace/context setup → routing to an engineer → engineer
warmup) emits **no `coding.*` events**, so the spine — which renders `coding.*` only —
otherwise sat on a frozen "Starting shortly…" spinner for up to ~60-90s and read as
*stuck*. `ConversationSpine.svelte` now advances the empty-state placeholder through the
REAL phases on an elapsed timer (`STARTING_PHASES`: Setting up → Routing to an engineer →
Engineer starting up → Still working), reset per `runKey`, hidden the instant the first
coding card lands. Directional phases (match the EM-setup → route → engineer-spawn
timeline), not fabricated specifics — honest motion instead of a frozen frame. This is
the perceived-latency half of `docs/plans/2026-06-17-vibedev-run-startup-latency.md`; the
real wall-clock cuts (API_MINING detach, memory-seeding dedup, pre-bind run_coding_task,
double-synthesis root-gate) are tracked there.

## No frozen "Building"/"Synthesizing" on dead runs (2026-06-18)

A looped-then-failed run (and an orphaned `synthesis_pending` flag) could leave the rail
badge stuck on "Building"/"Synthesizing" forever: `runStatus` keyed off `task.status` /
`task.synthesisPending` with no terminal- or staleness-guard, and the spine pinned itself
non-terminal on every approval. Fixes: `StudioRail.runStatus` no longer renders
"Synthesizing" for an orphaned `synthesis_pending` on an already-terminal task (it falls
through to the terminal status), and a running/synthesizing run with no task update for
`RUN_STALE_MS` (10 min) renders **"Stalled"**, not "Building"; `spineModel`'s
`coding.approval_requested` handler no longer pins `runMeta.terminal = false` (an approval
is a waiting state, not proof the run is live — pinning it false made a later task-level
failure, which emits no spine terminal event, read as "Building" forever). Part of the
EM re-delegation-loop RCA (`docs/plans/2026-06-18-em-redelegation-diff-approval-loop.md`,
fix D).

## Checkpoint nodes + one-click rewind (2026-06-18)

The rail's **Checkpoints** section renders one node per checks-passing applied change for the
active run (`vibeCheckpointsStore` ← `GET /vibedev/runs/{task}/checkpoints`, refetched whenever
the run's `updatedAt` bumps so a minted checkpoint appears). Each node is informational; a
git-anchored one (`cp.git_sha` present) also shows a **hover ⟲ rewind** button. Clicking it
dispatches `selectCheckpoint`, which `VibeStudio.rewindToCheckpoint` handles: confirm →
`revertVibeCheckpoint(taskId, checkpointId)` (`POST .../checkpoints/{id}/revert`) → success/error
toast. The backend snapshots the current tree first (undoable) and queues the checkpoint's Pi
session to resume on the next coding turn, so the agent's context rewinds with the code (see
`docs/components/magician/pi-coding-engine-contract.md` → "Checkpoint rewind"). The ⟲ is
deliberately a small hover affordance, not the whole-node click, so inspecting a checkpoint can
never trigger a destructive rewind. Non-git checkpoints render without the button (and the handler
guards with a "not rewindable" toast).

## Code tab — full-repo browse (2026-06-18)

The Code tab was bound strictly to the changed-file set (it showed "No changed files yet." until a
proposal existed). A **Browse repo** toggle in the tree (`StudioStage.svelte`) now flips it into a
filterable full-repo file list and a read-only content view, so you can read any file — not just
what a run changed. Backed by `checks.ts` `fetchProjectFiles` / `fetchProjectFile` → the
repo-confined `GET /vibedev/projects/{id}/files` and `/file?path=` endpoints (§13.3 #5). The browse
cache resets on run/project switch; binary and oversize files are reported rather than dumped; the
tree list is capped (`…list truncated`).

## Status-pill contrast — WCAG-AA (U6, 2026-06-18)

The rail's run-status pills (`.run__badge--*` + `.run__type`, 0.6rem/700 → **normal** text, needs
≥4.5:1) used the full-saturation status hue as text over a 16% tint of the same hue. On light
themes that failed badly — light-theme warning `#ffe66d` on its tint computed ≈ **1.2:1** (also
success ≈2.1, error ≈2.4, info ≈2.5). Fix (token-only, no per-theme overrides): the pill text is now
`color-mix(in srgb, <hue> 35%, var(--vibe-text))` — the hue pulled 65% toward the theme's
high-contrast text colour, keeping hue identity but readable. Verified by a relative-luminance
computation across light + dark themes: **all pills ≥4.5:1** (worst case ~4.7:1; dark themes
8–9:1). Tints unchanged. Mixing toward `--vibe-text` self-corrects in both directions (text is dark
in light themes, light in dark themes).
