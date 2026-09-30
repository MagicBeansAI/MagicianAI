# `@vibedev` — the rail into VibeDev

> Typing `@vibedev <request>` in ordinary chat starts a VibeDev run instead of
> producing a chat reply. `@vibedev #discuss <question>` starts a plan-only run.
> Saying **"start a vibedev build …"** hands-free does the same thing. A second
> `@vibedev` in the same conversation continues the first (§12).
> The cockpit starts its builds through the same server-owned service (§6.1).

**What this is not.** `@vibedev` is an explicit way to *start* a build. It is **not
a boundary**: it fences nothing off, and an agent that already holds `shell` can
still write to a repository without passing through here. A fence of that kind
was judged not achievable as posed; nothing below claims otherwise.

**Design:** `docs/archive/plans/2026-08-10-vibedev-code-rail-design.md` ·
**Plan:** `docs/archive/plans/2026-08-10-vibedev-code-rail-phase1.md`

---

## 1. What happens to a `@vibedev` turn

| Step | Where |
| --- | --- |
| The invoke is recognized — typed marker or spoken phrase | `magician_v2/chat/invoke_grammar.rs` — `parse_vibedev_rail_invocation` |
| The turn is classified `FeatureMode::Vibedev`, keeping the surface it arrived on (`Chat` or `RealtimeVoice`) | `chat/service.rs` — `chat_invocation_context_for_turn` |
| The turn diverts, after the user turn is persisted and before any model call | `chat/service.rs` — `process_chat_inline_turn` → `start_vibedev_rail_run` |
| The project is resolved | `VibeDevRunService::resolve_project`, over `magician_v2/vibedev/projects.rs` — `resolve_vibedev_project_for_session` |
| The turn is decided — start, refuse, or ask | `magician_v2/vibedev/rail.rs` — `decide_vibedev_rail_turn` |
| The owner is resolved from `coding.lead_agent_id` | `magician_v2/agents/task_ownership.rs` — `resolve_created_task_owner_agent_id` |
| Description, tags and title are assembled; the run is **durably admitted**, created and dispatched | `magician_v2/vibedev/run_service.rs` — `VibeDevRunService::start_build` |
| The turn answers and detaches | `chat/service.rs` — `reply_without_model` |

No model is consulted. The marker **is** the intent.

**The rail is a surface, not the machinery.** Everything between "a build should
happen" and "a task is running" — prose, durable admission, atomic create, typed
follow-up link, dispatch, rollback — belongs to `VibeDevRunService` (§6). The
rail recognizes the invoke, decides what was asked, hands the service a
`StartVibeDevBuild`, and says what happened.

### The seam, and why it is there

`process_chat_inline_turn` is the one function every inbound chat turn converges
on (non-streaming, SSE streaming, Tutor background continuation); diverting in
one HTTP entry point alone would leave the others un-railed. The divert sits at
its top: the user turn is already in the transcript, the Tutor lane has not run,
no model has been called.

**Module tree.** Product logic lives in `magician_v2/vibedev/` — `projects.rs`,
`dispatch_intent.rs`, `run_service.rs`, `rail.rs` — behind the 1.2 lane seam; it
is not an app package, and the coding engines it calls stay core
infrastructure. Chat service keeps orchestration (arm ordering, divert
placement, session listing, dispatch, logging). The rail module owns the
VibeDev arm's decisions: `rail_turn_lane` (lane triple a leading invoke mints),
`rail_admits_turn` (divert guard), `vibedev_rail_reply_key` (which sentence
answers which `start_build` outcome), `hot_chat_tools` (the lane's hot set), and
in `vibedev::run_service` the queued-turn coding-choice mappings
(`queued_coding_choice` / `vibe_coding_choice_from_queue`) that bound a queued
turn's engine choice across replay.

## 2. Which project a build lands in

Four tiers, in order (`magician_v2/vibedev/projects.rs` — `select_vibedev_project_for_session`):

1. the project whose `chat_session_id` **is** this chat session — silently;
2. exactly one non-archived project in the scope → that one, silently;
3. several non-archived projects and the session owns none → **the scope's
   active project when the cockpit is pointed at one**, else **ask** and start
   nothing (§2.1);
4. no non-archived project → **refuse**.

Archived projects are never resolved, counted toward ambiguity, or offered.

**The rail never creates a project.** The cockpit's list endpoint mints a
project record for an unclaimed cockpit session; a chat turn may only *find*
one. A never-listed cockpit session therefore resolves to nothing here —
deliberate; do not "fix" it into a write from chat.

### 2.1 The ask, and why the line falls where it does

`active_vibedev_project` returns the first non-archived project whose **cockpit
session is open**, else (no session open) the first in display order
(`updated_at_ms` desc). Only the first is a pointer the user set — picking a
project in the cockpit `POST`s `/vibedev/projects/{id}/activate`, which sets its
session `active` and bumps `updated_at_ms`. The second is an accident of
timestamps. So the rail acts on the active project whenever a pointer exists,
and asks only when several live projects exist and no cockpit session is open.

**How the user answers:** pick the project in the cockpit and re-send the turn.
No `@vibedev in <project>` grammar: free-text name matching can mis-match and
start a real build in the wrong repo, the cockpit already owns "which project",
and the spoken grammar would have to carry project names through ASR.

**The rail must never disagree with the cockpit.** At tiers 2/3 it uses exactly
what `active_vibedev_project` returned — the same function
`project_list_response` derives the cockpit's `active_project_id` from — and
the open-session test is one shared predicate
(`vibedev_project_session_is_open`). The rail narrows *when* it acts on that
answer; it never computes a second one.

**The question** lists candidates in cockpit display order, as name plus repo
path when the name is not unique (adopted sessions are all "VibeDev Project").
It stops at six with "…and N more" because it is read aloud on the hands-free
road (§10); the count stays the true total.

**Residual:** when several live projects all have open sessions, the rail
follows the cockpit's pointer (most recently activated) without asking — that is
the project on screen.

## 3. Refusals — each creates nothing

| Case | Reply |
| --- | --- |
| No non-archived project in the scope | Explains that `@vibedev` only finds projects, and points at the cockpit |
| Several live projects and no pointer at one (§2.1) | **A question, not a refusal**: names the candidates and says to pick one in the cockpit and re-send |
| `@vibedev` with no request after it | Shows the shape, including `#discuss` |
| `coding.lead_agent_id` unset | Says so; the rail will not pick an owner for a coding run |
| Create or dispatch failed | Reports the original error and confirms nothing was left running |
| The turn's idempotency key was reused with a **different** request | Names the run that key already started and says it is untouched — see §11 |

The key-conflict reply is deliberately distinct from the failure reply: the
failure reply promises *"nothing was left running"*, which is false during a
conflict — the earlier run is still going.

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
  lead).

