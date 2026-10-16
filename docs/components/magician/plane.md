# Magician plane

The governed MCP door a foreign harness uses to reach Magician. Every harness
tool call crosses it, so governance does not depend on what the harness CLI
reports. Plan:
`docs/archive/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.

## Grants and surface

- `InvocationSurface::Plane` is an owner audience, not a default surface.
- `plt_` grants: `PlaneGrantRegistry` owns short-lived process-local run grants.
  Revocation is shared with every pre-resolved clone and checked after dispatch
  queueing and again just before `execute_action`. Durable terminal grants are
  revalidated against `AuthStore` on every MCP request; an authority change
  replaces and revokes the old projection, carrying over only process-local
  loaded-tool state.
- One lock spans authorization and execution. The turn engine holds a
  `RevokeGrantOnDrop` lease; chat and agentic mints hold a drop guard, so an
  aborted owner still revokes (an approval pause hands this to pause cleanup).
- **Attenuation integrity**: the durable attenuation sidecar is HMAC-sealed
  (`PlaneAttenuationIntegritySeal`, domain `magician.plane-attenuation.v1`,
  `durable_attenuation.rs`), keyed from the restricted store and written
  marker → payload → seal so every crash window fails closed. Tampered, widened
  or cross-execution-replayed sidecars fail closed at dispatch.

## Catalog and `tool_search`

Two hot lists: **Terminal** (Magician's control surface) and **Spawned-bare**,
which adds governed file/HTTP leaves because `--tools ""` leaves the harness no
native tools. No raw shell: it could nest a credential-bearing harness past
`NEVER_ON_THE_PLANE`. Terminal and chat grants exclude loop lifecycle verbs; an
owned Work grant exposes `yield` and `need_user_input` through a private typed
capture slot. A grant with no live executors advertises an empty catalog.
`tools/call` enforces the same hot + grant-loaded projection as `tools/list`:
tiering is authorization, not only discoverability.

`tool_search` is grant-scoped (durable terminal grants included) over the
engraved workspace's embedded + installed-tool index, never another
workspace's. A hot name missing from that snapshot (`inspect_agent` when
harness services are unbound) still publishes the embedded pack's schema.

- `select:` **replaces** the loaded set with the pack's permitted leaves (merges
  for a conversation grant with preloaded Deferred hands); hot controls stay.
  Takes a leaf (`select:browser__open`) or pack (`select:browser`).
- A run grant treats a pack's leaves as dispatchable when the run's registry
  serves the pack (skill packs register one provider under the pack name).
- Keyword search does not load; it filters denied/control/allowlist-excluded
  tools before a 12-match limit. `_meta.toolsListChanged` fires on real change.
- The loaded set is per grant, survives revalidation, resets on restart; a leaf
  gone from the refreshed (revision-aware) index stops being callable.
- Plane-owned schemas: `tool_search` (required `query`), `session_ledger` (no
  arguments). A durable grant gets `503 tool_catalog_unavailable` when the
  resolver is missing, fails or returns an empty index.

## MCP door

`POST/GET/DELETE /api/magician/v2/plane/mcp` authenticates its own process-live
or durable `plt_` bearer; loopback deployment, the opaque bearer is the
authority (as in the Citizen API). Missing, revoked, expired or unknown grant →
`401`.

- `initialize` negotiates `2025-11-25`, advertises `tools.listChanged`, and
  server-mints a fresh `pltsess_` session id (never echoing bearer or client).
- `tools/call` needs a server-issued session id and a string/number JSON-RPC
  id; invented sessions and notification-shaped calls are refused.
- Replay window: process-local, per session, 256 completed ids per grant.
  Identical requests replay; reuse with different content fails closed.
  Retransmission protection, not exactly-once.
- GET: one notification stream per session (a new GET replaces it); prompts and
  results use their POST stream; disconnection does not revoke.
- `DELETE` with a server-issued `Mcp-Session-Id` ends the session (204,
  idempotent): the id stops being accepted, pending interactive calls are
  cancelled, the notification stream drops. The grant never ends with a session.
- **Session identity** is per MCP connection: terminals sharing a grant keep
  separate ledgers and replay namespaces but one dispatch lock.
- **Terminal parent engine** (`terminal_parent_engine`, fixed at `initialize`,
  info log only): the grant's engraved `harness_engine`, else the CLI family
  from `clientInfo.name` (`engine_family_from_client_name`, whole-token), only
  if installed. The session's calls and approved captures run under it
  (`grant_parent_engine`). Run and conversation grants keep Magician's choice.

## `tools/call`

- Plane-owned handlers: `tool_search` (grant-local), `request_user_input`,
  `wait_for_run`, `run_task`, `delegate_to_agent`, `session_ledger`. Others
  lower through `lower_native_tool_call` to `execute_action` when the grant has
  live `ActionExecutors`, else return `_meta.planeDispatch: unwired`.
- Pack confirmation is evaluated on the plane (the loop's `checked_approval`
  needs a `Decision` a harness never produces). Approval needed →
  `PlaneTurnStopReason::NeedsApproval`, run token not cancelled. Spent turn
  tool-call bound → `TurnBudgetSpent`: turn ends, no human asked, grant
  released. Only governed dispatches spend it (not refused, unwired or
  `tool_search` calls).
- **Per-action events**: one `AgenticActionExecuted` per governed call
  (iteration 0, observability-gated); never `AgenticIteration*`/
  `AgenticExecution*` (structural test). Terminals watch delegated runs through
  these plus hot tools `task_state`, `get_active_executions`,
  `get_execution_history`, `get_task_details`.
- **Terminal ledger** (`terminal_ledger.rs`): ≤512 entries per MCP session of
  tool, timestamp, outcome: executed (with `isError`), refused
  (`not_available`, `needs_approval`, `unwired`, `revoked`, `turn_paused`,
  `turn_budget_spent`, `input_not_resumed`) or elicitation-pending. No content,
  arguments or credentials. `session_ledger` returns the caller's own trail.
  Harness-turn calls are not ledgered here; runless sessions get no synthetic
  episode (start a task to have work remembered).

## Harness engines

`harness_engine_for` (`plane/engines/mod.rs`) has an arm per roster name
`resolve_turn_engine` uses; a drift test keeps roster and factory in step. The
decide seam builds no specific engine: a new harness is one engine file, factory
arm, roster name and dropdown entry.
`GET /api/magician/v2/plane/engines` returns the roster with install status
(PATH lookup, no spawn), `current`, `chat_current` and `native_tool_posture`;
Settings offers only installed engines.

Spawn rules: grant never on argv (create-new mode-`0600` config file or env
var); child env cleared and rebuilt from a minimal allowlist; protocol events ≤1 MiB.
Approval gate → `NeedsApproval` (kill child, keep grant); spent bound →
`TurnBudgetSpent` (release it). Start failure, Drop and shutdown revoke the
grant; Drop/shutdown kill the process group. The wall-clock ceiling
is an arm of the event select, so it also stops a silent harness.

| Engine | Shape | Native-tool posture | Streaming / resume |
| --- | --- | --- | --- |
| `pi` | Pinned Pi 0.87.1 RPC driver (shared with VibeDev), `agent_settled` boundary | Tools, extensions, skills, templates, context files, themes off; only bundled `magician-plane.js` | Native session in private Plane home |
| `claude_code` | Warm stream-json, `--include-partial-messages` | `--tools ""` + `--strict-mcp-config` (stripped) | `text_delta`; `--resume` |
| `codex` | One-shot `codex exec --json`, isolated `CODEX_HOME`, `bearer_token_env_var` | `-c sandbox_mode="read-only"` (also on `exec resume`), `--disable shell_tool`/`unified_exec` (sandboxed) | No deltas (`exec --json` emits completed items only), settles as one chunk; `codex exec resume <id>` |
| `codex_app_server` | `coding_engine` driver (`CodexAppServerAdapter::run_turn_spawned`), `CodexLaunchProfile::PlaneHarness`, child per turn | Read-only sandbox, same isolated `config.toml` (sandboxed) | Per-delta text tap (`CodingEngineRequest::text_delta_sink`); `thread/resume` in persistent `CODEX_HOME` |
| `grok` | One-shot `grok -p --output-format streaming-messages-json --include-partial-messages`, isolated `GROK_HOME` seeded with operator `auth.json` | Empty `--tools` + built-in denylist (stripped; MCP meta-tools remain). No `--sandbox`: fail-closes when `docker.sock` is a symlink | `text_delta` (never `thinking_delta`); `grok --resume <id>` |
| `agy` | One-shot `--output-format stream-json`; MCP in the **global** `~/.antigravity` via `agy mcp add`/`remove` | `--sandbox` + `--disable-slash-commands` (no strip flag) | `step_update` deltas; `agy --conversation <id>` |

- **Codex** reaches MCP only via its code-mode host, so `code_mode`/
  `code_mode_host` stay on and the plane's tools are pre-approved
  (`default_tools_approval_mode`); the door is the approval authority. Both
  Codex engines render one shared `config.toml`. Magician does not attest the
  code-mode host's confinement; the fenced VibeDev path refuses `PlaneHarness`.
  `codex_app_server` resolves its binary with
  `runtime_core::process::resolve_program` (the spawn rebuilds `PATH`); its
  driver folds message deltas (fine for the cockpit, not chat), hence the tap.
- **agy** hazard: concurrent sessions share one global registration. The add
  removes a leftover entry first and every exit path best-effort removes it,
  but a second concurrent session re-points it at its own grant.
- **One-shot engines** (`plane/engines/oneshot.rs`): process per turn, prompt on
  argv, native id from codex `thread.started`, grok init `session_id`, agy
  `init`; exit is terminal (success → Settled, failure → Refused). A turn that
  streamed settles with the streamed text alone (the final line repeats it),
  except on a failing exit, where the final line is the error and is kept. Text
  extraction is tolerant (`result`/`text`/`message`/`output`/`response`, 16 KiB
  cap) because these schemas are probed, not documented; governance is
  unaffected. Stderr keeps a 16 KiB grant-redacted tail (a straddling grant is
  replaced whole), logged at warn on a reply-less, failing or timed-out turn.
- **Resume**: only a persistent `native_home` passes or reports a native id (a
  temp home's CLI session dies with it; no cold fallback). A refused turn
  reports only an id learned that turn (driver-confirmed for
  `codex_app_server`), so a failed resume goes cold next turn instead of
  refusing forever. Settled or refused turns keep the session installed;
  config installs once per session.

### Pi harness

- **Homes**: chat uses its private Plane home; agentic turns use temp homes plus
  a private session dir under the runtime root as stable cwd (Pi scopes session
  lookup to cwd). Dirs must be private; a failed setup removes an owned home.
- Warm chat resumes send the current transcript user turn (with memory and
  attachment descriptions). Current-turn images go via RPC `images[]` (50 MiB
  total), older ones as transcript text on cold replay.
- `magician-plane.js` registers hot MCP tools plus a dispatcher for
  `tool_search`-loaded tools before the first prompt; the grant sits in a
  private file. Pi is not in the terminal-grant picker (no MCP client).
- **Credentials**: the version probe uses the same isolated env. With a
  Magician profile only its key is forwarded (a private model entry omits the
  operator's `auth.json`/`models.json`); without one, Pi uses those files
  (copied) or provider key env vars, and only its default provider, model and
  thinking level are copied from settings. `default` leaves the model to Pi;
  else `--model`.
- **Profile mapping**: adaptive profiles use their fast variant. `api_key_env` or
  `api_base_url` profiles get an isolated `models.json` entry (key by env var;
  missing key fails the turn). Providers: OpenAI, Anthropic, Gemini,
  OpenRouter, DeepSeek, MiniMax, xAI, Ollama (local endpoint always
  installed; local endpoints may be keyless). Anthropic base URLs drop a trailing `/v1`
  (Pi appends `/v1/messages`). Models Magician's Anthropic client treats as
  adaptive (`anthropic_model_uses_adaptive_thinking`: Fable, Mythos, Opus 5.x,
  4.7 and 4.6, Sonnet 5 and 4.6) get `compat.forceAdaptiveThinking`. Image/reasoning
  flags follow the chat picker; output budget is passed, other metadata is not. A profile or metadata change starts a new
  native session and replays history.
- **Agentic profile**: `execution.pi_profile` (Settings → Engines), resolved from
  the live router per Pi turn, used only by Pi-driven runs; empty keeps Pi's
  credentials. Settings refuses a
  missing or ineligible name; a later router change fails the turn.
- **Usage**: settled turns report `input_tokens` (uncached + cache-read +
  cache-write), `cached_input_tokens`, `output_tokens`, `total_tokens` in the
  `agentic harness turn settled` log and `harness_turn_settled` event; an
  operator-cancelled turn without usage does not exhaust the run's budget.
- **Budget** (since 2026-10-03): harness runs have no token meter (a harness
  re-sends its whole context every decision); each settled turn's
  CLI-reported `cost_usd` counts against
  `execution.budgets.harness_max_cost_usd` (default $100, `"unlimited"`) plus
  owner grants. No reported cost charges nothing. See
  [agentic budgets](execution/AGENTIC_EXECUTION_DESIGN.md#budgets).
- Background text operations can follow Pi through `op-harness-pi` (operator's
  installed Pi CLI and credentials); local,
  tool-carrying and pinned operations keep their profiles, and no shipped
  mapping selects Pi.

## Loop seam

`execution.harness_engine` selects what a run thinks with, inside
`phases::decide::run`. A harness turn mints a live `plt_` grant with the run's
executors and tool-call bound. `NeedsApproval` becomes
`Decision::NeedUserInput`, which apply converts (with a captured action) into
`AgenticOutcome::WaitingForConfirmation`: the ordinary HITL path.

**Work controls.** Owned Work turns capture `yield` and `need_user_input` like
Magician; dispositions come from the yield's completed/open/blocker fields, and
plain final text is only a partial deliverable. Terminal callers cannot see
these controls. Owner-input pauses persist session/home metadata; answers go
through the shared sensitive-input formatter. Cold root-owner recovery may
carry metadata-only native continuations through the signed exact-body
terminal receipt checks (never captured actions, secret approvals, delegated
ownership or foreign/missing homes); native metadata cannot reset spent
counters.

- **Internal slices**: `harness_turn_max_seconds` (86400) and
  `harness_turn_max_tool_calls` (12000) keep their remainder across pauses,
  checked before the next governed dispatch. A spent slice is a durable
  `Decision::HarnessCheckpoint`: apply rechecks cancellation, owner policy and
  run allowances and continues, never completing, failing or minting a budget
  grant (`harness_execution_preamble` v1.0.2 says slices renew and `yield` is terminal, so
  actors keep working rather than save a last call for a partial yield).
  Genuine blockers and owner questions keep their typed disposition.
- **Native sessions** (chat continuation home manager) warm-resume per
  execution and survive slices, pauses and durable recovery; a terminal outcome
  removes the owned home, native id and pending result note (paused outcomes
  keep them). Without a
  resumable session the harness asks for owner continuation instead of
  replaying. An engine/model/profile switch starts a new conversation.
- **Liveness**: quiet one-shot drivers stay alive while a dispatch holds the
  grant lock, then get a fresh idle grace; real stalls, deadlines and owner
  cancellation are enforced. A
  direct driver request keeps its hard timeout (zero = unlimited). Ordinary
  Work's active-time default is 24 hours; owner continuation adds a recorded
  allowance; reasoning may exceed a slice, never the run allowance.
- **Paused grants** are revoked once the in-flight dispatch ends, even if never
  resumed (the replaced continuation revokes on resume). The seam keeps the
  grant's captured stop and action state across adapter teardown; parent
  cancellation beats slices, approval and delegation, even stops latched
  during teardown.
- Chat harness turns count as work in flight for the chat watchdog; the
  Decision Engine checks each governed dispatch.
- **Listed schemas** go through the shared projection
  (`catalog::offered_index_entry` → `restricted_toolset::project_index_entry`):
  restricted leaves lose sender/passthrough keys and gain a note; withheld
  leaves are not listed. Work completion and input controls use the same
  projected matches. Until 2026-10-13 the declared schema was offered, so a plan
  for `agentmail-send__send` could carry `inbox_id`/`extra_args` and fail as
  `arguments_do_not_match_schema`.
- **God mode** (`execution.god_mode` or the composer orb) takes Magician's
  `shell` off the nesting floor for a live harness turn whose chat or run has
  it on, listed without a search; terminal grants keep the floor. See
  [god-mode.md](god-mode.md).
- **Decision Engine** (`decision.mode` covering the harness engine): each call
  is checked first at the dispatcher (`dispatch::decision_engine_refusal`), one
  `ResolvePlanner` step against the listed entry; rejection or an unreachable
  engine is an MCP error and the call never runs. `run_task` and
  `delegate_to_agent` runs are checked in their own loops. Until 2026-10-12 chat
  harness turns used a propose-only loop (`chat_decision_rail.rs`, removed).
- **`submit_plan`** (agent-run turns, engine on the rail; `submitted_plans.rs`):
  a sticky plan the loop runs after the turn; later calls in the turn are
  refused (`plan_submitted`); a plan from an unsettled turn is discarded at the
  next mint. Steps must name a tool from the turn's list (`mcp__magician-plane__…`
  prefix stripped) with arguments that pass
  `magician_decision::action::validation::validate_call`, else the submission
  fails in-turn naming the keys. On take, the loop loads its tools like a
  select (`agentic::load_tools_for_submitted_plan`, within the authorization
  ceiling; the rail's catalog holds only what the run loaded) and marks it
  fresh (`LoopProtectiveState.decision_rail_plan_fresh`), so the failure the
  harness planned around does not drop it.
- Protected app runs (app disclosure guard) never use a harness; the seam fails
  closed to Magician.
- **Parent engine**: `run_engine_for` (launch pin, else snapshot) parents the
  run's background LLM operations (`run_parent_engine`; `magician` ⇒ none),
  set on the scoped router and decide adapter (`routing_overrides_for_run`,
  never from the launch request), scoped around every phase and carried by
  `CapturedRunTaskLocals` across lanes and delegation hops. See
  [llm-routing-overrides.md](llm-routing-overrides.md).

### Execute-on-resume

On an approval stop the plane captures the gated action (the loop's stable
serialization); the durable pause
carries it, the native session id and budget remainder as a `harness_pause`
continuation in `FullPauseData`. On a confirmed resume that passes routing and
transition-authority admission, Magician **executes the action itself** via
`execute_action` (fresh trust guard, deterministic effect id bound to pause and
action) after re-checking the restored agent allowlist (refusal: "NOT executed"
note). No replay entry: the next harness turn opens with the **result**.

- Any harness pause restores the warm continuation; loop confirmation gates
  keep their one-shot replay waiver; the `inprocess` rollback arm keeps
  confirmation replay. Publication reads the continuation non-destructively.
- **Boundary**: outside the durable Apply intent machinery; a death between
  execution and the first resumed LoopState commit is not covered by
  effect-ledger recovery (the effect id correlates retries, not exactly-once).
- **Not built**: the result reaches the harness and continuation but is not
  appended to the run's iteration record.

## Delegated and runless runs

**Delegated runs.** `PlaneRunAuthority` on the grant carries `allowed_agents`,
`harness_engine` and ceilings. `run_task` routes to `run_ownership` before
lowering: no authority → refused naming "run authority"; unlaunchable engine →
"cannot launch"; explicit `magician` is a pin. Launches do not spend the turn
bound. `start_plane_delegated_execution` carries `PlaneDelegationAttenuation`:

- Denied = `NEVER_ON_THE_PLANE` **plus `run_task` itself** (it launches
  unattenuated). Sub-work uses `spawn_sub_goal`/`DelegateToAgent`, which
  inherit the denial.
- Persisted (`plane_attenuation.json` + `plane_attenuated.json` marker, task
  write guard) **before** the run can dispatch and loaded at both
  dispatch-composition sites; a marker without a readable payload fails closed.
- `plane_denied_capability_names` joins the profile deny list (inherited);
  non-empty `plane_allowed_capability_names` intersects the catalog at
  `build_catalog_context` (umbrella or `<pack>__<action>`);
  `ctx.harness_engine` overrides the snapshot. All three ride
  `AgenticPauseState` and are restored on resume.
- Ceilings `max_usd` and `max_wall_clock` only narrow the cost cap and work
  budget (min with the configured cap plus owner budget grants) and persist with
  the attenuation. `max_concurrent_runs` (default 4; mint sanitizes zero to
  unset) refuses at launch via a process-local grant-keyed ledger pruned
  against the execution store; a restart resets it.

**Runless (terminal) dispatch.** Each durable terminal grant dispatches through
its own copy of the process-global executors (`for_runless_scope`) seeded with
the scope's registry, allowlist and grant identity, so scope-bound compiled
tools get `__principal`/`__agent_id`. Narrowing keeps each allowlisted leaf's
parent pack and declared provider; the door's exact-leaf gate bounds what is
callable. Features needing a live execution (sandbox widening, browser session)
refuse naming `run_task`.

**Grant management.** `GET/POST /api/magician/v2/plane/grants` and
`DELETE /api/magician/v2/plane/grants/{id}` take only a session bearer (`plt_`
cannot mint siblings). Mint derives the workspace from that credential,
validates ownership, floors the
allowlist, bounds TTL, returns the token once and reports floor-dropped tools.
Durable rows carry `max_usd`, `max_wall_clock_secs`, `max_concurrent_runs`;
every durable projection gets run authority (a started run inherits the
engraved harness). Revocation applies on the next call. UI:
`TerminalGrantsPanel` in `ui/unified-ui`.

## Run engine pin

A run fixes what it thinks with at launch: `RunEnginePin` (`plane/engine_pin.rs`:
`engine`, `harness_model`, `pi_profile`, written together by Settings). A
Settings switch or reload moves only later launches; running, paused and
recovered runs keep their pin. Rule: **inherit the parent unless something names an engine**.

- **Resolution** (`resolve_launch_pin`): an engine named for the run (plane
  grant) wins, on its CLI's `default` model if it differs from the parent's or
  Settings'. Else `LAUNCHING_RUN_ENGINE_PIN`; else (API, scheduler) the task's
  creation pin; else Settings.
- **Launching pin set by**: a chat turn (`chat_turn_run_pin`: composer, a voice
  call's `session.start`, else `chat.harness_engine`/`chat.harness_model`; Pi
  profile from composer, else `execution.pi_profile`; uninstalled CLI →
  `magician`), carried by its conversation grant into every tool call, approved
  capture and bridged job; a run (`CapturedRunTaskLocals::for_context`, across
  lanes and hops); the goal trigger onto its runtime job.
- **Durable records**: `engine_pin.json` per execution (written at start,
  accepted launches, recovery, goal trigger, and for delegated children from the
  parent; only a plane grant overrides an existing pin; task write guard);
  `launch_engine_pin.json` per task (`create_task`, read by first launch);
  taskless roots under `runtime/taskless_engine_pins/` (hash of execution id).
  Unreadable files read as absent with a warning: a pin never wedges resume.
- **Composition**: the orchestrator loads the pin by execution id at its one
  compose point, else pins the Settings choice then (`RunEnginePin::for_engine`); `AgenticContext::with_run_engine_pin` sets
  `run_engine_pin` and `harness_engine`, so `run_engine_for` never reads the
  snapshot.
- **Pauses**: `AgenticPauseState.run_engine_pin` and `harness_engine` join the
  pause's authorization hash when present (older pauses keep their hash).
- **Not pinned**: turn limits, the plane endpoint. Background operations follow
  the engine family (`op-harness-<engine>`), not the harness model.
- **Coding tasks** without `coding_profile` map the pin
  (`inherited_coding_choice`): `pi` → the coding profile whose `llm_profile`
  is the pin's; other engines → their counterpart if `adapter_spec_for_engine`
  can launch it, on its default model; `magician`, non-Ready or unmapped → Pi
  on `coding.default_profile`. VibeDev inherits at admission
  ([vibedev-rail.md](vibedev-rail.md)).
- **Session fingerprint**: `session_fingerprint` (engine, model, profile) is
  stored with the native id; a mismatch starts cold.

## Chat mouth

`chat.harness_engine` (roster, default `magician`) swaps the chat's mouth,
independent of the loop; unknown names fail closed to the LLM path.
`PUT /api/magician/v2/plane/chat-engine` persists it (YAML + live snapshot).
Each send carries engine, model and profile (queued turns keep their route);
Settings and the composer share one browser-local choice, the server key is the
fallback.
Protected (disclosure-guarded), voice/meeting, App Copilot, Tutor and
public-envoy turns never use a harness; any fallback to Magician (guarded
disclosure, CLI missing, factory failure) abandons the warm continuation first.

**Grant.** A **ChatScoped** `plt_` grant keeps the turn's invocation surface
(run and terminal mints stamp Plane),
inherits its tool allowlist (empty skips the harness) and uses
`scoped_runless_executors`. Revoke cancels a child token, so a settled turn
does not abort the conversation. Gated plane actions refuse: chat HITL is the
next user message.

**Hands.** Direct and Deferred tools cross under their own names; the grant
carries the turn's scoped tool index, so non-hot Deferred names load through
`tool_search` (itself allowlisted). Engines with
`tools_list_changed: false` (codex, codex_app_server, grok) get Deferred hands
at mint. Each chat-runtime tool declares a `PlanePosture`:

- **Counterpart** rides the grant (task details → `get_task_details`, describe
  agents → `get_agent_details`).
- **Bridged**: the native implementation via a per-turn `ChatMouthBridge`
  (`ChatService::build_mouth_bridge`, `BridgeTurnContext`). The set
  (`PlaneGrant::bridged_tools`, `plane_bridged_specs`) is the native mouth's
  specs minus plane-executed and non-Bridged names, under the floor
  `chat_harness_bridgeable`: `NEVER_ON_THE_PLANE`, door verbs,
  `LOOP_INTERCEPTED_VERBS` (e.g. `request_thinking_mode`) and governed app
  tools never bridge; of control verbs only `read_result`.
  `NOT_A_PLANE_EXECUTED_HAND` names (e.g. `switch_personality`,
  `activate_skill`, `deactivate_skill`, `create_task`, `run_task`) bridge from
  the tool index (`floor_rejected_compiled_specs`): that is how a swapped mouth
  creates a task.
- **MouthOnly** is withheld.

The plane-execution floor (`NEVER_ON_THE_PLANE`, control verbs, mouth-tool
names, governed app tools) is one predicate, `chat_harness_floor_rejects`,
shared with the posture test.

**Bridged calls** are advertised hot with native name, description and schema,
added to an attenuated allowlist and routed to the bridge before lowering with
no plane approval capture: `dispatch_chat_tool_call` applies
its own approval, trust, deny and allowlist rules. Otherwise they count like any
turn call (bound, revocation re-check, `pltinv_` call id, events, ledger). No
calling profile (security-sensitive handlers fail closed); governed app results
are refused (`governed_app_result_not_admitted`); results pass
`sanitize_json_for_provider` first; `isError` follows native status. Bodies run
on `runtime_boundary::spawn_execution_job`, never the HTTP worker; their
subscriptions join the turn's once the harness settles.

- **Cancellation**: the job races the grant's dispatch token; on revoke the
  dispatcher gets `BRIDGED_CANCEL_GRACE` (2 s) to answer (dropping it would leak
  its fan-out and subscription), a late one unwinds detached, and the door
  returns revoked. No result → `bridged_call_lost`.
- **Two tokens**: the per-call token dies when the turn settles, so
  `delegate_to_agent`, `handover_to_agent` and `orchestrate_pipeline` get the
  turn's abort token (`bridged_call_cancel_token`: Stop, teardown, superseding
  turn) for their cascade watchdog.
- **Persona change**: a bridged `switch_personality`/`activate_skill`/
  `deactivate_skill` that changed state (judged from the recorded result, not
  the name) ends warm resume (`abandon_chat_harness_resume`), since the native
  session would keep the old system prompt.

**Parent engine.** The chat mouth parents the turn's background LLM operations
(`chat_turn_parent_engine`), scoped around the inline turn and re-scoped onto
its execution-runtime hops; conversation grants set
`ctx.llm_routing_overrides.parent_engine`.

**Events and transcript.** Conversation-grant calls emit
`tool.call.started`/`tool.call.finished` on the chat turn (`via: plane`,
`harness_engine`, redacted arguments; over 16 KiB → omission record; errors
4 KiB head). The grant's `turn_ledger` (per dispatched call; refusals
excluded; drained after revoke) is persisted like the native mouth: an
`AssistantTurn` of calls (`call_id` = `pltinv_` ref), a `ToolResult` each
(8 KiB head, ` …[cut]` marker; errors `{"status":"error",…}`; governed app
content omitted), then the reply, including failed and cancelled turns.
Bounds: `PLANE_TURN_LEDGER_MAX_BYTES` (512 KiB, then id/name only), batches of
`HARNESS_TRANSCRIPT_CHUNK_CALLS` (200). A failed batch append is logged and the
turn continues. Run grants keep a ledger too: `harness_checkpoint_note` drains
it into bounded receipts for a Work continuation. Terminal grants have none.

**Cold replay**: 512 B argument heads after the JSON sanitizer; results through
`sanitize_text_for_provider`, 4 KiB each; 48 KiB total, then omission lines.
Stale task-spawning calls (`ChatLlmService::is_stale_replay_neutralized_tool`)
before the last user message are named only. The current user text is
appended only if history does not already end on the user's side.

**Reply streaming.** All engines but `codex` stream through the plane sink
(Grok and agy settle with streamed text, 16 KiB). A streamed reply is persisted
whole from collected deltas; settled text goes as one chunk when nothing
streamed. A non-streamed reply is read per JSON line, else whole stdout as one
document, bounded at the event cap. Paid-call accounting records
harness tokens at **cost $0 / no model id** (lossy `HarnessUsage`).

**Continuation and native home.** Per chat session and **process-local**
(restart ⇒ cold harness, history replayed); warm resume sends only new user
text. Resumable engines get `<runtime root>/.magician-storage/plane/homes/<uuid>`
per conversation (0700, in memory only) as `HarnessSessionRequest::native_home`,
removed by the continuation on Magician takeover, persona change, teardown
(`forget_chat_harness_conversation`) or engine switch, never by a session.
Startup keeps existing homes (an older process may still use them); a crashed
process's homes wait for separate maintenance. A cancelled turn drops the id,
keeps the home; a spawn failure keeps both; a native id is reused only with its
home. Non-resumable engines get a temp dir per session; under `cargo test` the
root is a scratch directory.

## Config

| Key | Default | Meaning |
| --- | --- | --- |
| `plane.spawned_bare.hot`, `plane.terminal.hot` | empty = lists in `execution::plane::catalog` | Non-empty replaces; a `spawned_bare.hot` overlay only picks hot allowlisted names; dropping `tool_search` disables deferred loading |
| `execution.harness_engine` | `magician` | `magician` \| `claude_code` \| `codex` \| `codex_app_server` \| `grok` \| `agy` \| `pi` |
| `execution.harness_turn_max_tool_calls`, `execution.harness_turn_max_seconds` | 12000, 86400 | Internal slice bounds (Magician-run scale) |
| `execution.harness_model` | `default` | Model the run's harness uses (`default` = the CLI's) |
| `execution.pi_profile` | unset | Pi agentic profile |
| `chat.harness_engine`, `chat.harness_model` | `magician`, `default` | Chat mouth (`default` = the CLI's model) |
| `chat.harness_turn_stall_seconds` | 300 (0 = no stall stop) | No-progress bound (native and decision rail) |
| `chat.harness_turn_max_iterations` | 200 (0 = default) | Native loop backstop; ends with a no-tools hand-off reply. See [chat mode](chat-mode.md#purpose) |
| `chat.chat_pack_direct_dispatch` | `true` | Kill switch: fully specified pack calls run pre-decided without a model. See [chat mode](chat-mode.md#chat-pack-calls-pre-decided-vs-model-driven) |

`POST /settings/magician-config/reload` updates the snapshot. The last three
install at boot and reload, and the chat-engine writer preserves them.

## Endpoint

`RevokeGrantOnDrop` and the dispatch-lock helper are public (hidden from API
docs) for the `magician-bin` execution contract test. The conformance lane
(`scripts/eval_harness_plane.py`, `make test-plane-harness-live`) drives the
door like an external harness:
[eval lanes](eval-lanes.md#harness-conformance-plane-lane).

## Typed input

Terminal MCP clients that declare `capabilities.elicitation.form: {}` (or
`elicitation: {}`; not URL-only) at `initialize` can answer text, selects and
forms and approve governed actions. Prompts arrive as `elicitation/create` on
the original tools/call POST SSE stream; the answer is POSTed with the same
session and elicitation ID (empty HTTP 202) and the result follows on the
original stream. Approval needs `action:"accept"` and boolean
`content.confirm:true`; anything else never executes. Answers must match
grant, session and request ID and are consumed once; terminal captures are per
call, outside the live harness approval slot.

Plane-owned tools (subject to allowlist and hot catalog):

- `request_user_input`: returns a typed `UserInputValue` in
  `structuredContent.answer` (`question`, `input_type`, `options`,
  `questions`).
- `wait_for_run`: takes the `execution_id` from this session's `run_task`;
  relays pending questions, including from verified child executions. Refuses
  other sessions' runs (even on the same grant); one wait per run.
- `delegate_to_agent`: live run turns only; captures `targets`
  (`target_agent_id`, `context`, optional `input_data`) and ends the turn as a
  `DelegateToAgent` decision; the next turn gets the children's deliverables.

Form limits: 32 fields, 128 options, 64 KiB per text answer. Omitted = skipped;
empty values are answers. Unknown fields or options, duplicate selections,
missing required fields and mistyped answers refuse without resuming.
`allow_other` adds `other` plus nonempty `other_value`.

Service-backed asks carry a task-local route bound to the grant's
principal/workspace; `UserRequestService` still owns acceptance, publication,
timeout and first-response-wins (a UI answer can win). Agentic pauses go
through the scoped resume owner and compare an opaque revision of question and action, so a reused pause key
cannot take an old answer. A form answer grants nothing later.

Timing: prompts time out after five minutes; invalid responses end the call.
`notifications/cancelled` affects only the named call (never the run).
`wait_for_run.timeout_secs` 1–300 (default 300); after an answer the wait holds
up to 20 seconds to surface a refusal's HTTP status. Authority is revalidated
before answering or executing a capture. A disconnected POST is not
cancellation: an identical retry returns the cached result. No Event-ID stream
resumption.

Process-local limits per grant: 256 sessions, 32 interactive calls, 256
run-origin slots; capacity refusal precedes launch.

Not projected as forms (need Magician's UI): passwords/API keys, diff reviews,
multiple file paths, service options with conditional `requires_input` (no
URL-elicitation flow). Unsupported delegated-run input leaves the pause pending;
unsupported service-backed input in a direct call is cancelled through its
response owner. Magician-spawned harness turns keep the EndTurn/loop HITL path.

Tests: `make test-plane-elicitation`
(`magician-bin/tests/plane_elicitation_contract.rs`). Wire:
[MCP elicitation](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation),
[Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

Work checkpoints and approval pauses carry recent completed host tool receipts
from the Plane ledger (for a CLI that exits before reading a result). Notes
name omitted older receipts and merge the approved-action result on resume.
