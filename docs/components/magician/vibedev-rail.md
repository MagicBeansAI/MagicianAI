# `@vibedev` — the rail into VibeDev

> `@vibedev <request>` in chat starts a VibeDev run instead of a reply;
> `@vibedev #discuss <question>` starts a plan-only run; hands-free, say **"start
> a vibedev build …"**. A second `@vibedev` in the conversation continues the
> first (§12). The cockpit uses the same server-owned service (§6.1).

**Not a boundary**: it fences nothing off; an agent holding `shell` can still
write to a repository without it (such a fence was judged not achievable).

**Design:** `docs/archive/plans/2026-08-10-vibedev-code-rail-design.md` ·
**Plan:** `docs/archive/plans/2026-08-10-vibedev-code-rail-phase1.md`

---

## 1. What happens to a `@vibedev` turn

No model is consulted: the marker **is** the intent.

1. Recognized (typed or spoken): `magician_v2/chat/invoke_grammar.rs` —
   `parse_vibedev_rail_invocation`.
2. Classified `FeatureMode::Vibedev`, surface kept (`Chat` / `RealtimeVoice`):
   `chat/service.rs` — `chat_invocation_context_for_turn`.
3. Diverted after the user turn persists, before the Tutor lane or any model
   call, at the top of `process_chat_inline_turn` (non-streaming, SSE and Tutor
   background continuation all converge there) → `start_vibedev_rail_run`.
4. Project: `VibeDevRunService::resolve_project` over
   `magician_v2/vibedev/projects.rs` — `resolve_vibedev_project_for_session`.
5. Start, refuse or ask: `magician_v2/vibedev/rail.rs` — `decide_vibedev_rail_turn`.
6. Owner from `coding.lead_agent_id`: `magician_v2/agents/task_ownership.rs` —
   `resolve_created_task_owner_agent_id`.
7. Description, tags, title assembled; **durably admitted**, created, dispatched:
   `magician_v2/vibedev/run_service.rs` — `VibeDevRunService::start_build` (§6).
8. Reply and detach: `chat/service.rs` — `reply_without_model`.