### The one line that must not break

The description carries, verbatim:

```
run_coding_task repo_path: <path>
```

`agents/runtime.rs` (`build_vibedev_coding_delegate_goal`) string-matches that
prefix back out and **falls back to `.` when absent** — a missing or misspelled
line silently starts the engineer in the wrong directory. The prefix is one
`pub(crate)` constant (`VIBEDEV_REPO_PATH_LINE_PREFIX`) shared by reader and
assembler; tests read the path back the way the runtime does.

### The request is data: control lines are read from the server's half only

The user's request sits **verbatim** in the same string as three control lines
that are read back and acted on:

| Line | Decides | Read by |
| --- | --- | --- |
| `run_coding_task repo_path: <path>` | **which repository a build runs against** | `build_vibedev_coding_delegate_goal` |
| `VibeDev project: <uuid>` | which project a run, a Citizen API call and a `contribute_to_project` write bind to | `parse_vibedev_project_id` |
| `Parent task: <id>` | which chain the run threads onto, and so which coding session it inherits | `parent_task_id_from_description` |

Readers take the **first** match, so an unguarded request containing a
control-shaped line would win. The fence's end marker (`VIBEDEV_USER_PROMPT`) is
also a substring of its begin marker (`<<<VIBEDEV_USER_PROMPT`), so a request
could close the fence early.

**Guard.** Every reader goes through `vibedev_trusted_control_region`
(`agents/runtime.rs`), which cuts the request region — from the first begin
marker through the **last** line exactly equal to the end marker — before any
line is matched. Inside the fence, the server's closing line is last, so a
forged marker can only make the cut *longer*; a longer cut can only drop a
control line (readers fall back: repo `.`, no project, no parent = root run),
never promote a forged one.

**Post-fence invariant (a security requirement — do not remove as tidying):**
the server appends client-controlled fields *after* its closing line, and a
newline in one could write an end marker below the server's, **repositioning**
the cut and exposing a forged `repo_path` line. So *every client-controlled
value interpolated after the fence is normalised to a single line* by
`vibedev::run_service::vibedev_line_break_free`, which replaces `\n`, `\r`,
U+0085, U+2028, U+2029 with a space **and changes nothing else**. It does not
collapse whitespace: these fields are read back as data (ids, mime types,
filenames) and pinned by byte-identity tests against the client assembler.

| field | where it lands | why it needs covering |
| --- | --- | --- |
| attachment `attachment_id`, `mime_type` | post-fence | raw from the request body |
| attachment display name (`label \|\| filename`) | post-fence | raw; emptiness is tested on the **raw** label so `"   "` still renders as itself, matching the client's `\|\|` truthiness |
| `attachment_session_id` | post-fence | raw |
| seed **label** | post-fence | raw |
| `coding_profile_id` | post-fence, in the escalation-policy line | client-supplied and **unvalidated — no allowlist**. Normalised at the API boundary (stored/digested value) *and* at the interpolation site (description), deliberately redundant |
| parent **title** | *above* the fence in the rail assembler, below it in the cockpit's; normalised at both sites | see below |

Not needing the normaliser: `attachment_ids` (`serde_json`-encoded, escapes
newlines — do not "simplify" into a plain join), `preview_url` (round-trips
`url::Url::parse`), `chat_session_id` and `project_id` (server-minted). The
project name is one line via `normalized_project_name` at store time; a cockpit
parent's `summary` goes through `vibedev_cockpit_trim_one_line`.

**Parent title** sits *above* the fence in the rail layout, where excision keeps
everything verbatim, so a forged control line there would win on first-match
ordering alone. Reaching it requires creating a task with a chosen title in the
scope, which is not the same as "cannot redirect the build" — hence the
normaliser.

Invariants:

* **The cut is not the prompt extractor; do not unify them.**
  `extract_marked_vibedev_user_prompt` takes the **first** end marker (never
  swallow server lines into the engineer's prompt); excision takes the last
  (never leave user text behind).
* **Server-side on purpose**, so it covers every assembler, including the
  unchanged TypeScript one and any other client.
* **No fence ⇒ untouched** (legacy/non-cockpit tasks). An *unclosed* fence is
  cut to end of string — fail-closed onto defaults.

**Known residual: `seed_content`.** A cockpit run's seed (meeting transcript,
chat thread) is multi-line by definition and emitted *above* the project block,
so a control-shaped line or end marker in it wins with no trick. Closing it
needs a nonce-delimited fence; it is not closed.

**Keep the table complete.** Any new field interpolated into an assembled
description belongs in the table or in the residual. A wrong "closed" is worse
than an honest gap.

### Autopilot is not reachable from chat

The rail never adds the `autopilot` tag (unattended, applies its own diffs). The
tag is added only for `DispatchMode::Autopilot`, which only the cockpit's mode
switch produces; `DispatchMode::from_discuss`, all the rail can call, returns
`Build` or `Plan`. Tests still assert its absence in both rail modes, so a future
third mode has to notice.

### Why the bare word `vibedev` does not match, and why the guard stays

