# Magician plane

The governed MCP door a foreign harness uses to reach Magician. Plan:
`docs/archive/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.

Every tool call a harness makes crosses the plane, so governance does not depend
on what the harness CLI reports about itself.

## Grants and surface

- `InvocationSurface::Plane` is an owner audience, not a default surface.
- `plt_` grants: `PlaneGrantRegistry` owns short-lived process-local run grants.
  Revocation is shared with every pre-resolved clone and is checked after
  dispatch queueing and again immediately before `execute_action`, so queued
  clones and the cache cannot bypass it. Durable terminal grants are revalidated
  against `AuthStore` on every MCP request; an authority change replaces and
  revokes the old projection, carrying forward only process-local loaded-tool
  state.
- Grant dispatch takes one lock across authorization and action execution.
  The turn engine holds a `RevokeGrantOnDrop` lease; chat and agentic mints hold
  a drop guard, so an aborted owner still revokes its grant (an approval pause
  hands that duty to the dispatch-aware pause cleanup).
- **Attenuation integrity**: the durable attenuation sidecar is HMAC-sealed
  (`PlaneAttenuationIntegritySeal`, domain `magician.plane-attenuation.v1`,
  `durable_attenuation.rs`), keyed from the restricted store. Persist order is
  marker → payload → seal, so every crash window fails closed (marker-last would
  silently widen a run). Tampered, widened or cross-execution-replayed sidecars
  fail closed at dispatch.

## Catalog and `tool_search`

Two hot lists. **Terminal** is Magician's control surface. **Spawned-bare** also
advertises governed file/HTTP leaves because `--tools ""` leaves the harness
with no native tools. Raw shell is deliberately absent: arbitrary command
execution could nest another credential-bearing harness and bypass
`NEVER_ON_THE_PLANE`. Control verbs are never advertised. A grant with no live
executors advertises an empty catalog. `tools/call` enforces the same hot +
grant-loaded projection `tools/list` returns — tiering is authorization, not
only discoverability.

`tool_search` is grant-scoped (durable terminal grants included). The door
resolves the engraved workspace's embedded + installed-tool index through the
runtime's shared scope resolver, so hot tools carry real descriptions and
schemas and deferred leaves are searchable; another workspace's catalog is never
used.

- `select:` **replaces** the grant's loaded set with the permitted leaves of the
  whole pack (merges for a conversation grant whose Deferred hands were
  preloaded); configured hot controls stay. It may name a leaf
  (`select:browser__open`) or the pack (`select:browser`).
- A run grant treats a pack's leaves as dispatchable when the run's registry
  serves the pack (skill packs register one provider under the pack name).
- Keyword search does not load, filters denied/control/allowlist-excluded tools
  before the limit, and returns at most 12 matches. `_meta.toolsListChanged`
  fires on a real change.
- The loaded set is per grant, survives durable revalidation, and resets on
  restart. A leaf removed from the refreshed index stops being advertised or
  callable. The scope catalog cache is revision-aware.
- The plane owns the schemas for `tool_search` (required `query`) and
  `session_ledger` (no arguments). A valid durable grant gets
  `503 tool_catalog_unavailable` when the resolver is missing, fails or returns
  an empty index, rather than an inert search surface.

## MCP door

`POST/GET/DELETE /api/magician/v2/plane/mcp`. The route authenticates its own
process-live or durable `plt_` bearer. Loopback is the intended deployment, same
shape as the Citizen API: the opaque bearer is the authority. A missing, revoked,
expired or unknown grant is `401`.

- `initialize` negotiates protocol `2025-11-25`, advertises `tools.listChanged`,
  and server-mints a `pltsess_` session id (never reflecting the bearer or a
  client lookalike). Each initialize creates a fresh session.
- `tools/call` requires a server-issued session id and a string/number JSON-RPC
  id; client-invented sessions and notification-shaped calls are refused.
- Replay window: process-local, per session, 256 completed ids per grant.
  Identical requests replay the completed response; reuse with different
  content fails closed. This is retransmission protection, not exactly-once.
- GET notifications: one stream per session; a new GET replaces the old
  receiver. Prompts and results use their originating POST stream;
  disconnection does not revoke the grant.
- `DELETE` with a server-issued `Mcp-Session-Id` ends the session (204,
  idempotent): the id stops being accepted, pending interactive calls are
  cancelled, the notification stream drops. The grant is never ended by a
  session close.
- **Session identity** is per MCP connection, never per grant: two terminals on
  one grant keep separate ledgers and replay namespaces and serialize through
  the grant's single dispatch lock.
- **Terminal parent engine**: decided once at `initialize`
  (`terminal_parent_engine`, logged at info — the only visibility). The grant's
  operator-engraved `harness_engine` wins; else the connecting CLI family from
  `clientInfo.name` (`engine_family_from_client_name`, whole-token match); in
  either case only when that CLI is installed here. Every `tools/call` of the
  session, and every capture its elicitation approves, runs under that parent
  (`grant_parent_engine`). Run and conversation grants keep the engine Magician
  chose for them.

## `tools/call`

- `tool_search` is grant-local. `request_user_input`, `wait_for_run`,
  `run_task`, `delegate_to_agent` and `session_ledger` use plane-owned handlers.
  Ordinary tools lower through `lower_native_tool_call` and run `execute_action`
  when the grant carries live `ActionExecutors`; without them the result carries
  `_meta.planeDispatch: unwired`.
- Pack confirmation is evaluated on the plane (the loop's `checked_approval` is
  keyed to a `Decision` a harness never produces). A live turn that needs
  approval sets `PlaneTurnStopReason::NeedsApproval` without cancelling the run
  token. A spent turn tool-call bound sets `TurnBudgetSpent`: the turn ends, no
  human is asked, the grant is released. Only a governed dispatch spends the
  bound; refused, unwired and `tool_search` calls do not.
- **Per-action events**: every governed call emits one `AgenticActionExecuted`
  (iteration 0; observability-gated). The plane never emits
  `AgenticIteration*`/`AgenticExecution*` (pinned by a structural test).
  Terminals watch delegated runs through these plus the `task_state`,
  `get_active_executions`, `get_execution_history`, `get_task_details` hot tools.
- **Terminal ledger** (`terminal_ledger.rs`): every governed call from an MCP
  session records tool, timestamp and outcome — executed (with `isError`),
  refused (`not_available`, `needs_approval`, `unwired`, `revoked`,
  `turn_paused`, `turn_budget_spent`, `input_not_resumed`) or
  elicitation-pending — bounded at 512 entries per session. Content, arguments
  and credentials are never stored. `session_ledger` returns the caller's own
  trail. Harness-turn calls do not ledger here. No synthetic episode for runless
  sessions: a caller that wants its work remembered starts a task.

## Harness engines

Engine selection flows through `harness_engine_for` (`plane/engines/mod.rs`),
whose arms are the roster `resolve_turn_engine` names. The decide seam builds no
specific engine, so a new harness costs one engine file, one factory arm, one
roster name and one dropdown entry. A roster/factory drift test keeps them in
step. `GET /api/magician/v2/plane/engines` returns the roster with install
status (a PATH lookup, no spawns), `current`, `chat_current` and each engine's
`native_tool_posture`; Settings offers only installed engines.

Common spawn rules: the grant is never on argv (a create-new mode-`0600` config
file or an env var); the child environment is cleared and rebuilt from a minimal
allowlist; protocol events are capped at 1 MiB. A turn-stop signal turns an
approval gate into `NeedsApproval` (kills the paused child, retains the grant)
and a spent bound into `TurnBudgetSpent` (releases it). Start failure, Drop and
shutdown revoke the grant; Drop/shutdown kill the process group. The wall-clock
ceiling is an arm of the event select, so a silent harness still hits it.

| Engine | Shape | Native-tool posture | Streaming / resume |
| --- | --- | --- | --- |
| `pi` | Pinned Pi 0.87.1 RPC driver (shared with VibeDev), `agent_settled` boundary | Built-in tools, extensions, skills, templates, context files and themes disabled; only the bundled `magician-plane.js` extension | Native session in private Plane home |
| `claude_code` | Warm stream-json session, `--include-partial-messages` | `--tools ""` + `--strict-mcp-config` (stripped) | Streams `text_delta`; `--resume` |
| `codex` | One-shot `codex exec --json`, isolated `CODEX_HOME` with `bearer_token_env_var` | `-c sandbox_mode="read-only"` (works on `exec resume` too), `--disable shell_tool`/`unified_exec` (sandboxed) | No deltas (`exec --json` emits completed items only) — settles as one chunk; `codex exec resume <id>` |
| `codex_app_server` | Shared `coding_engine` driver (`CodexAppServerAdapter::run_turn_spawned`), `CodexLaunchProfile::PlaneHarness`, one child per turn | Read-only sandbox + same isolated `config.toml` (sandboxed) | Per-delta text tap (`CodingEngineRequest::text_delta_sink`); `thread/resume` in a persistent `CODEX_HOME` |
| `grok` | One-shot `grok -p --output-format streaming-messages-json --include-partial-messages`, isolated `GROK_HOME` seeded with operator `auth.json` | Empty `--tools` + built-in denylist (stripped; MCP meta-tools remain). No `--sandbox`: it fail-closes when `docker.sock` is a symlink | Streams `text_delta` (never `thinking_delta`); `grok --resume <id>` |
| `agy` | One-shot `--output-format stream-json`; MCP registered in the **global** `~/.antigravity` via `agy mcp add`/`remove` | `--sandbox` + `--disable-slash-commands` (no strip flag) | Streams `step_update` deltas; `agy --conversation <id>` |

Engine notes:

- **Codex** reaches MCP tools only through its code-mode host, so the plane
  leaves `code_mode`/`code_mode_host` on (sandbox read-only, shell and
  unified_exec off) and pre-approves the plane server's tools
  (`default_tools_approval_mode`) because the door is the approval authority.
  Both Codex engines render one shared `config.toml`. The code-mode host runs
  model-authored code under Codex's own confinement, which Magician does not
  attest. The fenced VibeDev path refuses the `PlaneHarness` profile.
  `codex_app_server` resolves its bare binary name with
  `runtime_core::process::resolve_program` because the spawn rebuilds `PATH`.
  Its driver's event queue folds message deltas (right for the cockpit, wrong
  for chat), hence the separate text tap.
- **agy** hazard: concurrent sessions share one global registration; the add
  removes a leftover entry first and every exit path best-effort removes it, but
  a second concurrent session re-points the registration at its own grant.
- **One-shot engines** (`plane/engines/oneshot.rs`) spawn one process per turn
  with the prompt on argv. Each stdout line is parsed once for the CLI's native
  id (codex `thread.started`, grok init `session_id`, agy `init`) and for a text
  delta. A turn that streamed settles with the streamed text alone (the final
  line repeats it) except on a failing exit, where the final line is the error
  and is kept. Process exit is the terminal event (success → Settled, failure →
  Refused). Text is extracted tolerantly (`result`/`text`/`message`/`output`/
  `response`, 16 KiB cap); these event schemas are probed, not documented, so
  summaries are best-effort while governance is unaffected. Stderr keeps its
  last 16 KiB after grant redaction (a straddling grant is replaced whole) and
  is logged at warn on a reply-less, failing or timed-out turn.
- **Resume**: a session in a persistent `native_home` reports its native id; a
  session on its own temp home (loop seam) neither passes nor reports one, since
  the CLI's session dies with the home and resume has no cold fallback. A
  refused turn reports only an id learned that turn (for `codex_app_server`,
  one the driver confirmed), so a failed resume goes cold next turn instead of
  refusing forever. Settled or refused turns keep the session installed; config
  installs once per session.

### Pi harness

- Chat keeps Pi's native session in its private Plane home; agentic turns use
  temporary process homes and a private Plane session directory under the
  runtime root as a stable cwd (Pi scopes session lookup to cwd), so the next
  iteration resumes the same native id. Pi requires private permissions on its
  home, agent and session directories; an owned temp home is removed if setup
  fails after credentials were copied.
- Warm chat resumes send the current transcript user turn (with memory and
  attachment descriptions). Current-turn images go through RPC `images[]`,
  bounded at 50 MiB aggregate; older images are transcript text on a cold replay.
- `magician-plane.js` registers the grant's hot MCP tools plus a dispatcher for
  tools later loaded by `tool_search`, and must finish MCP initialization before
  Pi gets a prompt. The grant lives in a private file in the Plane home. Pi is
  not in the terminal-grant picker: its CLI has no built-in MCP client.
- **Credentials and model**: the version probe uses the same isolated env. With
  a selected Magician profile only that profile's key is forwarded, and profiles
  with a private model entry omit the operator's `auth.json`/`models.json`.
  Without one, Pi uses the operator's `auth.json`/`models.json` (copied into the
  isolated agent dir) or provider key env vars, and only its default provider,
  model and thinking level are copied from settings. `default` leaves model
  choice to Pi; an explicit model is passed as `--model`.
- **Profile mapping**: adaptive profiles resolve to their fast variant. A
  profile with `api_key_env` or `api_base_url` gets an isolated entry in Pi's
  private `models.json` with the key referenced by env var; a missing key fails
  the turn. Mapped providers: OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek,
  MiniMax, xAI, Ollama (Ollama's local endpoint is always installed; local
  endpoints may omit a key). An Anthropic base URL is the API root with any
  trailing `/v1` removed, because Pi appends `/v1/messages`. Models Magician
  calls with adaptive thinking (Opus 5.x, Sonnet 5, Opus 4.7, Fable) are marked
  `compat.forceAdaptiveThinking`, since they reject Pi's default budgeted
  thinking. Image/reasoning flags use the chat picker's inference, and output
  budget is passed; other model metadata is not. Changing the profile or its
  model metadata starts a new native session and replays chat history.
- **Agentic profile**: `execution.pi_profile` (Settings → Engines) is resolved
  from the live router before each Pi turn; empty keeps Pi's own credentials.
  Only Pi-driven runs use it. The Settings write refuses a missing or ineligible
  name; a router change underneath fails a later turn.
- **Usage**: settled turns report `input_tokens` (all input read),
  `cached_input_tokens`, `output_tokens`, `total_tokens` in the
  `agentic harness turn settled` log and `harness_turn_settled` event. Pi
  reports uncached, cache-read and cache-write separately, so input is their
  sum. An operator-cancelled turn with no usage report does not exhaust the
  run's token budget.
- Background text operations can follow a Pi engine through `op-harness-pi`
  (operator's installed Pi CLI and credentials). Local, tool-carrying and
  pinned operations keep their profiles; no shipped mapping selects Pi.

## Loop seam

`execution.harness_engine` selects what a run thinks with; the seam is inside
`phases::decide::run`. A harness turn mints a live `plt_` grant with the run's
executors and the tool-call bound. `NeedsApproval` becomes a
`Decision::NeedUserInput` that apply converts, when the plane captured the gated
action, into `AgenticOutcome::WaitingForConfirmation`, so the human is asked
through the ordinary HITL path.

- Bounds: `harness_turn_max_seconds` (default 2400, same as
  `DEFAULT_AGENTIC_MAX_DURATION_SECS`) and `harness_turn_max_tool_calls`
  (default 4000, same as `AgentConstraints.max_iterations`, refused at
  `tools/call`). Both carry across an approval pause, so a resumed turn gets
  only the remainder; a bound spent by the pause ends the turn without spawning.
- Per-execution continuation warm-resumes the native session. A paused turn's
  retained grant is revoked once any in-flight dispatch finishes, even if never
  resumed; the replaced continuation revokes it on resume. A parent Stop beats
  an approval or delegation stop latched during teardown. A terminal outcome
  drops the native id and pending result note; paused outcomes keep them.
- Protected app runs never think through a harness: the seam fails closed to
  Magician's path when an app disclosure guard is present.
- **Parent engine**: `run_engine_for` (launch pin, else process snapshot) is
  also the parent of the run's background LLM operations (`run_parent_engine`;
  `magician` ⇒ none). It is named on the run's scoped router and decide adapter
  (`routing_overrides_for_run`, never from the launch request), scoped around
  every phase and carried by `CapturedRunTaskLocals` across lanes and
  delegation hops. See [llm-routing-overrides.md](llm-routing-overrides.md).

### Execute-on-resume

When the plane ends a turn for approval it captures the gated action in the
loop's stable serialization; the durable pause carries it (plus native session
id and budget remainder) as a `harness_pause` continuation through
`FullPauseData`. On a confirmed resume Magician **executes the approved action
itself** through `execute_action` (fresh trust guard, deterministic effect id
bound to the pause and action). No replay entry is pushed; the next harness turn
opens with the **result**, not the verdict. Execution happens only after the
resume passes the continuation's routing and transition-authority admission.

- Any harness pause restores the warm continuation; the loop's own confirmation
  gates keep their one-shot replay waiver; the `inprocess` rollback arm keeps
  confirmation replay. Publication reads the continuation non-destructively.
- **Boundary**: resume-time execution is outside the loop's durable Apply intent
  machinery. A process death between execution and the first resumed LoopState
  commit is not covered by effect-ledger recovery; the effect id correlates
  retries but is not exactly-once.
- **Not built**: the resume-executed action's result reaches the harness and the
  continuation but is not appended to the run's iteration record.

## Delegated and runless runs

**Delegated runs.** `PlaneRunAuthority` on the grant carries `allowed_agents`,
`harness_engine` and ceilings. `run_task` routes to `run_ownership` before any
lowering: no authority → refused naming "run authority"; an engine this build
cannot launch → "cannot launch"; explicit `magician` is a deliberate pin. A
launch does not spend the turn's tool-call bound.
`start_plane_delegated_execution` carries `PlaneDelegationAttenuation`:

- Denied = `NEVER_ON_THE_PLANE` **plus `run_task` itself** — the loop-side
  handler launches unattenuated, so a delegated run that could call it would
  hand a sibling the whole surface. Sub-work uses `spawn_sub_goal`/
  `DelegateToAgent`, which inherit the denial.
- The attenuation persists (`plane_attenuation.json` + `plane_attenuated.json`
  marker under the task write guard) **before** the run can dispatch; the
  orchestrator loads it at both dispatch-composition sites; a marker with a
  missing or unreadable payload fails closed.
- `plane_denied_capability_names` merges onto the profile's deny list (children
  inherit it); a non-empty `plane_allowed_capability_names` intersects the
  catalog at `build_catalog_context` (umbrella name or `<pack>__<action>` leaf);
  `ctx.harness_engine` overrides the snapshot. All three ride
  `AgenticPauseState` and are restored on resume.
- Ceilings: `max_usd` and `max_wall_clock` lower the run's cost cap and work
  budget (min with the global cap — a grant narrows, never widens) and persist
  with the attenuation. `max_concurrent_runs` (default 4; mint sanitizes zero to
  unset) is a launch-time refusal backed by a process-local grant-keyed ledger
  pruned against the execution store; a restart resets it.

**Runless (terminal) dispatch.** Boot installs process-global executors with
the shared scope catalog resolver. Each durable terminal grant dispatches
through its own copy (`for_runless_scope`) with the scope's registry, the
allowlist, and the grant's identity seeded, so scope-bound compiled tools run
without a `__principal`/`__agent_id` failure. Narrowing keeps each allowlisted
leaf's parent pack and declared provider; the door's exact-leaf gate bounds the
callable surface. Features that need a live execution (sandbox widening,
browser session) refuse naming `run_task`.

**Grant management.** `GET/POST /api/magician/v2/plane/grants`,
`DELETE /api/magician/v2/plane/grants/{id}` accept only a session bearer (a
`plt_` bearer cannot mint siblings). Mint derives the workspace from that
credential, validates ownership, floors the allowlist, bounds TTL, returns the
token once and reports floor-dropped tools. Durable rows carry `max_usd`,
`max_wall_clock_secs`, `max_concurrent_runs`; the door attaches run authority to
every durable projection (the engraved harness is what a started run inherits).
Revocation takes effect on the next call. UI: `TerminalGrantsPanel` in
`ui/unified-ui`.

## Run engine pin

Every run fixes what it thinks with at launch: a `RunEnginePin`
(`plane/engine_pin.rs`: `engine`, `harness_model`, `pi_profile`, the three
values Settings writes together). A Settings switch or reload moves only runs
launched afterwards; running, paused and recovered runs keep their pin. Rule:
**inherit the parent unless something names an engine**.

- **Resolution** (`resolve_launch_pin`): an engine named for the run wins (a
  plane grant's harness); a named engine other than the parent's or Settings'
  runs its CLI's `default` model. Otherwise inherit `LAUNCHING_RUN_ENGINE_PIN`;
  a run started by nothing (API, scheduler) takes its task's creation pin, else
  the Settings choice at that instant.
- **Who sets the launching pin**: a chat turn (`chat_turn_run_pin`: composer
  engine/model, a voice call's `session.start` choice, else
  `chat.harness_engine`/`chat.harness_model`; for Pi the composer profile, else
  `execution.pi_profile`; an uninstalled CLI pins `magician`); the chat mouth's
  conversation grant carries it into every tool call, approved capture and
  bridged job; a run (`CapturedRunTaskLocals::for_context` across lanes and
  hops); the goal trigger onto its runtime job.
- **Durable records**: `engine_pin.json` in the execution directory (written at
  start, accepted launches and recovery, goal trigger, and delegated children
  from the parent; an existing pin stands unless a plane grant names another
  engine; writes take the task write guard). `launch_engine_pin.json` in the
  task directory, written by `create_task` when a pin is in scope and read by
  the first launch. Taskless roots save under
  `runtime/taskless_engine_pins/` (hash of execution id). Unreadable pin files
  read as absent with a warning: the pin is a preference, not authority, and
  must never wedge resume or recovery.
- **Composition**: the orchestrator loads the pin at its one compose point by
  execution id; a run with none is pinned there (from the Settings choice at
  that moment, harness model per `RunEnginePin::for_engine`).
  `AgenticContext::with_run_engine_pin` sets `run_engine_pin` and
  `harness_engine`, so `run_engine_for` never falls back to the snapshot.
- **Pauses**: `AgenticPauseState.run_engine_pin` carries the pin; when present
  it and `harness_engine` join the pause's authorization hash (older pauses keep
  their original hash).
- **Not pinned**: turn limits and the plane endpoint stay live. Background LLM
  operations follow the parent's engine family (`op-harness-<engine>`), not its
  harness model.
- **Coding tasks** without an explicit `coding_profile` map the pin
  (`inherited_coding_choice`): `pi` keeps Pi on the coding profile whose
  `llm_profile` is the pin's Pi profile; other harness engines code with their
  counterpart when `adapter_spec_for_engine` can launch it, on the coding
  engine's default model. `magician`, a non-Ready engine or an unmapped Pi
  profile fall back to Pi on `coding.default_profile`. VibeDev inherits at
  admission ([vibedev-rail.md](vibedev-rail.md)).
- **Session fingerprint**: the continuation stores `session_fingerprint`
  (engine, model, profile) with the native id; a turn resumes only under the
  same fingerprint, else starts cold.

## Chat mouth

`chat.harness_engine` (roster, default `magician`) swaps the chat's mouth; it
is orthogonal to the loop key. Unknown names fail closed to the LLM path.
`PUT /api/magician/v2/plane/chat-engine` persists it (YAML + live snapshot).
Each send carries engine, model and profile (queued turns keep their route);
Settings and the composer share one browser-local choice, and the server key is
the fallback for clients that omit one. Protected (disclosure-guarded),
voice/meeting, App Copilot, Tutor and public-envoy turns never think through a
harness. A turn that falls back to Magician (guarded disclosure, CLI missing,
factory failure) abandons the warm continuation first.

**Grant.** A chat harness turn mints a **ChatScoped** `plt_` grant that keeps
the turn's invocation surface (run and terminal mints stamp Plane), inherits the
turn's tool allowlist (empty skips the harness rather than opening the catalog),
and uses per-grant scoped executors (`scoped_runless_executors`). Revoke cancels
a child of the chat-turn token so a settled turn does not abort the
conversation. Gated plane actions refuse: chat HITL is the next user message.

**Hands.** Direct and Deferred tools cross under their own names; the grant
carries the turn's scoped tool index so non-hot Deferred names load through
`tool_search` (which must itself be allowlisted). Engines that do not re-list
mid-turn (`tools_list_changed: false` — codex, codex_app_server, grok) get
Deferred hands loaded at mint. Every chat-runtime tool declares a
`PlanePosture`:

- **Counterpart** rides the grant (task details → `get_task_details`, describe
  agents → `get_agent_details`).
- **Bridged**: the native implementation, reached through a `ChatMouthBridge`
  built per turn (`ChatService::build_mouth_bridge`) over a captured
  `BridgeTurnContext` shared by `Arc`. The bridged set
  (`PlaneGrant::bridged_tools`, `plane_bridged_specs`) is the native mouth's
  advertised specs minus plane-executed names and non-Bridged postures, under
  the floor `chat_harness_bridgeable`: `NEVER_ON_THE_PLANE`, the door's verbs,
  `LOOP_INTERCEPTED_VERBS` (e.g. `request_thinking_mode`) and governed app tools
  never bridge; of the loop's control verbs only `read_result` does. Compiled
  names the plane never executes (`NOT_A_PLANE_EXECUTED_HAND`: e.g.
  `switch_personality`, `activate_skill`, `deactivate_skill`, `create_task`,
  `run_task`) are bridged from the tool index (`floor_rejected_compiled_specs`)
  — that is how a swapped mouth creates a task.
- **MouthOnly** is withheld from a swapped mouth.

The plane-execution floor (`NEVER_ON_THE_PLANE`, control verbs, mouth-tool
names, governed app tools) is one predicate, `chat_harness_floor_rejects`,
shared with the posture test.

**Bridged calls.** Advertised hot under the native name, description and schema;
added to an attenuated allowlist; routed to the bridge before lowering with no
plane approval capture — the native dispatcher applies its own approval, trust,
deny and allowlist rules in `dispatch_chat_tool_call`. Otherwise a bridged call
is one of the turn's calls: same tool-call bound, revocation re-check, `pltinv_`
id handed to the dispatcher as the call id, same events and ledger entry. It has
no calling profile (security-sensitive compiled handlers fail closed); a
governed app result is refused (`governed_app_result_not_admitted`). Results go
through `sanitize_json_for_provider` before any text, row or record is derived;
`isError` follows the native status vocabulary. The body runs on the execution
runtime (`runtime_boundary::spawn_execution_job`), never the HTTP worker. Its
subscriptions join the turn's once the harness settles.

- **Cancellation**: the job races the grant's dispatch token; on revoke the
  token is cancelled and the dispatcher gets `BRIDGED_CANCEL_GRACE` (2 s) to
  answer (dropping it would leak its fan-out and subscription). A late
  dispatcher unwinds detached; the door returns a revoked error. A dispatcher
  that ends without a result is `bridged_call_lost`.
- **Two tokens**: the per-call token dies when the grant is revoked, i.e. the
  moment the turn settles. `delegate_to_agent`, `handover_to_agent` and
  `orchestrate_pipeline` hang a cascade watchdog on their token, so they get the
  turn's own abort token (`bridged_call_cancel_token`: Stop, session teardown,
  superseding turn) — otherwise a delegated run would die when the chat turn
  returned.
- **Persona change**: a bridged `switch_personality`/`activate_skill`/
  `deactivate_skill` that actually changed state (decided from the recorded
  result, not the name) ends the warm resume (`abandon_chat_harness_resume`),
  because the native session would keep the old system prompt.

**Parent engine.** The chat mouth is the parent of the turn's background LLM
operations (`chat_turn_parent_engine`), scoped around the whole inline turn and
re-scoped onto its execution-runtime hops; a conversation grant names it on
`ctx.llm_routing_overrides.parent_engine`.

**Events and transcript.** Every governed call in a chat harness turn emits
`tool.call.started`/`tool.call.finished` on the chat turn (`via: plane`,
`harness_engine`, redacted arguments), so the activity card and conformance eval
see the swapped mouth's hands (conversation grants only). Arguments past 16 KiB
become an omission record; errors keep a 4 KiB head. The grant's `turn_ledger`
(written per dispatched call; refusals excluded; drained after revoke) is
persisted to the transcript as the native mouth would: an `AssistantTurn` of
calls (`call_id` = `pltinv_` ref), one `ToolResult` each (8 KiB head, ` …[cut]`
marker; errors as `{"status":"error",…}`; governed app content omitted), then
the reply. Bounds: `PLANE_TURN_LEDGER_MAX_BYTES` (512 KiB; past it calls are
recorded by id/name only) and batches of `HARNESS_TRANSCRIPT_CHUNK_CALLS` (200).
Failed and cancelled turns still record their calls. A failed batch append is
logged and the turn continues.

**Cold replay** of that history is bounded and redacted per item: argument
heads of 512 B after the JSON sanitizer; each result through
`sanitize_text_for_provider` then cut to 4 KiB; everything charged to one 48 KiB
budget, after which items become omission lines. Stale task-spawning calls
(`ChatLlmService::is_stale_replay_neutralized_tool`) before the last user
message are named without arguments or results. The current user text is
appended only if history does not already end on the user's side.

**Reply streaming.** Every engine except `codex` streams deltas through the
plane sink. Grok and agy settle with the streamed text bounded at 16 KiB. The
chat turn persists a streamed reply whole from the deltas it collected, and
sends the settled text as one chunk whenever nothing streamed this turn. A
non-streamed reply is read per JSON line, else from the whole stdout as one
document, bounded at the event cap. Paid-call accounting records harness tokens
with **cost $0 / no model id** (lossy `HarnessUsage`).

**Continuation and native home.** Continuation is per chat session and
**process-local** (restart ⇒ cold harness; Magician history is replayed). Warm
resume sends only the new user text. For engines that can resume, each
conversation gets `<runtime root>/.magician-storage/plane/homes/<uuid>` (mode
0700, remembered only in memory) passed as `HarnessSessionRequest::native_home`.
A session never removes a handed-in home; the continuation does, on
Magician-mouth takeover, persona change, session teardown
(`forget_chat_harness_conversation`) and engine switch. Startup preserves
existing homes (an overlapping older process may still use them); homes from a
crashed process wait for separate maintenance. A cancelled turn drops the native
id but keeps the home; a spawn failure keeps both. A native id is reused only
with the home it was minted in. Engines that cannot resume get a temp dir per
session. Under `cargo test` the root resolves to a scratch directory.

## Config

- `plane.spawned_bare.hot`, `plane.terminal.hot`: empty means the built-in lists
  in `execution::plane::catalog`; non-empty replaces them. A `spawned_bare.hot`
  overlay only changes which allowlisted names are hot; dropping `tool_search`
  disables deferred loading.
- `execution.harness_engine` (`magician` | `claude_code` | `codex` |
  `codex_app_server` | `grok` | `agy` | `pi`), `harness_turn_max_tool_calls`
  (4000), `harness_turn_max_seconds` (2400), `execution.pi_profile`. These are
  Magician-run-scale defaults. `POST /settings/magician-config/reload` updates
  the process snapshot.
- `chat.harness_engine`, `chat.harness_model` — chat mouth.

## Endpoint

The door's internal turn helpers (`RevokeGrantOnDrop`, the dispatch-lock
helper) are public but hidden from generated API docs because the separate
`magician-bin` execution contract test compiles the production turn engine
against the Magician library.

The harness conformance plane lane (`scripts/eval_harness_plane.py`,
`make test-plane-harness-live`) drives the door as an external harness would;
see [eval lanes](eval-lanes.md#harness-conformance-plane-lane).

## Typed input

Terminal MCP clients can gather free-form text, select options, complete forms
and approve governed actions through their originating session. The client
declares `capabilities.elicitation.form: {}` (or `elicitation: {}`) at
`initialize`; URL-only, absent or malformed declarations do not enable forms.
Prompts arrive as `elicitation/create` on the original tools/call POST SSE
stream; the client POSTs its answer with the same session and elicitation ID,
gets empty HTTP 202, and receives the final tools/call result on the original
stream. Approval requires `action:"accept"` and boolean `content.confirm:true`;
anything else never executes. Answers must match grant, session and request ID
and are consumed once. Terminal captures are per call, outside the live harness
approval slot.

Plane-owned tools for form-capable clients (subject to allowlist and hot
catalog):

- `request_user_input`: ask the originating user; returns a typed
  `UserInputValue` in `structuredContent.answer`, using Magician's `question`,
  `input_type`, `options`, `questions` vocabulary.
- `wait_for_run`: pass the `execution_id` this session's `run_task` returned.
  Waits for completion and relays pending questions, including child executions
  reached by verified parent edges. Refuses runs launched by another session
  (even on the same grant); one wait owns a run at a time.
- `delegate_to_agent`: live run turns only. Captures `targets` on the grant and
  ends the turn with a delegation stop; the settled turn becomes the loop's
  `DelegateToAgent` decision and the next turn resumes with the children's
  deliverables. Each target carries `target_agent_id`, `context`, optional
  `input_data` only — ownership and filing are Magician's to judge.

Form limits: 32 fields, 128 options, 64 KiB per text answer. Omitted fields mean
skipped; empty strings and selections are explicit answers. Unknown fields,
duplicate selections, unknown option IDs, missing required fields and mistyped
answers refuse without resuming. `allow_other` adds `other` plus `other_value`
(nonempty required).

Service-backed asks during an MCP call carry a task-local route bound to the
grant's principal/workspace; `UserRequestService` still owns acceptance,
publication, timeout and first-response-wins (a UI answer can win). Agentic
pauses go through the scoped resume owner and compare an opaque revision of the
captured question and action, so a reused pause key cannot apply an old answer
to a newer pause. A form answer grants nothing to later actions.

Timing: prompts time out after five minutes; invalid responses end the call with
an error. `notifications/cancelled` affects only the named call; cancelling a
wait leaves the run as is. `wait_for_run.timeout_secs` is 1–300 (default 300).
An answered question resumes the run on its own task; the wait holds up to 20
seconds so a refusal surfaces with the runtime's HTTP status, then resumes
polling. Authority is revalidated before submitting an answer or executing an
approved capture. A disconnected POST is not cancellation: its result is cached
and an identical retry returns it. Event-ID stream resumption is not
implemented.

Sessions, pending calls and run-origin bindings are process-local. Limits per
grant: 256 sessions, 32 simultaneous interactive calls, 256 run-origin slots;
capacity refusal happens before launch.

Not projected as MCP forms: passwords/API keys, diff reviews, multiple file
paths, service options with conditional `requires_input` fields — these need
Magician's authenticated UI (no URL-elicitation flow). Unsupported delegated-run
input leaves the pause pending; unsupported service-backed input in a direct
call is cancelled through its response owner. Live Magician-spawned harness
turns keep the EndTurn/loop HITL path.

Tests: `make test-plane-elicitation`
(`magician-bin/tests/plane_elicitation_contract.rs`). Wire references:
[MCP elicitation](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation)
and [Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).