Product logic: `magician_v2/vibedev/` (`projects.rs`, `dispatch_intent.rs`,
`run_service.rs`, `rail.rs`), behind the 1.2 lane seam; not an app package (the
coding engines stay core). Chat service keeps orchestration (arm ordering,
divert placement, session listing, dispatch, logging). `rail.rs` owns
`rail_turn_lane` (lane triple a leading invoke mints), `rail_admits_turn`
(divert guard), `vibedev_rail_reply_key` (sentence per `start_build` outcome)
and `hot_chat_tools` (the lane's hot set); `vibedev::run_service` owns the
queued-turn coding-choice mappings (`queued_coding_choice` /
`vibe_coding_choice_from_queue`) that bound a queued turn's engine across replay.

## 2. Which project a build lands in

`select_vibedev_project_for_session`, in order:

1. the project whose `chat_session_id` **is** this session;
2. the scope's only non-archived project;
3. several, none owned → **the active project if the cockpit points at one**,
   else **ask** (§2.1);
4. none → **refuse**.

Archived projects are never resolved, counted or offered. **The rail never
creates a project** (only the cockpit's list endpoint mints records), so a
never-listed cockpit session deliberately resolves to nothing.

### 2.1 The ask

`active_vibedev_project` returns the first non-archived project whose **cockpit
session is open**, else the newest by `updated_at_ms`. Only an open session is a
pointer the user set (`POST` `/vibedev/projects/{id}/activate` sets its session
`active` and bumps `updated_at_ms`), so the rail acts only on that. It shares `project_list_response`'s source of `active_project_id`
and `vibedev_project_session_is_open`, so it never disagrees with the cockpit.

The user answers by picking in the cockpit and re-sending (no
`@vibedev in <project>` grammar: name matching could build in the wrong repo).
Candidates are listed in display order (repo path when names repeat, e.g. adopted
"VibeDev Project"), capped at six plus "…and N more" (N the true total) for
reading aloud. **Residual:** among several open
sessions, the most recently activated wins silently.

## 3. Refusals — each creates nothing

| Case | Reply |
| --- | --- |
| No non-archived project | `@vibedev` only finds projects; go to the cockpit |
| Several live projects, no pointer | **A question**: candidates; pick one and re-send |
| No request after `@vibedev` | The shape, including `#discuss` |
| `coding.lead_agent_id` unset | Says so; the rail will not pick an owner |
| Create or dispatch failed | Original error; nothing was left running |
| Key reused with a **different** request | Names that key's run; untouched (§11) — not the failure reply, whose "nothing was left running" would be false |

## 4. The task the rail creates

* `ui_thread_id: "vibedev"` — appears in the cockpit's run history.
* Tags: `vibedev`; plus `plan` for `#discuss`; plus `vibedev-follow-up` and
  `vibedev-threaded` when continuing an earlier run (§12).
* `lifecycle: Internal` — a cockpit-coupled execution, not a tracked
  deliverable (same as the cockpit's `save_as_task: false` default). A cockpit
  run is promoted to `Persistent` when saved as a task **or** scheduled: the
  cron scheduler enumerates only the `tasks/` root, so an `Internal` scheduled
  run would never fire.
* `chat_session_id` — the starting conversation, so chat-session cleanup owns
  it. **Rail runs only**: cockpit runs stay unbound, because binding them would
  make every existing cockpit run sweepable with its project's session.
* Owner: `coding.lead_agent_id`, via the same helper `POST /v3/tasks` uses. The
  rail also passes it as the *requested* agent, because a Discuss run is not a
  build run and skips the override (the cockpit covers that with a client-sent
  lead). A Discuss run that names no owner gets the lead too
  (`task_ownership::choose_owner`), never `personal-assistant`: the lead
  delegates the plan call to an engineer, and the assistant can neither call
  `run_coding_task` nor delegate to one. The lead delegates once; the
  engineer's `run_coding_task` is forced plan-only by the `plan` tag, and the
  coding engine runs boxed read-only (Pi included, via
  `PiTurnOptions.plan_only`), so the repo is not written.

### Control lines

`run_coding_task repo_path: <path>` must not break: `agents/runtime.rs`
(`build_vibedev_coding_delegate_goal`) **falls back to `.` when absent**, so a
missing or misspelled line silently starts the engineer in the wrong directory.
Reader and assembler share `pub(crate)` `VIBEDEV_REPO_PATH_LINE_PREFIX`; tests
read the path back the way the runtime does.

| Line | Decides | Read by |
| --- | --- | --- |
| `run_coding_task repo_path: <path>` | **the repository a build runs against** | `build_vibedev_coding_delegate_goal` |
| `VibeDev project: <uuid>` | project for a run, a Citizen API call, a `contribute_to_project` write | `parse_vibedev_project_id` |
| `Parent task: <id>` | chain (hence coding session) the run joins | `parent_task_id_from_description` |

**The request is data.** Readers take the **first** match, and the end marker
(`VIBEDEV_USER_PROMPT`) is a substring of the begin marker
(`<<<VIBEDEV_USER_PROMPT`), so every reader first applies the server-side
`vibedev_trusted_control_region` (`agents/runtime.rs`), cutting from the first
begin marker through the **last** line equal to the end marker. A forged marker
only lengthens the cut, dropping a control line (repo `.`, no project, root
run), never promoting one. No fence ⇒ untouched (legacy); an unclosed fence
cuts to end of string (fails closed). Server-side on purpose, so it covers every
assembler, including the TypeScript one. **Do not unify it with**
`extract_marked_vibedev_user_prompt`, which takes the **first** end marker
(never swallow server lines into the prompt; the cut never leaves user text).

**Post-fence invariant (security):** a newline in a client field after the fence
could forge a later end marker and `repo_path`, so each goes through
`vibedev::run_service::vibedev_line_break_free` (`\n`, `\r`, U+0085, U+2028,
U+2029 → space, **nothing else changed**, no whitespace collapse: the fields are
read back as data and pinned by byte-identity tests against the client assembler):

| field (post-fence unless noted) | note |
| --- | --- |
| attachment `attachment_id`, `mime_type` | raw from body |
| attachment display name (`label \|\| filename`) | emptiness tested on the **raw** label, so `"   "` renders as itself (client `\|\|` truthiness) |
| `attachment_session_id` | raw |
| seed **label** | raw |
| `coding_profile_id` (escalation-policy line) | **unvalidated, no allowlist**; normalised at the API boundary (stored/digested) *and* interpolation, deliberately redundant |
| parent **title** — *above* the fence (rail), below (cockpit) | normalised at both; above the fence first-match alone would let a chosen task title win |

Safe as is: `attachment_ids` (keep it `serde_json`-encoded, not a plain join),
`preview_url` (round-trips `url::Url::parse`), server-minted `chat_session_id`
and `project_id`. Project name: `normalized_project_name` at store time; cockpit
parent `summary`: `vibedev_cockpit_trim_one_line`.

**Known residual: `seed_content`.** A multi-line cockpit seed (transcript, chat
thread) sits *above* the project block, so a control-shaped line in it wins
until a nonce-delimited fence closes it. Any new interpolated field
belongs in the table or this residual; a wrong "closed" is worse than an honest gap.

### A run that ends tells the owner

`vibedev/run_terminal_notice.rs` files an owner briefing
(`harness.notify_owner.briefing`; Attention and the owner's devices) when a run
reaches `completed`/`failed`/`cancelled` and is Autopilot, or **failed**. A
build or Discuss run that finishes or is stopped stays quiet however long it
took (no duration rule). Skipped if the run's agent already filed `notify_owner`
for the task; request id `vibedev-run-ended:{task_id}:{root_execution_id}` (no
timestamps) makes a re-asserted status replay. Hooks (every terminal path):
`close_owner_escalations_if_ended` (failed/cancelled),
`auto_publish_completed_task_note` (completed), in `artifact_v2/service.rs`.
Autopilot not-green uses `severity=warning` (`notify_owner` reads `warn` as
`warning`).

### Matching rules

* **No Autopilot from chat**: only `DispatchMode::Autopilot` (cockpit) adds the
  `autopilot` tag; the rail's `DispatchMode::from_discuss` returns `Build` or
  `Plan`. Tests assert the tag's absence in both rail modes.
* **Bare `vibedev` does not match** (unlike `@tutor`/`tutor`,
  `@copilot`/`copilot`): "vibedev is stuck again" must not spend compute. The
  marker is **tokenized, leading position only** (not `@vibedev-review`, quotes
  or mid-sentence).
* **One reader**: `parse_leading_feature_marker` does **not** list `"@vibedev"`;
  it calls `parse_vibedev_rail_invocation`, which also holds the spoken phrase,
  so `parse_leading_feature_invocation` can never say `Vibedev` for text the
  rail refuses as empty.

## 5. Where the prose lives

Prompt store `data/magician_v2/prompts/`; each has a compiled fallback, so a
missing template degrades instead of bricking the rail:

| Prompt | Use |
| --- | --- |
| `vibedev_rail_project_context` | Project-context block |
| `vibedev_rail_plan_directive` | Discuss/plan directive |
| `vibedev_rail_reply_started` | Ack naming the 32-hex task id |
| `vibedev_rail_reply_no_project` | No project |
| `vibedev_rail_reply_empty_prompt` | No request (both spellings, no markup) |
| `vibedev_rail_reply_no_coding_lead` | No owner |
| `vibedev_rail_reply_start_failed` | **Promises "nothing was left running"**: only after nothing was created or a rollback |
| `vibedev_rail_reply_admitted_not_started` | Admitted, not dispatched (§7) |
| `vibedev_rail_reply_key_conflict` | Key reused, different request |
| `vibedev_rail_reply_ambiguous_project` | No pointer (§2.1) |
| `vibedev_rail_follow_up_context` | Continuation block (§12) |

Spoken `voice_*` prose (`voice_diff_approval_waiting`,
`voice_verification_{verified,unverified,exhausted}`) has **no** fallback (§13).
Never in the store: machine-read lines (`VibeDev project:`,
`run_coding_task repo_path:`) and any "Execution policy" block — builds get the
server's `vibedev_execution_policy` at execution start (`artifact_v2/service.rs`,
gated on `is_vibedev_coding_build_run`); a second copy could drift and would
contradict the plan directive on a plan run.

## 6. One server-owned creation path

`VibeDevRunService` (`magician_v2/vibedev/run_service.rs`) is **the** way a run
is created; `start_build(StartVibeDevBuild)` is its only public entry
(`admit_create_and_dispatch` is private), called by this rail, the cockpit
(§6.1) and restart recovery (`vibedev_run_create_task_input`).
`StartVibeDevBuild` carries **trusted context and validated choices only**:
scope, chat session and turn ids, owner agent id, resolved project, verbatim
request, mode, optional `VibeDevCodingChoice`, optional `VibeDevCockpitRun`
(`None` from chat). One assembler produces task id, title and description.

### `VibeDevCodingChoice`

Per the handoff plan
§4 and the Codex app-server plan
§2/§6.1: `Auto` or `Profile { profile_id }`, omitted = default, same type from
every surface; wire `{"kind":"auto"}` / `{"kind":"profile","profile_id":"…"}`,
engine-neutral, immutable per request; browser type `ClientVibeDevCodingChoice`
(`deny_unknown_fields`).

* Omitted **inherits the launching engine**: `bind_coding_constraint` maps the
  `RunEnginePin` ([plane.md](plane.md) "Run engine pin") through
  `inherited_catalog_profile` (Ready Claude/Codex/Grok/Antigravity, or a Pi
  pin's LLM profile; source `inherited_run`), else (native loop, engine not
  Ready, unmapped Pi profile) `configured_default`.
* It is **not** `CodingEngineConstraint`, which is derived from it plus the live
  `coding.profiles` catalog, pinned on `DispatchTaskPlan` before admit, enforced
  by `run_coding_task` on the task and its parents; the adapter comes from the
  journaled engine.
* Pi fields stay on `request.pi`; fabricated `pi_binary` / `session_name` /
  `persist_session` are rejected; events go only through the sink
  (`event_count`); checkpoints resume from `native_session_id`, else
  `pi_session_id`.
* `codex-default` is published from a filesystem snapshot (a cached
  qualification receipt may overlay it). The composer shows only selectable Pi
  profiles (Pi is the default) unless the kill switch is on, when a Ready
  `codex-default` row is selectable; Auto is an opt-in last row. Chat,
  composer-voice and hands-free `session.start` send the persisted
  `coding_choice`; the rail never reads an engine name from prose.

### 6.1 The cockpit

`POST /api/magician/v2/vibedev/runs` (`magician-api/src/vibedev_api.rs` —
`start_vibedev_run_handler`) calls the same `start_build` (create, pin,
dispatch, rollback in one). The client sends **preferences**; the server writes
the **prose**:

| Client block replaced | Sent as | `localStorage` key |
| --- | --- | --- |
| `vibeStudioStore.policyPromptLine()` | `mode` + `auto_apply` | `magician.vibedev.autoApplyCodeProposals` |
| `vibeStudioStore.visualSelfCorrectDirectiveBlock(…)` | `visual_self_correct` + `project_is_visual` | `magician.vibedev.visualSelfCorrect.auto` |
| `vibeStudioStore.costBudgetLine()` | `cost_budget_usd` | `magician.vibedev.costBudgetUsd` |

**Two layouts, one assembler**: `vibedev_run_task_description` branches on
whether a `VibeDevCockpitRun` was passed. The cockpit layout is byte-identical to
the former client `buildCodingTaskDescription`
(`the_cockpit_build_description_is_byte_identical_to_the_client_assembler`),
built from compiled constants in `vibedev/run_service.rs`, not the prompt store.
Port quirks (commented): an absent optional block is an **empty line**; JS
`toFixed` rounds ties up (Rust `{:.0}` to even); millisecond-`Z` ISO timestamps.
`VibeLegacyStudio.svelte` (`?studio=0`, not the default cockpit) keeps its own
builder.

**`VibeDevCockpitRun`:** staged attachments and their session, seed context,
`@task` chips, cron `schedule`, `save_as_task`, explicit `parent_task_id` with a
`threaded` flag, `created_by`, `pin_project_pointer`.

**Parent:** the rail *infers* one (§12). The cockpit *names* one (never infers:
its runs share one `#vibedev` session); the server loads it, checks it is an
in-scope VibeDev run, derives title/status/execution/summary from the record
(never the request), and **refuses** if invalid.

### 6.2 The cockpit's idempotency key

`blake3(domain ‖ principal ‖ workspace ‖ project.chat_session_id ‖ client_run_id)`.
The composer mints `client_run_id` per submission and re-sends it on retry, so a
re-submit returns the existing run instead of a second build; a client omitting
it gets a fresh id per call and no dedup.

## 7. Failure and rollback

Create and dispatch are one step. A dispatch failure deletes the task (else an
uncancellable `pending` run), reports the **original** error (cleanup failure
never masks it), and records the failure terminally on the intent (§11) so
recovery, which rebuilds missing tasks from it (§11.1), does not resurrect it.
Losing that best-effort write could let a restart start a build reported as not
started — accepted, rather than risk losing admitted runs.

**Project pointer.** A cockpit run pins itself as `active_root_task_id` after the
task exists and before dispatch; on failure the pin is **unwound before the
orphan is deleted** (even if the delete fails, so it never names a deleted task),
only if it still names this run (`VibeDevProjectPointer::PinTo(task_id)` /
`ClearIfNames(task_id)`; else unwinding crashed run A would clear later run B's
pointer); live path and recovery share `rollback_undispatched_vibedev_run`.
`@vibedev` sets no pointer (passes no `VibeDevCockpitRun`).

**`projects.json`.** Both publishers (`mutate_project_store_at`,
`save_project_store`) write a unique temp, fsync, rename, fsync the directory,
and remove the temp on failure; a torn store makes `load_project_store`
hard-error (endpoints 500; rail says "no project"). **Every writer takes
`project_store_lock`** (each rewrites the whole file, so a lost update
republishes the loser's store; process-wide registry keyed by store path, so
scopes never wait on each other; one process owns a data root): the four
project endpoints (create, activate, update, delete), the deployment recorder,
`GET /vibedev/projects` (mints records), and
`set_vibedev_project_active_root_task_id` (`async`, `tokio::fs`). The
`tokio::sync::Mutex` is held across the chat-store awaits from load to save
(narrowing it reopens the window). Readers (`read_vibedev_projects`,
`load_vibedev_project`, the citizen resolver) take none: the file is replaced
atomically. Test `a_rename_and_a_pointer_pin_do_not_overwrite_each_other` races
a rename against a pin on different projects.

### Admitted but not started

A replay of `AlreadyAdmitted(pending)` (or any admission with
`execution_id: None`) returns `Replayed { execution_id: None }` →
`VibeDevRunStartError::AdmittedNotStarted` /
`vibedev_rail_reply_admitted_not_started`; `POST /vibedev/runs` answers
**202 Accepted**: recorded, not started, nothing to clean up, and a resend buys
no second run. The claim precedes `ensure_task_with_id`, so the reply's task id
does not resolve yet. The live path claims only freshly `Admitted` intents, so
the run starts at the next process start via `recover_pending_vibedev_dispatch`
(spawned **once per process** from `magician-bin/src/main.rs`); the rail says
*"at the latest the next time the service restarts"* and the endpoint names the
same finisher.

## 8. The run is not tailed

`subscribe_to_task` would set `current_tailed_task_id`, and queue-on-tail would
hold later messages until the build ends (up to the 3h watcher deadline).
Progress lives on the task surfaces.

## 8.1 The cockpit's run list is paged

`vibedev::run_pages::page_runs` serves `GET /vibedev/projects/{id}/runs` from the
project's `run_task_ids` + `active_root_task_id`
(`ArtifactV2Service::list_task_items_by_ids`), so a page costs one project's
records, not a walk of every internal task. A run = root task + threaded
follow-ups (`vibedev-threaded`; parent from `run_links[task].parent_task_id`,
else the `Parent task:` line), newest first by `updated_at` then id, paged by
`next_cursor` (`<updated_at>|<id>`). Tasks whose `VibeDev project:` line names
another project are dropped. Each admitted run is recorded
(`record_vibedev_project_run` from `record_vibedev_run_on_project`) in
`run_task_ids`, with a `VibeDevRunLink` (validated parent, `crew_id`, `lane`) in
`run_links`; only cockpit runs move `active_root_task_id`. Runs admitted before
this recorded nothing and fall back to the description line.

`vibedev::live_runs` serves glance surfaces (desktop notch,
`GET /vibedev/runs/live`): each open project's running, owner-waiting or
ended-within-a-window runs, one state each (`run_pages::fold_into_runs`).
Owner-waiting = a pending request names one of its turns, a diff awaits review,
or a live turn asks a question. Each row carries the root's `crew_id` and `lane`.

## 8.2 Crews

`vibedev::crews` runs one goal as parallel lanes on their own clones and
branches, so a crew (`VibeDevProjectRecord::crews`) needs a trusted project.

* **Planning.** Caller- or planner-supplied lanes (operation and prompt
  `vibedev_crew_plan` 1.1.0, compiled fallback), validated by
  `normalize_crew_lanes`: 1–8 lanes, distinct one-line labels, a brief each,
  repo-relative `owns` paths. The planner sees files on the project's
  `base_branch`, else the current branch (`trusted_workspace::base_branch_files`; ≤ 400 paths, else
  folder counts via `crew_plan_repo_layout`). A proposed crew asks
  `vibedev.crew_start` (`vibedev-crew-<crew>-start`, answerable from Attention
  and the notch) unless the caller started it.
* **Lanes** are ordinary `start_build`s (`created_by: vibedev_crew`,
  `pin_project_pointer: false`, crew id and lane on the run link);
  `lane_request` names goal, brief, claim and the other lanes' claims. Its first
  line is the lane's own task (work brief, race goal, or a fixed integrate/review
  line), since the run title comes from it. Key `<crew>-lane-<index>`, so a
  repeat replays.
* **Supervisor** (`spawn_vibedev_crew_supervisor`): applies `HitlResolved`
  answers, re-reads lanes when a lane run ends (only running lanes' root
  execution ids wake it), sweeps every minute (catching what a restart missed),
  restarts lanes an interrupted start left without a run (the key replays or
  names the admitted run). A lane without a task directory is stopped only once
  its intent is gone, `settled` or `failed`; while `pending`/`claimed` it waits,
  and a `pending` intent idle 30 minutes is failed first (generation-fenced) so
  recovery never starts a run the crew stopped watching. Status changes re-check
  under the project-store lock, so a stop is never undone by an in-flight start,
  merge or answer; a lane admitted mid-stop is recorded stopped and its run
  stopped.
* **Budget.** `budget_usd` ≥ $0.01; each lane's `cost_budget_usd` =
  `share_lane_budget` (remainder ÷ (starting + running lanes), whole cents),
  recorded by `CodingCostLedger` and enforced by the engine; when a share would
  be sub-cent, fewer lanes start and the rest wait `pending`. Integrator or
  review lanes (started after the others end) get the whole remainder. No lane
  starts once it is spent; each pass sums lane spend and at `budget_usd` stops
  every running lane.
* **Merge.** All lanes done → `ready`; branches merge in lane order into the
  crew's trusted chain (`trusted_workspace::integrate_branches`, branch
  `vibedev/<slug>-crew-<id8>`, `--no-ff`, already-merged lanes skipped) and
  publish. A failed merge (not a conflict) stays `ready` and retries (one sweep,
  then doubling, ≤ 30 minutes, per process); three failures in a row → one owner
  briefing; the owner's merge-again retries at once. Clean → `integrated`, asks
  `vibedev.crew_pull_request`.
* **Pull request.** That answer only records intent; the PR is the owner-only
  `pull-request` action (Attention answers may come from bots or plane grants),
  pushed with owner credentials via `open_pull_request` (secret scan kept).
  `git push` / `gh pr create` run in their own process group, killed after 120 s
  or when a forked helper holds an output pipe 10 s after exit. An empty or
  non-`http(s)` URL from `gh` is an error and is not saved.
* **Conflicts.** `conflict` names lane and files, keeps earlier-merged lanes
  unpublished, asks `vibedev.crew_resolve`. An integrator lane
  (`LaneRole::Integrate`) runs `git fetch origin <branch> && git merge` per work
  lane, resolves, runs checks, and its branch is merged. A failed integrator
  keeps `conflict` only while that conflict is open (merge-again clears it, so a
  later clean merge and review stand). At most three integrators
  (`MAX_INTEGRATOR_LANES`; one budget-stopped before a run is not a try; six
  ever added); then, or once the budget is spent, only manual resolution.
* **Review / race.** `review` adds a post-merge lane that resets onto the crew
  branch, reviews and fixes `git diff <base>...HEAD`, and is merged before the
  pull-request question. `race` (`CrewMode::Race`; `race_lanes`: one per named
  profile, else distinct engines first) gives every lane the whole goal and asks
  `vibedev.crew_choose`; only the chosen lane is merged.
* **Questions.** `CrewQuestion` ids `vibedev-crew-<crew>-<start|pr|choose>` and
  `vibedev-crew-<crew>-resolve-<ms>` (per conflict; a merge can conflict again)
  apply from `HitlResolved` or the sweep only while the crew waits on that id;
  the sweep re-files any never filed. A failed or stopped crew files one
  briefing; its lane runs skip their run-ended notice.

### Mobile run detail

`vibedev::run_detail` serves the mobile companion so native apps need not port
`spineModel.ts`: `run_turns` (root plus folded follow-ups; request from the
`VIBEDEV_USER_PROMPT` fence, engine from the durable log, `CodingCostLedger`
cost, trusted chains' `TrustedTurnRecord` commit ranges), `fold_timeline`
(durable, delta-coalesced `coding_events.jsonl` → message / reasoning / action /
error / completed / notice cards), `run_changes` (trusted:
`trusted_workspace::changed_files_between`, `file_patch_between`; sandboxed:
proposals named by `coding.completed`). `wire_fixtures` pins
`magician/tests/fixtures/vibedev/*.json` to server output
(`UPDATE_VIBEDEV_FIXTURES=1` regenerates).

### Steering a run by name

`@vibedev #steer <run>: <message>` (or `#steer <run> to <message>`), or spoken
"tell vibedev <run> to <message>", sets `steer` (`chat::invoke_grammar`); typed
`@vibedev tell …` stays a build. `vibedev::steer::steer_live_run` matches live
runs in scope (crew lane, title, project; exact name wins) and steers a single
match via the coding control registry; zero or several matches → a sentence
naming running runs, nothing starts.

## 9. Surfaces

`feature_surface_is_authorized(FeatureMode::Vibedev, …)` is true only for
`InvocationSurface::Chat` and `InvocationSurface::RealtimeVoice` (server-minted;
`chat_api`'s `rejects_client_selected_protected_surface`); `PublicEnvoy` stays
closed so strangers never spend compute. The `@`-picker offers `@vibedev` and
`@vibedev #discuss` on web (`ui/unified-ui/src/lib/magician/chat/composerMentions.ts`)
and iOS (`magios/Shared/MentionCatalog.swift`); no client interception.

## 10. Voice

```
[hey] <start|run|launch|begin|open> [a|the] <vibedev|vibe dev> <build|plan>  <request>
```

e.g. **"start a vibedev build fix the footer spacing"**; `plan` gives the
`#discuss` run. ASR renders `@vibedev` as *"at vibe dev"*, hence a whole phrase,
**leading position only**, same parser as the typed marker (so typing the phrase
works too); `code`/`coding` are not accepted. `plan` exists because `#discuss`
cannot be spoken. Each slot blocks a false positive (a real, costly build):

| Slot | Blocks |
| --- | --- |
| verb | "vibedev build failed on main", "the vibedev build is red again" |
| mode noun | "start vibedev on the login page", "start a vibedev review of the diff" |
| subject | "start a build of the docker image", "start a code build" |
| leading position | "can you start a vibedev build for the footer", "let's start a vibedev build …" |

| Road | Reaches `process_message`? | Works? |
| --- | --- | --- |
| Cascaded hands-free (`voice_orchestrator::process_cascaded_voice_turn`) | Yes, `InvocationSurface::RealtimeVoice` | **Yes** |
| Ambient dictation / voice notes | Yes, `InvocationSurface::Chat` | Yes, as chat |
| Vendor-realtime Live (**iOS default**) | **No**: transcripts persist as `ChatMessage`s (`media_rails/voice_orchestrator.rs::ingest_transcript_turn`) | **No** |

Vendor-realtime's only escape hatch is
`voice_control_handler::handle_tutor_takeover_transcript` (interrupts the
provider, suppresses audio, diverts to Tutor/App Copilot); coding would need its
own takeover branch, not a relaxed predicate.

**Authorization** needs both §9's surface check and the `FeatureMode::Vibedev`
arm sitting *above* `authenticated_realtime_voice` in
`chat_invocation_context_for_turn` (below it, the voice arm returns
`FeatureMode::None`); public-envoy stays first. Test:
`a_hands_free_voice_turn_reaches_the_vibedev_rail_and_is_authorized`
(`chat/service.rs`). `process_cascaded_voice_turn` speaks the
`reply_without_model` reply (on the `ChatResponse`) while the build runs on.

## 11. Starting is idempotent, and admission is durable

Every start goes through a **dispatch intent**
(`magician_v2/vibedev/dispatch_intent.rs`), so a retry buys no second run and a
crash between create and dispatch loses nothing. The per-scope record holds
key, request digest, task id, **the assembled task** (§11.1), execution id,
state (`pending` → `claimed` → `settled`/`failed`), recovery attempts, fencing
generation, timestamps.

**Key** (server-derived, length-prefixed):
`blake3(domain ‖ principal ‖ workspace ‖ chat_session_id ‖ chat_turn_id)`.
`chat_turn_id` is the turn's identity (the UI subscribes to
`/events?chat_turn_id=…`), unique only within a scope, hence scope and session
in the key; a missing one is minted as `chat-turn-<uuid>`, so that client's
retries are not deduplicated (a transport gap). Nothing a model writes reaches
it (it could split or merge runs).

**Conflict digest**:
`blake3(domain ‖ request ‖ mode ‖ parent_task_id ‖ project_id ‖ coding_choice)`.
Same key + same digest ⇒ existing task, nothing created or dispatched; different
digest ⇒ conflict refusal, task untouched. `parent_task_id` is in it because a
root run and a follow-up over the same words differ. `coding_choice` is `auto`
or `profile:<id>` (variant-prefixed, so a profile named `auto` cannot collide;
absent ≠ present). **Excluded:** title and
description (prompt edits must not cause conflicts), project `repo_path` and
name (the caller chose an identity), owner agent (config reloads), anything
time-derived.

**Task id**: `dispatch_task_id(key)` = `task_` + 32 hex, named before the task
exists; `ArtifactV2Service::ensure_task_with_id` is idempotent and rejects a
collision with differing content.

`admit` commits the journal before the `intents/` projection, so it checks
`journal.last_sequence` first, replays the record and repairs the projection
(else `validate_successor` would refuse a second sequence-1 admission and leave a
key admitted just before a crash permanently unreplayable). `find` tells **absent** from **unreadable**
(`Path::exists()` is also false for EIO/EACCES/ENOTDIR/unmounted volumes).

### 11.1 The intent carries the task

A **task plan** (assembled title and description, owner, raw scope, mode,
project id, parent link, continuation references) is written in the admission's
journal transaction; task creation is multi-write (workspace, `TaskCreated`, feed
item, progress row), so instead of being atomic with it, the intent suffices to
**rebuild** the task.

* Assembled prose, not inputs (mismatch: `caller_task_id_conflict`); coding
  choice carried (`skip_serializing_if`) so a moved default cannot change what
  recovery starts; continuation references stored so `depends_on` is stable.
* **Not carried:** thread id, tags, lifecycle, output mode, `created_by` —
  re-derived (`vibedev_run_task_tags`), so a recovered run lands on today's
  definition; only *whether* there is a parent is stored, never tag names.
* **Scope twice:** raw principal/workspace in the plan, sanitised segments on the
  record; recovery checks one sanitises to the other, so a mismatched payload
  cannot create a task in a scope that never admitted one.

### Crash windows

| Step | Crash leaves | Recovery |
| --- | --- | --- |
| 0 · assemble | nothing durable | nothing |
| 1 · admit | `pending`, no task | **creates from the plan, dispatches** |
| 2 · claim | `claimed`, no task | same |
| 3 · create (`ensure_task_with_id`) | `claimed`, task, no execution | **dispatches** (no re-create) |
| 4 · dispatch | `claimed`, task and execution | settles from execution state — **never dispatches twice** |
| 5 · settle | terminal, out of the outbox | nothing |

Every window from step 1 is finishable: an admitted request is never dropped.
No separate execution shell is committed (`start_execution` provisions it; a
crash before it is row 3), and the rail sets no project pointer (§7). Recovery
charges `recovery_attempts` (bound **3**; a count, not an age, so fast restarts
and clock jumps cannot evade it) durably in the claim's commit before the work,
then settles terminally with the reason; only recovery charges (`claim` and
`claim_for_recovery` are separate). `failed` is terminal: retrying that key gets the same answer; a new
attempt is a new turn and key.

### Restart recovery

`vibedev::run_service::recover_pending_vibedev_dispatch` (re-exported via
`vibedev::rail`; spawned from `magician-bin/src/main.rs` beside
`recover_pending_verification`) rebuilds projections from the journal and walks
the outbox. Each unfinished intent ends **dispatched**, **terminally settled**,
or **left to a live turn in this process**, tallied in `VibedevDispatchRecovery`
(`dispatched` ⊃ `recreated`, `already_running`, `settled_terminally`,
`exhausted`, `settled_scheduled`, `held_by_live_claim`, `skipped`,
`unreadable_keys`, `unreadable_scopes`; only `touched_anything()`, no `total()`,
as unreadable counters can double-count). A second pass is a no-op.

* **Nothing is decided before the claim**: every branch (dispatch, settle,
  rollback) reads the claimed record, under the `HeldByThisProcess` refusal and
  generation fence; the retry bound is checked there (one past the bound, since
  the claim charges this pass).
* **Scheduled runs are settled, never started or deleted** (`settled_scheduled`;
  the task keeps its cron; checks the plan's schedule and the created task's
  `manifest.schedule`, which older records have alone).
* Exhaustion unwinds only what it left (pointer, then orphan), never a run with
  an execution (`already_running` can charge an attempt and leave it claimed)
  or a scheduled one.
* One damaged key does not hide a scope: it is logged, named in
  `DispatchProjectionRepair::unreadable` and skipped; only failing to enumerate
  the scope is scope-wide.
* Pre-plan records (task id only) settle terminally; `task_plan` and
  `recovery_attempts` must keep `skip_serializing_if`, since the journal's
  integrity check re-serializes and compares hashes.

**Storage** mirrors `execution/verification/{store,journal}.rs` (not imported):
an append-only journal (atomic rename) is the truth; `intents/` and `live/` (the
outbox; the intent *is* the work item, "retired" when terminal) are
replay-repaired projections; a monotonic generation, re-checked under the lock,
fences stalled writers; every transaction asserts the record's next state.
**No TTL lease** (a claim spans one create plus one `start_execution` and never
outlives its process): claims record `<holder>@<instance>`
(`dispatch_claim_holder`, `dispatch_process_instance_id`), and
`claim_for_recovery` refuses this instance's own claims under the fence lock,
without writing or charging (the reconciler runs beside live turns; else a
loser's rollback, `archive_task_with_options` with `remove_files = true`, could
delete the winner's task); other instances' claims (or ones without `@`) are
reclaimable at once, so `recovery_attempts` advances at most once per process.
**Not closed** (needs a renewed lease): two processes over one data directory; a
claim this process abandoned without dying (refused until restart, logged at
`debug!`, counted `held_by_live_claim`).

## 12. Follow-ups continue the conversation's own last run

A follow-up gets its own task, key and row, chained to the parent so
`run_coding_task` reuses the chain root's coding session.

* **Parent = the conversation's own most recently admitted run**, within
  **twelve hours of admission** (longer than a 3h build, short enough that next
  morning starts fresh) — no `#follow` flag (every ordinary follow-up would start
  cold), never the cockpit's `active_root_task_id` (§7). An unrelated build in
  the window threads onto it (same project, recoverable); a terminally failed
  admission is not a run.
* **Text cannot reach it**: `resolve_vibedev_run_parent` takes no user text
  (service, scope, session id, resolved project id, this turn's key); it reads
  the scope's intent records (§11) filtered by the **server-minted** chat session
  id, compares projects by typed `DispatchTaskPlan.project_id` (not the
  `VibeDev project:` line), and excludes this turn's key (else a retry would
  nominate its own run, digest differently and turn a replay into a conflict).
* **Validation**: exists, same scope (the intent store is built on one scope's
  directory and re-checks every record; the task read is scoped), same project,
  a VibeDev run (`is_vibedev_cockpit_run`: rail or cockpit), inside the window.
  **Any miss starts a fresh root run (never reaching further back); none is an
  error**: a missing continuation costs a warm session, a wrong one runs against
  another chain.

Mirrors the cockpit (`submit.ts`): `VIBEDEV_FOLLOW_UP_TAG` =
`vibedev-follow-up` (same colour); `VIBEDEV_THREADED_TAG` = `vibedev-threaded`
(**always**: the rail's one follow-up gesture is the composer's Send, folding into
the chain root; the cockpit's Run button has no chat spelling); `promptTitle`'s
`VibeDev follow-up · …` prefix (for `stripRunTitlePrefix`); the
`continuationReferenceTaskIds` rule (parent only when `completed` and not
synthesizing) onto `depends_on` (where the create handler puts validated
`reference_task_ids`); `Parent task: <id>` from the reader's own `pub(crate)`
constant (the test asserts against the reader), placed **above** the fenced
prompt (cockpit: below; §4 excises the fence either way). One store-backed block
(`vibedev_rail_follow_up_context`, above the parent's status and title) replaces
the cockpit's four framings (plan/code × discuss/build), since the rail selects
rather than knows the parent's kind; its header (`VibeDev plan continuation:` vs
`VibeDev continuation context:`) follows the parent's tags. Root runs have no
continuation block.

## 13. A run that is waiting on you, and what voice says about checks

### 13.1 The pending approval is derived

A build stages a `CodeChangeProposal` and stops.
`TaskListItemV3.awaiting_diff_approval` is computed on **every read** from
`<scope>/code_change_proposals/` (not a one-shot `hitl.requested` event), so it
survives restart (as `list_attention_items` re-derives per poll). **A terminal
task never claims to be waiting**, even with an orphaned `Pending` proposal (the
attention feed drops it too). Wire otherwise unchanged: omitted while false; when true the feed card gains
`awaiting_diff_approval` metadata and `NeedsAction`. Set in **both** list-item
builders (`Internal` `@vibedev` runs reach only the lightweight one). UI:
[unified-task-panel.md](../unified-ui/unified-task-panel.md).

**One walk per request** (flat store, no by-task index, mostly diff text, on a
15s feed poll): two per-request snapshots (born and dropped within the request,
nothing to invalidate) —
`PendingDiffApprovals` (`<scope>/code_change_proposals/`,
`ArtifactV2Service::pending_diff_approvals`) and `PendingFileEditTransactions`
(`<scope>/transactions/`, `ArtifactV2Service::pending_file_edit_transactions`)
— are taken once and handed to `build_task_list_item`,
`list_tasks_with_pending`, `list_internal_tasks_with_pending`,
`task_progress_projection_with_pending`,
`announce_pending_diff_approvals_to_voice_with` and `list_attention_items`'s
HITL gate. No-argument forms (`list_tasks`, `list_internal_tasks`,
`get_task_progress_projection`, `announce_pending_diff_approvals_to_voice`,
`get_task_list_item`) and indexed page loads (`list_task_page_items`,
`list_internal_task_page_items`, one per page) take their own. A process-global
test counter in `CodeChangeProposalStore::scan_records` (before `read_dir`)
enforces it; asserting tests hold a mutex, since a thread-local cannot see the
`spawn_blocking` thread. The store peeks `task_id` + `status` before
deserializing, so another run's proposal never allocates its diff (a strict field
subset, so it only over-admits; the full parse re-filters; a byte search would
match diff text).

**No pruner**: nothing deletes a `ccp-*.json` (`reject`/`apply` rewrite in
place). A future one must prune only terminal (`Applied`/`Rejected`) records
with `resolved_at` past retention, never `Pending` or a **live task**'s; hold
`acquire_decision_lock`; respect the trusted-store authority (keyed on proposal
id); and outlast the revert window (`apply_root`/`snapshot_id` are the only
revert handle).

### 13.2 Announcing it once

`announce_pending_diff_approvals_to_voice` (run from `list_attention_items` on
its snapshot, so restart cannot lose it) pushes `task.awaiting_diff_approval`
to the live call by the run's `chat_session_id`, else **by scope** (cockpit
runs) via a `(principal, workspace) → voice_session_id` index on
`VoiceDownstreamFanout`, registered when a call binds (binding `chat_session_id`
on cockpit manifests would re-expose them to the task sweep); a test asserts no
cross-scope delivery. Because the snapshot's staleness matters for a durable
marker, it re-checks the proposal (`is_unclaimed_pending`, peek-only) as the
last statement before speaking, with no `await` between.

**A proposal is announced once, ever** (not per call or boot): an empty marker
per proposal id under `<scope>/spoken_hitl_announcements/`
(`artifact_v2::spoken_hitl_log`). No live call ⇒ no announcement **and no
marker** (the first poll that finds a call says it); the marker is written
**after** delivery (a lost marker costs one repeat, an early one the
announcement); a terminal task's orphaned proposal is skipped. `VoiceControlSession` injects the sentence (the cascaded
road speaks it); `realtimeVoiceClient.ts` has no case for it, so the
data-channel road stays silent while the card shows. `voice_diff_approval_waiting`
names the task's **title**, never its id, and says nothing without one.

### 13.3 `unknown` must say nothing about checks

`verification_state_for_task`: **missing state reads as `Unknown`, never
`Verified`** — common, as state exists only when the controller gated the run
(`verification.mode: observe` or `enforce`, or
`MAGICIAN_VERIFICATION_CONTROLLER`; see
[verification controller](verification-controller.md)). The completion
announcement (`task_completed_message`'s `verification_state`) appends one
store-backed clause:

| State | Voice says |
| --- | --- |
| `verified` | *"Its code checks passed."* |
| `unverified` | *"Nothing checked that code, so I can't tell you whether it works."* |
| `exhausted` | *"Its code checks kept failing and the repair attempts ran out, so that code is not working yet."* |
| `unknown` | **nothing at all** |

Templates `voice_verification_verified`, `voice_verification_unverified`,
`voice_verification_exhausted`; **no `voice_verification_unknown`**
(`verification_voice_clause` returns `None`; a test asserts no "check", "verif",
"passed", "repair", "proven", "tested"). `repairing`, `verifying`,
`unavailable`, `cancelled` and `blocked_partial` are silent too (an announcement
fires once). "Passed" appears only in the verified clause, so `exhausted` cannot
sound like success. **No compiled fallback** (unlike §5), so a drifted copy can
never claim checks passed: a missing template means no clause, as `unknown`.

Coding run handoff records use the durable atomic byte writer (unique staging
files, file/parent fsync); continuation and branch-diff provenance never assign
the backend service name to an assistant.