Unlike `@tutor`/`tutor` and `@copilot`/`copilot`, bare `vibedev` does not match:
"vibedev is stuck again" is an ordinary sentence that must not spend compute.
The marker is matched by **tokenizing, in leading position**, not `contains`, to
keep out a longer handle (`@vibedev-review`), a quoted example, and a
mid-sentence mention. Speech gets a multi-word phrase instead (§10).

### One reader for the invoke

`parse_leading_feature_marker` (the authorization-grade classifier) does **not**
list `"@vibedev"` itself; it calls `parse_vibedev_rail_invocation`, so there is
exactly one `@vibedev` decision. The spoken phrase lives inside that same
function for the same reason — adding it to `parse_leading_feature_invocation`
beside `hey tutor` would let the classifier say `Vibedev` while the rail failed
to parse the text and refused with an empty-request message.

## 5. Where the prose lives

LLM-facing and user-facing text is in the prompt store
(`data/magician_v2/prompts/`), not Rust:

| Prompt | Use |
| --- | --- |
| `vibedev_rail_project_context` | Prose half of the project-context block |
| `vibedev_rail_plan_directive` | The Discuss/plan directive |
| `vibedev_rail_reply_started` | Dispatch acknowledgement |
| `vibedev_rail_reply_no_project` | Refusal — no project |
| `vibedev_rail_reply_empty_prompt` | Refusal — no request |
| `vibedev_rail_reply_no_coding_lead` | Refusal — no configured owner |
| `vibedev_rail_reply_start_failed` | Create/dispatch failure — **promises "nothing was left running"**, so only paths that created nothing or rolled back may use it |
| `vibedev_rail_reply_admitted_not_started` | Durably admitted, not dispatched (§7) — neither of its neighbours is true |
| `vibedev_rail_reply_key_conflict` | Refusal — the turn's key was reused with a different request |
| `vibedev_rail_reply_ambiguous_project` | Question — several live projects, no pointer at one (§2.1) |
| `vibedev_rail_follow_up_context` | Prose half of the continuation block (§12) |

Each has a compiled fallback so a missing template degrades rather than
bricking the rail. Spoken post-start prose lives under `voice_*` names with the
opposite fallback rule (§13): `voice_diff_approval_waiting`,
`voice_verification_{verified,unverified,exhausted}`.

**Deliberately not in the store:** the machine-read lines (`VibeDev project:`,
`run_coding_task repo_path:`, §4), and any "Execution policy" block — a build
run already gets the server-authoritative `vibedev_execution_policy` appended at
execution start (`artifact_v2/service.rs`, gated on
`is_vibedev_coding_build_run`); a second copy could drift and would contradict
the plan directive on a plan run.

## 6. One server-owned creation path

`VibeDevRunService` (`magician_v2/vibedev/run_service.rs`) is **the** way a
VibeDev run is created. `start_build(StartVibeDevBuild)` is its only public
entry; `admit_create_and_dispatch` is module-private so a second creation path
cannot be written by accident. **Three callers**: this rail, the cockpit (§6.1),
and restart recovery (rebuilds through `vibedev_run_create_task_input`).

`StartVibeDevBuild` carries **trusted context and validated choices only**:
scope, chat session id, chat turn id, owner agent id, resolved project, request
verbatim, mode, optional `VibeDevCodingChoice`, optional `VibeDevCockpitRun`
(§6.1; `None` on chat turns). No task id, title or description — those are
derived by one assembler.

### `VibeDevCodingChoice`

Defined by the handoff plan
§4 and the Codex app-server plan
§2/§6.1: `Auto` or `Profile { profile_id }`. Semantics: omitted means default;
every surface sends the same type. Wire form: `{"kind":"auto"}` /
`{"kind":"profile","profile_id":"…"}`, engine-neutral, one immutable choice per
request. Browser-boundary type is `ClientVibeDevCodingChoice` with
`deny_unknown_fields`.

An omitted choice **inherits the launching engine** first:
`bind_coding_constraint` maps the launching turn's or run's `RunEnginePin` (see
[plane.md](plane.md) "Run engine pin") onto the catalog via
`inherited_catalog_profile` — the Ready entry for Claude, Codex, Grok or
Antigravity, or for a Pi pin the Pi entry on its LLM profile — source
`inherited_run`. Otherwise (native loop, engine not Ready, unmapped Pi profile)
the configured default stands, source `configured_default`.

It is **not** `CodingEngineConstraint`: that is derived from this choice plus
the live `coding.profiles` catalog, pinned on `DispatchTaskPlan` before admit,
and enforced by `run_coding_task` on the VibeDev task and its parents. The
adapter is constructed from the journaled engine. Pi session/model fields stay on
`request.pi`; fabricated `pi_binary` / `session_name` / `persist_session` on
VibeDev tasks are rejected. Events stream only through the sink (returning
`event_count`); checkpoints resume from `native_session_id` with a
`pi_session_id` fallback. A filesystem snapshot publishes `codex-default`; a
cached qualification receipt may overlay that identity. The composer shows only
selectable Pi profiles unless the kill switch is on (then a Ready
`codex-default` row is selectable); Auto is an opt-in last row; Pi is the
default. Chat, composer-voice and hands-free `session.start` send the persisted
`coding_choice`. The rail never reads an engine name out of prose.

### 6.1 The cockpit

`POST /api/magician/v2/vibedev/runs` (`magician-api/src/vibedev_api.rs` —
`start_vibedev_run_handler`) calls the same `start_build`. The client sends
inputs; the server composes the prose. Create, pin, dispatch and rollback are
one `start_build`.

Browser preferences are sent as inputs; the client owns the **preference**, the
server owns the **prose**:

| Block | Sent instead as | Persistence |
| --- | --- | --- |
| `vibeStudioStore.policyPromptLine()` | `mode` + `auto_apply` | `localStorage` — `magician.vibedev.autoApplyCodeProposals` |
| `vibeStudioStore.visualSelfCorrectDirectiveBlock(…)` | `visual_self_correct` + `project_is_visual` | `localStorage` — `magician.vibedev.visualSelfCorrect.auto` |
| `vibeStudioStore.costBudgetLine()` | `cost_budget_usd` | `localStorage` — `magician.vibedev.costBudgetUsd` |

**Two layouts, one assembler.** `vibedev_run_task_description` branches on
whether a `VibeDevCockpitRun` was passed. The cockpit layout is byte-identical
to the former client assembler (`buildCodingTaskDescription`), pinned by
`the_cockpit_build_description_is_byte_identical_to_the_client_assembler`.
Port details commented in code: an absent optional block contributes an **empty
line**; JS `toFixed` rounds ties **upward** (Rust `{:.0}` rounds to even);
timestamps are millisecond-`Z` ISO strings.

**`VibeDevCockpitRun` carries:** staged attachments and their session, seed
context, `@task` chip references, a cron `schedule`, `save_as_task`, an explicit
`parent_task_id` with a `threaded` flag, `created_by`, and `pin_project_pointer`.

**The parent:** the rail *infers* one (§12) and a miss silently starts a root
run. The cockpit *names* one: the server loads it, checks it is an in-scope
VibeDev run, derives title/status/execution/summary from the record (never the
request), and **refuses** if it does not validate. Inference is not run for the
cockpit — all cockpit runs in a project share one `#vibedev` session, so "last
run" would chain every build.

**Cockpit prose is compiled Rust** (constants in `vibedev/run_service.rs`), not
the prompt store, so it exists in one place and stays checkable by the
byte-identity test.

`VibeLegacyStudio.svelte` (behind `?studio=0`) still has its own private copy of
the description builder; it is not reached by the default cockpit.

### 6.2 The cockpit's idempotency key

Same derivation as the rail's (§11), over different components:

```
blake3(domain ‖ principal ‖ workspace ‖ project.chat_session_id ‖ client_run_id)
```

`client_run_id` is minted by the composer per submission and re-sent on
re-send, so a re-submit returns the existing run and **does not dispatch a
second multi-hour build**. A genuinely new submission is a new run. A client
that omits the field gets a fresh id per call and is not deduplicated.

## 7. Failure and rollback

Create and dispatch are one step. A dispatch failure deletes the task (otherwise
it sits as an uncancellable `pending` run) and reports the **original** error —
cleanup failure never masks the root cause. It also records the failure
terminally on the dispatch intent (§11) so restart recovery, which rebuilds
missing tasks from the intent (§11.1), does not resurrect it. If that
best-effort write is itself lost (the journal is failing), a restart could start
a build the user was told had not started; accepted, because refusing to rebuild
any missing task would lose every genuinely admitted run.

**Project pointer ordering is load-bearing.** A cockpit run pins itself as the
project's `active_root_task_id` after the task exists and before dispatch. A
dispatch failure **unwinds the pointer before deleting the orphan**, and the
unwind is not gated on the delete succeeding, so the pointer never names a
deleted task. Live path and recovery share `rollback_undispatched_vibedev_run`.

**The unwind clears the pointer only if it still names this run**
(`VibeDevProjectPointer::PinTo(task_id)` / `ClearIfNames(task_id)`): otherwise
unwinding a crashed run A would clear the pointer a later run B set.

**`projects.json` writes:**

* Both publishers (`mutate_project_store_at`, `save_project_store`) write a
  **uniquely named** temp, fsync it, rename, fsync the directory, and remove the
  temp on failure. A torn store makes `load_project_store` hard-error (every
  project endpoint 500s) and the rail report "no project".
* **One lock in front of every writer.** Each writer rewrites the whole file, so
  a lost update republishes the loser's entire store. Seven writers: the four
  project endpoints (create, activate, update, delete), the deployment recorder,
  `GET /vibedev/projects` (it mints records), and
  `set_vibedev_project_active_root_task_id`. The lock is a process-wide registry
  keyed by store path (`project_store_lock`) — scopes never wait on each other;
  in-process suffices because one process owns a data root.
* It is a `tokio::sync::Mutex` held **across** the chat-store awaits between
  load and save (narrowing it reopens the window), which is why
  `set_vibedev_project_active_root_task_id` is `async` and uses `tokio::fs`.
  Readers (`read_vibedev_projects`, `load_vibedev_project`, the citizen resolver)
  take no lock: the file is replaced atomically.
* Regression test `a_rename_and_a_pointer_pin_do_not_overwrite_each_other` races
  a rename against a pointer pin on different projects (racing a writer against
  itself proves nothing).

`@vibedev` sets no pointer at all (passes no `VibeDevCockpitRun`) — a chat aside
must not move what the cockpit shows.

### "Nothing was left running" is a promise, so it is not said when it is false

A replay of `AlreadyAdmitted(pending)` returns `Replayed { execution_id: None }`
— dispatch was never reached, so neither "started" nor "start failed" is true.
That state is `VibeDevRunStartError::AdmittedNotStarted` /
`vibedev_rail_reply_admitted_not_started`: recorded, not started, nothing to
clean up, a resend will not buy a second run. The rail uses it for that error and
for any admission with `execution_id: None`. `POST /vibedev/runs` answers
**202 Accepted**.

**What finishes it.** The claim is taken before `ensure_task_with_id`, so the
reply's task id is derived and does not resolve yet. Nothing in the running
process creates it — the live path claims only freshly `Admitted` intents, so
resubmits are answered as replays. The finisher is
`recover_pending_vibedev_dispatch`, spawned **once per process at startup** from
`magician-bin/src/main.rs`. So this 202 means "started at the next process
start"; the rail's sentence says *"at the latest the next time the service
restarts"*, and the endpoint names the same finisher.

## 8. The run is not tailed

`subscribe_to_task` would set `current_tailed_task_id`, and queue-on-tail would
hold every later message in the conversation until the build is terminal (up to
the 3h watcher deadline). Silencing chat for hours is the wrong trade; progress
lives on the task surfaces.

## 9. Surfaces

`feature_surface_is_authorized(FeatureMode::Vibedev, …)` is true for
`InvocationSurface::Chat` **and** `InvocationSurface::RealtimeVoice`, false
elsewhere. `RealtimeVoice` is server-minted — `chat_api`'s
`rejects_client_selected_protected_surface` refuses it from a request body.
`PublicEnvoy` stays closed: a stranger must never spend the deployment's
compute on a build.

The composer `@`-picker offers `@vibedev` and `@vibedev #discuss` on web
(`ui/unified-ui/src/lib/magician/chat/composerMentions.ts`) and iOS
(`magios/Shared/MentionCatalog.swift`). Picker entries only: the marker travels
in the text and the server dispatches; no client-side interception.

## 10. Voice

### The phrase

```
[hey] <start|run|launch|begin|open> [a|the] <vibedev|vibe dev> <build|plan>  <request>
```

* **"start a vibedev build fix the footer spacing"** → a build.
* **"start a vibedev plan should we split this panel"** → the `#discuss` run.

ASR renders `@vibedev` as *"at vibe dev"*, so voice gets a whole phrase, in
**leading position only**, matched by the same parser as the typed marker. The
subject is the product name only; `code`/`coding` are not accepted. Every slot
exists because a false positive starts a real (recoverable but costly) build:

| Slot | The ordinary sentence it stops |
| --- | --- |
| the verb | "vibedev build failed on main"; "the vibedev build is red again" — an engineer narrating CI, not asking for anything |
| the mode noun | "start vibedev on the login page"; "start a vibedev review of the diff"; "start a vibedev session with the team" |
| the subject | "start a build of the docker image" — in this repository a "build" is at least as likely to be a container image. Also "start a code build", which is not the rail's name |
| leading position | "can you start a vibedev build for the footer"; "we should start a vibedev build once the tests pass" |

Consequences: "let's start a vibedev build …" does not fire (same leading rule as
typing); and because there is one grammar, **typing** the phrase works too. `plan`
exists because `#discuss` cannot be spoken, and the plan run is the safer mode.

### Which voice road this enables

| Road | Reaches `process_message`? | `@vibedev` / the phrase |
| --- | --- | --- |
| Cascaded hands-free (`voice_orchestrator::process_cascaded_voice_turn`) | Yes → `InvocationSurface::RealtimeVoice` | **Works.** This is the road the feature enables |
| Ambient dictation / voice notes | Yes → `InvocationSurface::Chat` | Works — it is an ordinary chat turn |
| Vendor-realtime Live (**the iOS default**) | **No** — transcripts persist straight as `ChatMessage`s (`media_rails/voice_orchestrator.rs::ingest_transcript_turn`) | **Does not work** |

On the vendor-realtime road no feature parsing happens. The only escape hatch
there is `voice_control_handler::handle_tutor_takeover_transcript` (interrupts
the provider, suppresses audio, diverts to Tutor/App Copilot); a coding
equivalent would need its own takeover branch, not a relaxed predicate.

### Voice authorization

Both, or neither works:

1. **The predicate.** `feature_surface_is_authorized` admits `Vibedev` on
   `RealtimeVoice` (§9).
2. **Arm ordering.** The arm minting `FeatureMode::Vibedev` sits *above* the
   `authenticated_realtime_voice` arm in `chat_invocation_context_for_turn` and
   keeps the arrival surface; below it, the voice arm would match first and
   return `FeatureMode::None`.

The public-envoy arm stays first. Test
`a_hands_free_voice_turn_reaches_the_vibedev_rail_and_is_authorized`
(`chat/service.rs`) asserts both halves together.

### The spoken reply, and detaching

`reply_without_model` persists the assistant message and returns it on the
`ChatResponse`; `process_cascaded_voice_turn` speaks exactly that. The build runs
on without holding the call (§8). `vibedev_rail_reply_empty_prompt` names both
spellings and drops markup, since it is the refusal hands-free callers hear most.
The started reply names the 32-hex task id, tedious aloud; one template serves
both roads, and a deployment can edit `vibedev_rail_reply_started`.

## 11. Starting is idempotent, and admission is durable

Every start goes through a **dispatch intent**
(`magician_v2/vibedev/dispatch_intent.rs`) so a retried turn does not buy a
second multi-hour run and a crash between create and dispatch does not lose it.
The per-scope durable record holds the idempotency key, request digest, task id,
**the assembled task** (§11.1), execution id, state
(`pending` → `claimed` → `settled`/`failed`), recovery-attempt count, fencing
generation and timestamps.

### The key is derived by the server, never supplied

```
blake3(domain ‖ principal ‖ workspace ‖ chat_session_id ‖ chat_turn_id)
```

Fields are length-prefixed. `chat_turn_id` is the turn's existing identity (the
UI subscribes to `/events?chat_turn_id=…`), so a retry re-sends it; a missing one
is minted as `chat-turn-<uuid>`. Scope and session are folded in because
`chat_turn_id` is only unique within a scope. Nothing a model writes reaches the
key (a model-supplied key could split or merge runs).

**Limit:** a client that omits `chat_turn_id` gets a fresh one per call, so its
retries are not deduplicated — a transport gap, not a storage one.

### The conflict is a digest, not a field-by-field compare

```
blake3(domain ‖ request ‖ mode ‖ parent_task_id ‖ project_id ‖ coding_choice)
```

Same key + same digest ⇒ return the existing task, create/dispatch **nothing**.
Same key + different digest ⇒ conflict refusal, existing task untouched.

**Decided against the journal, not the projection.** `admit` commits the
journal transaction and *then* writes the `intents/` projection, so a crash
between them leaves a key durably admitted but invisible to `find`. A second
admission at sequence 1 would be refused by `validate_successor` on an
append-only journal, leaving the key permanently unreplayable. So `admit`
consults `journal.last_sequence` first, replays the record, and repairs the
projection. `find` distinguishes **absent** from **unreadable**
(`Path::exists()` is false for EIO/EACCES/ENOTDIR/unmounted volumes too).

`parent_task_id` is digested because a root run and a follow-up over the same
words are different work. `coding_choice` is the canonical token of
`VibeDevCodingChoice` — `auto` or `profile:<id>`, variant-prefixed so a profile
named `auto` cannot collide; absent and present hash differently.

Deliberately **excluded**:

| Excluded | Why |
| --- | --- |
| Task title and description | Both derived from the request, and the description embeds store-backed prose. A prompt-store edit between a call and its retry must not manufacture a conflict out of an identical request |
| The project's `repo_path` and name | The caller chose a project *identity*; its contents are the project's business |
| The owner agent | Read from `coding.lead_agent_id` at start time, so a config reload would otherwise turn a retry into a conflict |
| Anything time-derived | A digest that moves on its own is not a digest |

### The task id is derived, not minted

`dispatch_task_id(key)` is `task_` + 32 hex, so **the intent names the task
before it exists**. Creation goes through `ArtifactV2Service::ensure_task_with_id`,
which is idempotent and rejects an id collision with differing content — turning
"created, then crashed" into something recovery can find.

### 11.1 The intent carries the task, not just its id

The record holds a **task plan** — assembled title and description, owner agent,
raw scope, mode, project id, parent link, continuation references (§12) —
committed in the *same journal transaction* as the admission. Task creation is
multi-write (workspace, `TaskCreated`, feed item, progress row), so instead of
making it atomic with the intent, the intent is made sufficient to **rebuild**
the task.

* **Assembled prose, not inputs.** `ensure_task_with_id` rejects field mismatches
  as `caller_task_id_conflict`, so re-deriving store-backed prose at recovery
  would turn a prompt-store edit into a failed recovery; and a request is fixed
  when made.
* **The coding choice** is carried too (with `skip_serializing_if`), so a moved
  deployment default cannot change what recovery starts.
* **Not carried:** thread id, tags, lifecycle, output mode, `created_by` — the
  service re-derives them (`vibedev_run_task_tags` is the one place), so a
  recovered run lands on today's definition. The record stores *whether* there is
  a parent, never the tag names.
* **Continuation references are stored**, because they are a fact about the
  parent at request time; recomputing would change `depends_on`, which
  `ensure_task_with_id` compares exactly.
* **Scope twice:** the plan holds the **raw** principal/workspace (what the
  manifest gets); the record holds the **sanitised** directory segments. Recovery
  checks the first sanitises to the second, so a mismatched payload cannot create
  a task in a scope that never admitted one.

### The order, and what each crash window costs

| Step | Crash after it leaves | Recovery does |
| --- | --- | --- |
| 0 · assemble | nothing durable | nothing — a turn that never happened |
| 1 · admit | intent `pending`, no task | **creates the task from the plan and dispatches it** |
| 2 · claim | intent `claimed`, no task | same |
| 3 · create (`ensure_task_with_id`) | intent `claimed`, task exists, no execution | **dispatches** (and does not re-create) |
| 4 · dispatch | intent `claimed`, task exists, execution exists | settles from the task's own execution state — **never dispatches twice** |
| 5 · settle | terminal; out of the outbox | nothing |

From step 1 every window is *finishable*: a request the caller was told was
durably admitted must not be dropped. There is no separate execution shell to
commit (`start_execution` provisions it; a crash before it is row 3), and the
rail sets no project pointer (§7).

### The retry bound

Recovery charges `recovery_attempts`, bounded at **3**, then settles terminally
with the reason. An attempt count rather than an age ceiling, because the only
infinite loop is a crash loop, and a count charged *durably in the same commit
as the claim, before the work* bounds it where wall-clock cannot (fast restarts,
clock jumps). Only recovery charges: `claim` (live turn) and
`claim_for_recovery` are separate calls.

### Restart recovery

`vibedev::run_service::recover_pending_vibedev_dispatch` (re-exported through
`vibedev::rail`) is spawned from `magician-bin/src/main.rs` beside
`recover_pending_verification`. It rebuilds projections from the journal, then
walks the outbox; every unfinished intent ends **dispatched**, **terminally
settled**, or **left alone because a live turn in this process owns it**, tallied
in `VibedevDispatchRecovery` (`dispatched` / `recreated` / `already_running` /
`settled_terminally` / `exhausted` / `settled_scheduled` / `held_by_live_claim` /
`skipped` / `unreadable_keys` / `unreadable_scopes`). A second pass is a no-op.
`recreated` is a subset of `dispatched`. There is deliberately **no `total()`** —
only `touched_anything()` — because the unreadable counters are diagnostics and
can double-count.

Rules:

* **Nothing is decided before the claim.** `claim_for_recovery` holds the
  `HeldByThisProcess` refusal and the generation fence; every branch (dispatch,
  settle, rollback) reads the claimed record. The retry bound is checked on the
  claimed record (one past the bound, since the claim charges this pass) — the
  branch that can physically delete a task must not run unprotected.
* **A scheduled run is settled, never started or deleted** (`settled_scheduled`).
  The task keeps its cron; dispatching would run an unattended Autopilot build
  early. Both this and the exhausted branch check the plan's schedule *and* the
  created task's `manifest.schedule` (older records have only the latter).
* **Exhaustion unwinds only what it left behind** — pointer first, then orphan —
  but never a run that **already has an execution** (`already_running` can charge
  an attempt and leave the record claimed) and never a **scheduled** one.
* **One damaged key does not hide a scope.** An unreplayable key is logged, named
  in `DispatchProjectionRepair::unreadable` and skipped; only a failure to
  enumerate the scope is scope-wide.
* **Pre-plan records** (task id only) are settled terminally. Reading them relies
  on `task_plan` and `recovery_attempts` having `skip_serializing_if`: the
  journal's integrity check re-serializes and compares hashes.

### What this reuses from the verification store, and what it does differently

Same shape as `execution/verification/{store,journal}.rs`: an append-only journal
committed by atomic rename is the truth; `intents/` and `live/` are projections
repaired by replay; `live/` is the outbox (recovery is O(unfinished)); a
monotonic generation, re-checked under the lock against the stored record,
fences stalled writers.

Differences:

1. **No TTL lease.** A dispatch claim spans one create plus one
   `start_execution` and never outlives its process, so another process's claim
   is abandoned by definition. **Enforced, not assumed:** the reconciler runs
   detached while the server serves turns, and reclaiming a live turn's claim
   would dispatch twice and let the loser's rollback
   (`archive_task_with_options`, `remove_files = true`) **physically delete the
   winner's task**. So each process mints an instance id
   (`dispatch_process_instance_id`), every claim records `<holder>@<instance>`
   (`dispatch_claim_holder`, applied inside the store), and
   `claim_for_recovery` refuses — writing nothing, charging nothing — a claim
   from the current instance, checked under the fence lock. Claims from other
   instances (or with no `@`) are reclaimable immediately. Consequently
   `recovery_attempts` advances at most once per process.
   **Not closed** (would need a renewed lease): two processes over one data
   directory; a claim this process abandoned without dying (stays refused until
   restart — safe direction, logged at `debug!`, counted `held_by_live_claim`).
2. **No separate outbox object.** The intent *is* its work item; "retire" means
   the record went terminal.
3. **One journal payload.** Every transaction asserts the record's next state.

The verification module is not imported (it gates code; this admits work). A
shared generic journal would be a separate change.

### `failed` is terminal, and that is the point

A retry of a key whose attempt failed gets **the same answer**, not a second
attempt. A new attempt is a new turn, hence a new key.

## 12. Follow-ups continue the conversation's own last run

A second `@vibedev` in a conversation continues the first: its own task, key and
row, chained to the parent so `run_coding_task` derives the same coding session
from the chain root.

### What makes a turn a follow-up

**The conversation's own most recently admitted run**, and nothing else.

| Candidate | Why not |
| --- | --- |
| An explicit `#follow` flag | Grammar nobody discovers, defaulting to the side of the trade that silently loses continuity. Every ordinary follow-up would start cold |
| The project's `active_root_task_id` | That is the **cockpit's** pointer, which the rail deliberately never touches (§7). Continuing off it would let a chat aside thread onto whatever the cockpit is doing, in a run the conversation never started |

The conversation is already the rail's identity anchor (idempotency key,
`chat_session_id`, cleanup). Cost: an unrelated build in the same conversation
inside the window threads onto the previous one (same project, recoverable).
**Most recent** means one candidate: a terminally *failed* admission is not a
run; if the candidate does not validate, the turn starts a **root** run rather
than reaching further back.

### The bound

**Twelve hours from admission** — comfortably longer than a build (watcher
deadline 3h), short enough that next morning starts fresh. Past it, a root run:
a missing continuation, never a wrong parent.

### Where the parent comes from, and why text cannot reach it

From the scope's **dispatch-intent records** (§11), filtered by the turn's
**server-minted** chat session id. `resolve_vibedev_run_parent` has no user text
in its inputs (service, scope, session id, resolved project id, this turn's key).
The project comparison uses a **typed field written at admission**
(`DispatchTaskPlan.project_id`), not the `VibeDev project:` line — a string
protocol is tolerable for a repo path, not for a link deciding whose work a build
continues.

This turn's own key is excluded, or a retry would nominate its own run as parent,
digest differently, and turn a replay into a conflict.

### Validation, and what a failed one does

The candidate must **exist**, be in the **same scope**, be admitted for the
**same project**, be a **VibeDev run** (`is_vibedev_cockpit_run`, rail or
cockpit), and be inside the window. Scope holds structurally: the intent store is
built on one scope's directory and re-checks every record; the task read is
scoped. **Every miss starts a fresh run — none is an error**: a missing
continuation costs a warm session; a wrong one runs against another chain.

### What the follow-up carries, and where the parent line sits

Mirrors the cockpit (`submit.ts`):

| Cockpit | Rail |
| --- | --- |
| `VIBEDEV_FOLLOW_UP_TAG` = `vibedev-follow-up` | same tag, same colour |
| `VIBEDEV_THREADED_TAG` = `vibedev-threaded` | **always** on a rail follow-up |
| `promptTitle` → `VibeDev follow-up · …` | same prefix, so `stripRunTitlePrefix` still recognizes it |
| `continuationReferenceTaskIds` — the parent only when `completed` and not synthesizing | same rule, landing on `depends_on` (which is where the create handler puts validated `reference_task_ids`) |
| `Parent task: <id>` in the description | same line, from the **same constant** the reader uses |

`vibedev-threaded` is unconditional: the rail's one follow-up gesture is the
conversational one (the composer's Send, folding into the chain root); the
cockpit's Run button has no chat spelling.

**The `Parent task:` line sits ABOVE the fenced user prompt** (the cockpit puts
it below). The reader already excises the fence (§4), so this ordering is
belt-and-braces. The reader is `pub(crate)` and the test asserts against it. A
root run has no continuation block.

### One block, not the cockpit's four framings

The cockpit composes four framings (plan/code × discuss/build) because it knows
the parent's kind. The rail *selects* a parent, so it uses one store-backed block
(`vibedev_rail_follow_up_context`) that reads correctly either way, above the
parent's status and title; the header (`VibeDev plan continuation:` vs
`VibeDev continuation context:`) comes from the parent's tags.

## 13. A run that is waiting on you, and what voice says about checks

### 13.1 The pending approval is derived, never remembered

A build stages a `CodeChangeProposal` and stops.
`TaskListItemV3.awaiting_diff_approval` is computed on **every read** from
`<scope>/code_change_proposals/`, never from an event — a `hitl.requested` event is
one-shot and lost on crash, a directory is not. That is what "survives restart"
means (same reason `list_attention_items` re-derives per poll).

* **A terminal task never claims to be waiting**, even with a `Pending` proposal
  on disk (orphaned; the attention feed drops it too).
* **Wire unchanged otherwise:** the field is omitted while false; the feed card
  gains `awaiting_diff_approval` metadata and `NeedsAction` status only when
  true.
* Set in **both** list-item builders — `@vibedev` runs are `Internal` and reach
  only the lightweight projection.

### 13.1.1 Derived per read, but read once per request

The proposal store is a flat directory with **no by-task index**, and records are
mostly diff text. Deriving per task would cost M walks of F files per listing, on
a 15s feed poll. So two per-request snapshots are taken at the top of a request
and threaded down:

| Snapshot | Store | Taken by |
|---|---|---|
| `PendingDiffApprovals` | `<scope>/code_change_proposals/` | `ArtifactV2Service::pending_diff_approvals` |
| `PendingFileEditTransactions` | `<scope>/transactions/` | `ArtifactV2Service::pending_file_edit_transactions` |

`build_task_list_item`, `list_tasks_with_pending`,
`list_internal_tasks_with_pending`, `task_progress_projection_with_pending`,
`announce_pending_diff_approvals_to_voice_with` and `list_attention_items`'s HITL
gate take a handed snapshot. The public no-argument forms (`list_tasks`,
`list_internal_tasks`, `get_task_progress_projection`,
`announce_pending_diff_approvals_to_voice`, `get_task_list_item`) and the indexed
page loads (`list_task_page_items`, `list_internal_task_page_items`, one per
page) take their own. Invariant: one walk per request.

A snapshot, not a cache or index: born and dropped within a request, nothing to
invalidate. The staleness window it adds is cosmetic for cards but not for the
**voice announcement**, which writes a durable dedupe marker — so that path
re-checks the single proposal (`is_unclaimed_pending`, peek-only) as the last
statement before speaking, with no `await` between.

The one-walk property is enforced by a process-global test counter in
`CodeChangeProposalStore::scan_records` (incremented before `read_dir`); asserting
tests hold a mutex, since a thread-local cannot see the `spawn_blocking` thread.

The store decides ownership and status from a `task_id` + `status` **peek**
before deserializing, so another run's proposal never allocates its diff. The
peek's required fields are a strict subset of the full record's, so the
pre-filter can only over-admit and the full parse re-applies the filter (a byte
search for the task id would match diff text).

### 13.1.2 The proposal store has no pruner

Nothing deletes a `ccp-*.json` (`reject`/`apply` rewrite in place), so the store
grows for the scope's life. A future pruner must:

1. Prune **terminal status only** (`Applied`/`Rejected`) with `resolved_at` older
   than a retention window — never `Pending`.
2. Not touch records of a **live task** (card and attention gate re-derive from
   them).
3. Hold the per-record decision lock (`acquire_decision_lock`).
4. Respect the trusted-store authority, which keys on proposal id.
5. Keep retention longer than the revert window: `apply_root`/`snapshot_id` are
   the only revert handle.

### 13.2 Announcing it once, and what once costs

`announce_pending_diff_approvals_to_voice` pushes `task.awaiting_diff_approval`
to the live call — by the run's `chat_session_id`, else **by scope**. It runs
from `list_attention_items` (sharing its snapshot) because an announcement fired
at staging time is exactly the one-shot event restart would lose.

**Cockpit runs have no chat session** (kept out of the task sweep), so the
fallback is a `(principal, workspace) → voice_session_id` index on
`VoiceDownstreamFanout`, registered when a call binds. Scope is the widest route
the caller can honestly claim; a test asserts it does not reach another scope.
Binding `chat_session_id` on cockpit manifests instead would re-expose them to
the sweep. The caller is not assumed to be watching the cockpit — its diff rows
show only the active chain, and hands-free callers are not looking at all.

Suppression is durable: one empty marker per proposal id under
`<scope>/spoken_hitl_announcements/` (`artifact_v2::spoken_hitl_log`). **A
proposal is announced once, ever** — not per call or per boot; the card carries
the state afterward.

| Rule | Why |
| --- | --- |
| No live call ⇒ no announcement **and no marker** | A diff staged while nobody is listening must still be announced on the first poll that finds a call. Recording silence as "said" would lose it permanently |
| The marker is written **after** delivery | A lost marker costs one repeated sentence; a marker written first costs the announcement outright |
| A terminal task's orphaned proposal is skipped | Same rule as the card, from the same helper |

`VoiceControlSession` renders and injects the sentence itself, so the
backend-proxied / cascaded road speaks it. `realtimeVoiceClient.ts` has no case
for this kind, so on the data-channel road the announcement is silent while the
card still shows it.

The nudge (`voice_diff_approval_waiting`) names the task's **title**, never its
id, and produces nothing when there is no usable title.

### 13.3 `unknown` must say nothing about checks

`verification_state_for_task` is the durable projection of whether a run's code
was checked: **missing state reads as `Unknown`, never `Verified`**. `Unknown` is
the common case — the verification controller is inert unless
`MAGICIAN_VERIFICATION_CONTROLLER=enforce`.

The completion announcement (`task_completed_message`'s `verification_state`)
appends one store-backed clause:

| State | What voice says |
| --- | --- |
| `verified` | *"Its code checks passed."* |
| `unverified` | *"Nothing checked that code, so I can't tell you whether it works."* |
| `exhausted` | *"Its code checks kept failing and the repair attempts ran out, so that code is not working yet."* |
| `unknown` | **nothing at all** |

Templates: `voice_verification_verified`, `voice_verification_unverified`,
`voice_verification_exhausted`. There is deliberately **no
`voice_verification_unknown`**; `verification_voice_clause` returns `None`.
Speaking `Unknown` as verified or unverified would each be a lie; silence is
correct in both cases, and the test asserts the absence of words like "check",
"verif", "passed", "repair", "proven", "tested". Unsettled states (`repairing`,
`verifying`) and controller states (`unavailable`, `cancelled`,
`blocked_partial`) are silent too — an announcement fires once. "Passed" appears
only in the verified clause, so `exhausted` cannot be mistaken for success.

**No compiled fallback** for these clauses (unlike §5): a drifted copy claiming
checks passed is the exact failure to prevent. A missing template degrades to no
clause — the same as `unknown`.
