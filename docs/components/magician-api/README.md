# Magician API

Version 0.3.43 adds scoped receipt correlation, confirmed calendar transitions, exact reply Work routing and bounded follow-through pagination.

Quality owner preview resolves scoped `native_harness.metered_tools` capability
pins and includes their call ceilings and maximum charges in the attempt quote.
Task policy admission resolves the same pins; changed tools require a successor
reviewed contract. No provider name or output format determines eligibility.

Ordinary task budget GET/POST routes return `409 external_controller_budget_frozen`
for a controller-owned root, including Relentless. Its reviewed contract owns
the limits; the endpoint writes no grant and performs no resume. Unverifiable
ownership returns `503 execution_budget_owner_unavailable`. The regression fixture
uses an isolated auth store, an owned workspace and a real session bearer through
production authentication middleware, so it checks owner access as well as the
controller's frozen budget boundary.
The Relentless API regression covers both ordinary budget methods and checks the
JSON conflict code, keeping these guards in the workspace all-targets check.

Artifact evidence inspection includes `admitted_sources`: each original frozen
reference's identity, content hash, byte length and exact UTF-8 text. Non-text
sources expose metadata with `text: null`; binary media is not inlined. References
are read from integrity-checked run blobs, never mutable note paths or later
owner answers. Existing artifact membership, verified owner scope and `no-store`
response guards apply. The Work evidence panel renders this additive field for
reviewing an already admitted draft against its request and fact sheet.

`POST /relentless/runs/{id}/resume` requires the canonical HITL answer only when
the current attention names that unfinished actor checkpoint. A retained
checkpoint from an already answered question cannot misclassify a later
infrastructure blocker as an unanswered request. Run, attention, deadline and
budget checks still apply; status resume supplies no new factual evidence.

Scoped ordinary Work reply/continue admission can recover an unchanged root
owner after restart from its existing exact, scope-authenticated terminal
receipt. Verification happens before consuming the durable pause and binds its
complete continuation. A missing verifier or invalid source preserves the pause;
additional approval grants retain the existing boot-local authorization gate.
The server execution record and requested execution bind this admission;
stripping saved owner/source markers cannot bypass the receipt requirement or
the existing staged-authority check.
Ordinary resume rejects an execution/request scope mismatch with HTTP 403 before
reading continuation ownership or consuming any pause. Same-boot ordinary roots
retain their staged owner authority while normal input-type validation runs.
See [pause persistence](../magician/pause-and-chat-persistence.md).

**Current development version:** `0.3.43`

API `0.3.30` supports scoped provider-health notice dismissal through the existing HITL response endpoint.

The HTTP/WS surface of the Magician service, extracted from the `magician`
monolith as a **satellite crate** over the `magician` lib. All routes below are
under `/api/magician/v2` unless stated otherwise.

## Why a satellite

`api` was the aggregation point nearly every surface hung from. Lib-module
dependencies were drained into the lib first (VibeDev project store, task lanes,
HITL metrics, observe-connector config IO, monitor support, event-scope
visibility, task ownership, the task-run factory, screen capture, scope
resolution, the today-projection cache, canonical attention, the memory API, the
MCP OAuth broker), each re-exported from its old api path, then `api/` moved
wholesale. Editing api code rebuilds only this crate, and surfaces only api
consumed (`channel_assist`, `attention_learning`, `resurfacing`, …) are
extractable behind it.

Depends on `magician` (lib), `runtime-core`, `tool-runtime-core` (exact MagicRun
pin), `magicllm`, `magician-vector-index`. Secret APIs keep Magician's
compatibility facades over `magicvault-core`. The server binary lives in
`magician-bin`. The monolith's `pub(crate)` items crossing the boundary were
widened to `pub`.

`GET` and `POST /meetings/whatsapp-ring` handle a WhatsApp call that is already
open in everyday Chrome. `join` starts the voice loop while a call is ringing.
`listen` starts passive capture for a ringing or ongoing call. Idle does not
start a session. Neither decision calls `/meetings/join`. `profiles` is the
selectable realtime catalog, the same list as `GET /media/providers`. The
stored default profile is `voice_realtime_openai_backend`. The request bearer
is the Join voice-session token. `POST /meetings/join` accepts an optional
`realtime_profile` from that same catalog. Omitted keeps
`MEET_BOT_REALTIME_PROFILE`.

## Shape

- Media and UI preference stores resolve the workspace through
  `magician_v2::process_storage`, never `MAGICIAN_ROOT_DIR`. Durable
  object/dataset/lease I/O uses `StorageRuntime::current()`. API mining
  projection SQLite opens through `database_file_path(..., ApiMining)`.
  Durable-artifact REST and the realtime WebSocket handshake extract
  `api_scope::ResolvedScope` via `LocalPermissive`.
- The realtime WebSocket (`websocket_handler.rs`) is the app's single event
  hub: `hub.hello` first, `seq`/`epoch`/`categories`/`user_relevant`/`refs`
  on every event frame, `resume_epoch`/`resume_seq` replay from the
  broadcaster's 8192-event ring, and `hub.resync` instead of a close on lag
  or a full mailbox. `events_api.rs` classifies through the same
  `realtime_events::classify_transport_event`. Contract:
  [`v2-websocket-events.md`](../magician/v2-websocket-events.md#event-hub-sequence-resume-resync).
- Product-lane shaping lives behind seamed modules with unchanged routes:
  `media_ux::{dictation, session_controls}`, `crew_surface::{projection,
  health}`, `magician_v2/notes_projection.rs`, `magician_v2::audio_notes_seam`,
  `magician_comms::channel_assist::assist`. Attention-learning/resurfacing
  handlers import engine types from `magician::magician_v2::attention::`.
- Supervisor background loops (`magician-bg`) take their worker count from the
  pre-bootstrap `runtime.scale` plan (`current` is 4).
  `POST /settings/magician-config/reload` lists `runtime.scale` and
  `MAGICIAN_SCALE_PROFILE` as restart-required. It live-reloads
  `planning.analysis_path` (reported as `planning.analysis_path`).
- HTTP handlers durably seal and schedule direct/planned work through Artifact V2
  before returning 202. Control mutations use scoped lifecycle exclusions; the
  canonical Runtime/Artifact projector owns task and execution status.

## Cross-cutting contracts

- **Startup.** The [startup contract](../magician/startup.md) exposes public,
  coarse readiness at `/startup` and `/api/magician/v2/startup`. Feature requests
  get retryable 503 until every HTTP worker has its full application. Agent
  hydration and supervisor loops wait for this barrier.
- **Auth boundary.** Ordinary API routes reject plane terminal grants.
  Login/session responses expose the canonical bearer principal. Public bootstrap
  routes discard caller scope headers.
- **Error mapping is a cross-crate contract.** Handlers translate lib error
  enums with exhaustive `match`es (`installation_review_error_response`,
  `registry_error_response`, …), so **adding a lib-side variant breaks this
  crate** and a new failure can never reach a client as an unlabelled 500. Give a
  variant its own code when it means something distinct
  (`app_installation_grant_unreadable`: the grant in force could not be read, so
  a review must not render as "nothing changed").
- **Spawn resolution.** `spawn_skill_step` and the VibeDev deploy runner override
  `PATH`, so each resolves its program with
  `runtime_core::process::resolve_program` first (bare name + PATH override makes
  Rust `fork`, which can hang in macOS atfork handlers). The skill step's
  `config/.env` applies after the PATH prepend and wins. See
  `docs/components/runtime-core/runtime-core.md`.
- **Large futures are boxed at the boundary.** An `async fn` stores its callee's
  state machine inline, so handler chains multiply stack use on 2 MiB workers.
  Boxed: `ensure_agent_startup_hydrated` (returns `Pin<Box<dyn Future>>`, awaited
  by ~41 handlers, runs at most once per process), `run_agent_startup_hydration`
  at its call site, the task-backed-root provisioning job (`with_secret_scope`,
  `ensure_dispatch_task_backed_root_inner`,
  `create_v3_task_execution_shell_boxed`), `MagicianV2Api::create_execution`
  (definition-level boxed future, pointer-size regression test), the
  agentic-resume handler, `resume_agentic_execution_with_scope_inner` (awaits
  children via `HeapAwaitExt`), and app launches (`launch_direct_app_action` and
  `execute_admitted_bridge` `Box::pin` their `invoke_direct_input`; any new
  adapter over an app launch must too). Boxing moves storage to the heap but does
  not shrink a single function's own frame — split those instead. Rule: box
  handlers that await a provisioning, hydration or dispatch chain. Test lanes do
  not raise `RUST_MIN_STACK`, so an unboxed entry point is detected. See
  [runtime async stack boundaries](../magician/runtime-async-stack-boundaries.md).
- **Cached-response headers.** Surfacing, maintenance and background-behavior
  responses are `private, no-store`.

## Agentic resume and HITL respond

- **Detached settlement.** The resume handler runs validation, the loop's job and
  settlement (next pause persisted, `Executing → WaitingUser`, outcome
  projection) as its own `tokio::spawn` task and awaits the join handle
  (`MagicianV2Api::resume_detached_from_request`). Why: Actix drops a handler's
  future on client disconnect, and settlement in the request future would leave a
  delegated child's next ask with no pause, no card and `Executing` forever. The
  task returns status, content type and bytes (`HttpResponse` is not `Send`).
- **Respond acknowledges on admission.** `POST /hitl/{id}/respond` uses
  `resume_agentic_execution_acknowledged_when_admitted`: it races the spawned
  resume against a `loop_admitted` oneshot fired once the answer passed
  validation. Earlier exits (re-ask, abort, missing pause, error) return as
  before; after admission the client gets
  `202 {accepted: true, reason: "resume_admitted"}` and later pauses arrive as
  events. The plain resume keeps its synchronous contract.
- **Missing pause.** A resume that finds no pause finalizes a possibly-orphaned
  execution (`ArtifactV2Service::finalize_orphaned_user_pause_by_execution_id`),
  after the handler drops its own lifecycle exclusion (the in-process fence is a
  non-reentrant tokio mutex). Every in-process fence wait is bounded (30 s,
  `STATELESS_LIFECYCLE_PROCESS_LOCK_WAIT`); the durable flock beneath gives up
  after 5 s.
- **Controller-owned questions.** The authenticated HITL responder resolves the
  existing scoped external-output owner before ordinary pause consumption.
  Read-only ownership inspection selects this bridge; an ordinary execution
  keeps the existing HITL route and does not acquire Relentless's stricter
  owner-admission gate merely because it has an execution ID. The controller
  wire address also selects strict inspection when its owner is unavailable.
  Registered native Relentless questions go through the owner service's exact
  checkpoint/question gate; a durable normalized answer receipt returns
  `200 {accepted: true, reason: "owner_input_recorded"}` and wakes the existing
  controller. Sensitive values use existing scoped custody and only references
  enter the receipt; identical replay reuses it without re-registering one-time
  material, while conflicting answers return 409. Receipt publication,
  resolution and controller wake settle on a detached task, so client
  disconnect cannot strand an authorized answer after its durable commit.
  Ordinary resume refuses an
  externally owned root with `external_controller_owns_execution` before taking
  its pause. The bridge is source implemented; native restart/answer completion
  and API row-retention qualification remain pending. Status-only Relentless
  resume refuses an unfinished actor checkpoint; the exact question needs an
  answer through HITL rather than a status toggle.
- **Existing-task Relentless policy.** The verified owner reads
  `GET /relentless/task-context/{task_id}` and may include
  `existing_task: {task_id, context_hash}` in the reviewed run start. Admission
  preserves the task goal, agent and thread and uses the existing root factory.
  It refuses active/unsettled prior work, stale task configuration and changed
  output ownership. An interrupted admission returns its original run ID and
  requires the exact reviewed brief on retry. These API/root-recovery regressions
  are part of the pending source batch, not an executed test result.
- **Declared artifacts.** Relentless proposals may include `artifact_review`
  file/source representations and base64 `source_files`. Preview returns the
  resolved recipe/specifications with the contract hash. The native task actor
  seals exactly the declared files; video review uses pinned visual samples.
  `GET /relentless/runs/{id}/artifacts/{hash}` retains text `files` and adds
  binary file metadata. `GET .../file?name=<declared_name>` returns exact sealed
  bytes through the same verified owner scope, with attachment/no-store/nosniff
  headers. It cannot read arbitrary filenames or another run's blob. These
  paths are source-connected and await batched qualification.
- **Sealed artifact evidence.** Verified-owner
  `GET /relentless/runs/{id}/artifacts/{hash}/evidence` returns exact
  candidate-bound fetched source text/provenance, cached verification results
  and cached sampled-audio receipts with a content hash. It checks scoped run
  membership and immutable evidence references; it never executes a command or
  prepares new media. Artifact downloads now include declared WAV/MP3 types.
  The profiles catalog exposes bounded zero-cost audio review providers and
  their exact model/configuration pins. These source paths await qualification.
- **Run reports.** Verified-owner `GET /relentless/runs/{id}/report` returns
  journal-derived typed results, a content hash and Markdown snapshot with
  selected artifact/review evidence, stop reason, open items and current spend.
  Capture coverage is returned separately. This is read-only and cannot accept
  bytes or grant dispatch authority; the response disables caching. Source is
  connected, with validation deferred for the implementation batch.
- **Recurring task policy.** Verified owners activate/revoke
  `POST /relentless/task-policy/{task_id}` with the reviewed context/contract,
  exact expected current policy ID, new policy ID, finite `max_runs` and explicit
  `expires_at`. Ordinary starts and scheduled occurrences use the same native
  controller admission. Accepted roots atomically consume a run allowance;
  protected launch snapshots repair admission after restart. Changed pins or
  task context require new review. Revocation stops future starts and preserves
  prior counters and admitted budgets. No generic worker fallback is allowed.
  `GET /relentless/runs/{id}/lessons` exposes observed review evidence; terminal
  publication also files it in the existing learning review queue. These source
  paths remain uncompiled/untested during the owner-requested implementation batch.
  Owner `POST /relentless/runs/{id}/reconcile-costs` retries terminal billing
  using local durable receipt metadata or a fully settled native child ledger.
  It cannot resume productive work; missing/unknown proofs retain ceilings.
  Startup recovery uses the same path. Published lesson bytes stay immutable
  and their response includes `current_spend` separately from the sealed snapshot.
  `GET /relentless/work?goal_id=<exact_id>` projects the scoped existing task
  pipeline, including deadlines, controller runs, policy bounds and spend.
  It adds no scheduling or outward authority and does not mark captured drafts
  as external submissions or deliveries.
- **Procedural guidance qualification.** Verified owners preview
  `POST /relentless/guidance-policies/preview` with `policy_id` and `spec`, then
  activate `POST /relentless/guidance-policies/{id}` with that exact spec and
  `expected_version_hash`. List/inspect use GET on the collection/ID; `/revoke`
  and `/resume` take the exact expected version. Resume applies only to a paused
  frozen qualification, preserving cases/reservations; it cannot revive rejection
  or revocation. Policies pin a strict learning candidate, baseline, terminal
  training provenance, fresh paired evaluation/canary drafts, independent review,
  separate finite budgets and exact recurring draft targets, adoption cap/expiry
  and regression rules. Trials reuse existing metered task-backed admission and
  match the targets' native actor recipe, artifact kind and actor model/family.
  Unknown evidence/usage never passes. Qualified procedures require explicit
  bounded admission and do not enter ambient learning retrieval. Adoption keeps
  the ordinary task's acceptance/tool/authority policy; capture targets require
  separate review. Regression disables future adoption and archives guidance.
  These source endpoints/UI/workers have not yet been compiled or qualified.
  Trials use internal task storage and their owning output path skips generic
  reflection/episode repair. Generic memory backfill refuses externally owned
  roots. Exact-input reuse audits ignore source aliases/rubric edits; suite
  semantic independence is owner-curated. The explicit
  `monitor_nonacceptance_is_regression` flag includes cancellation; missing
  reviews remain unknown. Expiry/revocation and procedure status updates reuse
  the policy journal and existing version-checked store transition.
  Trial observations include a content-free sealed operator-redirect count;
  assisted trials cannot qualify guidance. Definitive case failures stop the
  remaining suite. Phase/promotion replay keeps the existing decision rather
  than advancing or reactivating it again. Strict candidate bodies may be direct
  or in the standard single `procedure` wrapper.
  Paused qualifications retain target reservations until revoked. Activation and
  resume share the overlap lock; equal peer observations retain the first proof.
  Unavailable guidance disables adoption and lets new roots use the reviewed
  baseline; an interrupted guided reservation cannot change its snapshot.
  Revocation/expiry/failure durably cancels admitted trials; run recovery checks
  the frozen trial policy before productive continuation. Known terminal receipts
  still settle phase costs after revocation, without resuming qualification.
  Trial admission pins an absolute parent `not_after` ceiling at policy expiry;
  the ordinary controller/physical-call deadline enforces it. Verified owner GET
  `/relentless/guidance-policies/{id}/evidence/{run_id}` returns the exact sealed
  trial/monitor snapshot and recorded hash with no-store, refusing an unrecorded
  or cross-policy run. It validates origin, scope and contract without generating
  new review evidence.
  Ordinary agents can propose strict guidance through the existing task tool's
  `propose_guidance` operation. Its runtime-stamped candidate remains review-
  required, with terminal same-agent training provenance and no trial/activation
  authority. Owner policy admission includes all declared candidate training runs.
  Verified owner GET `/relentless/guidance-proposals` lists pending typed agent
  proposals from the existing learning queue, including their exact body and
  training IDs; this read activates nothing.
- **Campaign captures.** Verified owners list `GET /relentless/campaigns` and
  activate/revoke `POST /relentless/campaigns/{id}` using an exact expected
  version and reviewed structured policy. Existing task-context hashes and
  counterparty identity IDs are mandatory. `POST .../{id}/captures` binds the
  exact accepted capture artifact, run and target after verifying all file
  hashes. `POST .../{id}/captures/{capture}/review` binds the owner decision to
  that capture hash. Reservations consume finite total/day/recipient slots;
  retries are idempotent and changed policy/content refuses. Audit rejections
  suspend automatic capture. These counts are simulated and grant no live
  delivery authority. The source implementation awaits batched qualification.
  Owner `POST /relentless/campaigns/preview` validates the structured policy and
  resolves its approved template hash without activation. Automatic capture
  requires source-bound slots, exact rendered payload/attachment hashes, pinned
  normalized addresses and a prior clean approval for that identity. Template
  failure asks for exact review. Capture notices use stable scoped attention
  IDs; dismissal/expiry cannot approve a capture and boot retries publication.
  Existing-task `/relentless/runs` admission and `/relentless/task-policy/{id}`
  accept optional `capture_handoff` with exact `campaign_id`, `version_hash`
  and `identity_id`. It requires a capture contract and matching task context.
  Admission/root ownership pins this target; terminal projection automatically
  binds its accepted artifact. Policy/capacity refusal preserves local output
  with an open item and one attention notice. Run GET/list views expose
  `capture_handoff` target/receipt/availability. Historical receipt lookup
  does not require fresh policy coverage and creates no new act or authority.
- **Recovered pause executors** get the run's canonical event scope
  (`with_canonical_event_scope_for_resume`: registered scope, else one built from
  the pause's ids), so resumed events are persisted as runtime facts rather than
  routed transport-only, and the scope's MagicVault secret store
  (`with_scoped_secret_store_for_resume`), so an answered password stored in
  `resolved_inputs` is registered ephemerally and `[REDACTED:password]`
  placeholders resolve at dispatch (`inject_inline`).
- **Resolution timing.** The resume announces the answer (`AgenticResumed` +
  canonical `HitlResolved`) when the answer is admitted — after the fence is
  released, before the loop runs — because all pauses of a run share a
  correlation id and a later resolution would hide the next ask. A validation
  re-ask or loop failure restores the pause and re-requests it canonically
  (`emit_hitl_requested_for_restored_pause`). See
  `docs/components/magician/hitl-attention.md`.
- `magician`'s executor identity is one copy-on-write `RunIdentity`
  (`ActionExecutors::run_identity`); pause read/write uses a single snapshot so a
  resumed pause cannot name a scope neither half held. `FullPauseData.harness_pause`
  is the plane-captured action a harness approval pause waits on.
- Exact taskless Attention reads consult the private HITL SQLite authority
  across restarts and fail closed with 503 when it cannot be validated (macOS
  tests canonicalise the temp root because `/var` → `/private/var`, keeping the
  `SQLITE_OPEN_NOFOLLOW` policy real).
- Service-health notices use canonical scoped HITL; `POST /hitl/{id}/respond`
  accepts source `service_health` with choice `dismiss`.
- **Plan approval from a notification.** `POST /hitl/{plan_id}/respond` with
  source `plan_approval` takes an explicit `task_id` (or `execution_id`) as
  authoritative; with neither (a phone notification action posts only the
  correlation id), the task is read from the pending `hitl.requested` the plan
  approval published, under the caller's own principal/workspace
  (`plan_approval_task_id_from_pending`). No pending plan approval in scope →
  `400 plan_approval_requires_task_id`.
- **One ask per re-parked confirmation.** When a resume or continue re-parks a
  confirmation pause, the approval record it creates no longer announces a
  second `approval` ask (`emit_approval_requested_event` publishes only the
  agent update); the pause's agentic `confirmation` is the one ask, and
  answering it reconciles the approval.
- **Preference alignment holds.** A hold is a `confirmation` pause with
  `pause_state.alignment_ask`. Respond accepts its typed `choice` and free
  `text`/`guidance` whatever `input_type` the client names. Its re-ask
  response and every re-announce (`emit_hitl_requested_for_restored_pause`)
  carry the generic prompt and the ask's public projection, never the
  pause's `question`: that is the owner-private reason, and the answer may
  have come from a shared surface. See
  `docs/components/magician/preference-alignment.md`.
- **Refused elevation pause.** When an elevation pause is refused at any of
  the three refusal sites, `finalize_after_refused_elevation` drops the
  handler's lifecycle exclusion first, then finalizes the orphaned run instead
  of leaving it in `WaitingUser`. The run's failure names the refusal
  (`REFUSED_ELEVATION_REASON`: the authorization did not match what this
  Magician staged; ask again to re-approve), not the genuine orphan's "lost
  across a restart" text.
- **Attention feed (`feed_api.rs`).** `/feed/attention` carries delegated
  child asks on the parent task, `waiting_without_question` escalations (once
  60 s old) and every taskless pending ask (`mcp_oauth`, `user_request`,
  `service_health`, `bot_auth` in requests; `approval` in approvals). A taskless
  row whose lifecycle resolved is dropped from both lanes, and one a task-bound
  row on the same page also carries is listed once, as the task's row. Shapes are in
  HITL surfacing fixes.
  The exact read `GET /feed/attention/{correlation_id}` (what a phone's
  notification extension fetches) titles a pending ask with its question and
  fills `metadata.correlation_id`/`source`/`input_type`/`input_schema`/
  `execution_id` and `task_id` from the pending `HitlRequested` where the V3
  card lacks them. A task-bound ask the lifecycle still holds pending but the
  V3 card does not carry yet (or only as an older ask of the same execution)
  is served from that pending event while its task exists, not 404. A resolved
  ask is 404.

## Routes

### Notes and memory

- `GET /notes/tree` (one level; empty `path` is root), `GET /notes/file?path=`
  (one Markdown note), `GET /notes/backlinks?path=`. `POST`/`PUT /notes/file`
  and the folder/delete routes write that folder and refresh the search index.
  The process watches each notes folder and reindexes after an outside edit is
  quiet for ~1 s. All stay inside the notes-search boundary.
- `POST /memory/search` runs `search_memory` recall for one agent in the scope
  (does not record a temperature use). The owner page stays unfiltered.
  An untrusted agent's own call applies the public-surface filter, and so does
  search during a shared call, including when the seated agent is trusted.
- `POST /hitl/{correlation_id}/respond` answers a `user_request` of type
  `alignment.wording_unlock` (the analyst asking to read preference wording)
  only from the owner's web, iOS or Android app (`403 private_surface_required`
  otherwise); an `approve` records a run-scoped wording grant
  ([analyst access](../magician/preference-alignment.md#analyst-access)).
- Taste proposal approve and reject also journal the decision in the
  owner's preference history (`taste_decided`) when the API data is present.
- `/preference-alignment/intentions` (view, add, edit, confirm, retire,
  forget) and `/preference-alignment/history` serve the owner's preferences
  and their history to the owner's web, iOS and Android apps only
  ([owner view and edits](../magician/preference-alignment.md#owner-view-and-edits)).
- `DELETE /memory/user-knowledge/{path}` refuses (`403
  owner_instruction_needs_the_owner`) a bot or plane bearer whose delete
  would remove an instruction the owner gave. It writes through
  `persist_user_knowledge_as(.., "memory_api")`, so a deleted preference is
  journaled in the owner's preference history (and its wording erased there)
  like any other knowledge write
  ([preference history](../magician/preference-alignment.md#preference-history)).
- `GET /memory/temperature/status` carries `tier_health`
  (`unearned_active_ratio`, `working_set_ratio`, `tier_lift`,
  `dead_entry_ratio`, key hygiene) from the overlay alone.
  `POST /memory/temperature/maintain` returns `compaction` when `resync_overlay`
  is set; it is the only route that may evict (candidates: `TierScope::AgentGoal`
  tiers via `discover_goal_ids_for_tier`). See [memory-evals.md](../magician/memory-evals.md).
- `POST /evidence/distill/{producer}` and `POST /screen/observations/distill`
  pass the scoped prompt manager (and screen tier) into the shared tier
  distiller; sampled background references recheck the exact cluster, tier and
  prompt before inference and export, dropping changed or deleted sources.
- Memory connections reuse feed, resurfacing and user-request endpoints; Today
  keeps individual insight IDs and detail links; the taste proposals envelope
  adds scoped `capture_health` / `capture_health_unavailable`
  ([memory connections](../magician/memory-connections.md)).
- `load_harness_supplemental_profile` resolves through
  `ArtifactV2Workspace::programs_supplemental_profile_path`, the same helper the
  learning lane writes through ([harness supplemental profile](../magician/harness-supplemental-profile.md)).

### Plane and harness engines

- `GET /plane/engines` reports `current` (process default), `chat_current`
  (`chat.harness_engine`), each engine's `native_tool_posture`, install status
  (PATH lookup, no spawns), the always-available `magician` pin, the pinned Pi
  harness when `pi` is on PATH (model list starts with `default`, leaving
  selection to Pi's isolated agent directory), and `run_pi_profile`. No auth: it
  names binaries and one config value.
- `PUT /plane/engine` persists `execution.harness_engine` + `harness_model`, and
  `pi_profile` (a chat-eligible profile validated against the router, or `null`
  for Pi's own settings) as `execution.pi_profile`. `PUT /plane/chat-engine`
  persists `chat.harness_engine` for any launchable roster name. Both pass the
  persisted engine to `set_harness_affinity(Some(engine))` unconditionally —
  `harness_affinity()` filters `""` and `magician`, so passing the built-in is
  how affinity is cleared. See [Plane](../magician/plane.md).
- Chat message POST and SSE POST accept optional `harness_engine` and
  `harness_model` beside `profile`. The engine must be installed (or
  `magician`); the route belongs to this turn, survives the pending-message
  queue, and never rewrites the server default. For Pi, `profile` resolves
  server-side to provider, model, endpoint and key variable; for other harnesses
  `harness_model` is the CLI pin (`default` lets the CLI choose). Web chat
  Settings store the choice in browser storage and send it per request; only
  `PUT /plane/chat-engine` changes the server default.
- **Plane MCP door** — `POST`/`GET`/`DELETE /plane/mcp`, loopback
  streamable-HTTP MCP. Auth accepts process-live or durable terminal `plt_`
  bearers. `initialize` advertises `tools.listChanged`, mints the session id
  `tools/call` requires, reads `params.capabilities.elicitation`, and fixes the
  session's parent engine for a terminal grant: the grant's engraved
  `harness_engine`, else the CLI family from `params.clientInfo.name`
  (`engine_family_from_client_name`), only when installed here. Every call of
  that session (and approved elicitation captures) runs with that parent engine;
  run and conversation grants keep Magician's choice. `DELETE` returns 204, ends
  the session and its terminal ledger, never the grant.
- **Durable terminal rehydration** resolves the engraved workspace's tool index
  through the shared scope catalog resolver (a cold terminal can initialize it),
  so terminals get canonical hot-tool schemas and can select deferred tools
  across requests. Missing wiring, failed resolution or an empty catalog returns
  `503 tool_catalog_unavailable` after credential validation. The grant gets its
  own runless executors narrowed to its allowlist
  (`ActionExecutors::for_runless_scope`), so scope-bound compiled tools dispatch.
  Expiry and revocation are checked every request.
- **Typed elicitation.** Capabilities bind to a freshly minted session (reusing
  an initialize ID never joins clients). Form-capable terminal calls carry
  `elicitation/create` prompts and one final result on the original POST SSE
  stream; answer POSTs return empty 202; GET notification streams are
  session-scoped. Approval requires `action: "accept"` and boolean
  `content.confirm: true`. `request_user_input` adapts text, choice,
  multi-choice and form contracts; `wait_for_run` relays questions only for runs
  this session launched. Execution pauses resume via
  `resume_agentic_execution_from_plane`, which verifies the exact displayed pause
  revision; the agent/goal/cycle selector is sent complete or not at all. The
  continuation runs on its own task, watched for 20 s so a refusal surfaces with
  its HTTP status, then polled under `timeout_secs`. Capability absence, bad
  answers, timeout, cancellation, revoked grants and cross-session answers never
  approve. See [plane typed input](../magician/plane.md#typed-input);
  `make test-plane-elicitation`.
- **Plane grants** — session-authenticated `GET/POST /plane/grants`,
  `DELETE /plane/grants/{id}`. A `plt_` grant cannot mint siblings. Mint returns
  the token once, validates the harness engine, and reports tools dropped by the
  `NEVER_ON_THE_PLANE` floor. Default concurrency 4.
- **Decision routing** — owner-authenticated `GET/PUT /plane/decision-routing`
  proxies Decision Engine model/operation settings, preserving validation
  failures and revision conflicts as `{code, message}` (the engine's stable
  code, or the status's own for an older engine).

### Run budgets

Contract: slash commands and run budgets;
runtime rules in [agentic budgets](../magician/execution/AGENTIC_EXECUTION_DESIGN.md#budgets).
All four routes are **session bearer only** (`plane_api::owner_identity`); errors
are JSON `{error, message}`. Handlers live in `budgets_api.rs`.

- `GET /budgets/defaults` → `{native_max_cost_usd, harness_max_cost_usd,
  native_env_override}`; each cap is a number or `"unlimited"`;
  `native_env_override` is true when `MAGICIAN_AGENTIC_MAX_COST_USD` is set (it
  then wins over the native value for runs).
- `PUT /budgets/defaults` takes a **partial** body (either or both caps; a
  missing field is unchanged). Values are positive finite numbers or
  `"unlimited"`; zero, negative, `NaN`, unknown fields and an empty body are
  `400 invalid_budget_defaults`. Edits only `execution.budgets` inside the live
  `magician-config.yaml` under the shared config-file lock (other lines and
  comments survive), verifies the file reads back, then publishes the new caps
  to running loops. Response: same shape as `GET`.
- `GET /tasks/{task_id}/budget` → the effective budget of the task's latest
  root execution: `{execution_id, cost_cap_usd (number | "unlimited"),
  cost_used_usd, tokens_limit (number | null), tokens_used, iterations_limit,
  iterations_used, duration_granted_secs, resumed, harness}`. Values come from the paused run's pause
  record, else the running loop's last snapshot, else its bounded durable
  `budget_usage.json` checkpoint, including the final settled pause charge.
  Attention preserves the current canonical budget dimension and exact ask ID
  when its execution carries the historical `max_iterations_reached` status.
  The checkpoint is display data and grants no
  execution authority; resumed iteration counters include earlier segments.
  Historical runs without a checkpoint retain the engine-pin-only fallback
  (zeros, `tokens_limit: null`).
- `POST /tasks/{task_id}/budget` with one of `{add_cost_usd}`, `{add_tokens}`,
  `{add_iterations}`, `{add_duration_secs}`, `{unlimited: true}` (several may be combined; at least one
  must add something; non-positive / non-finite amounts are `400
  invalid_budget_grant`). Appends a grant to the execution's
  `budget_grants.json` (`409 run_finished` for a completed/failed/cancelled run,
  `404 task_has_no_run` when nothing has launched). A running run picks it up at
  its next iteration check; duration grants add active time at the next resume,
  once per recorded grant, without resetting cost or token usage. When the run
  is paused on a **budget** pause, the
  grant also continues it through the HITL confirmation path (the same
  `Confirmation {confirmed: true}` a person gives); `resumed: true` reports that
  the resume was admitted. Responds with the effective budget and emits the
  realtime event `task.budget.updated` (`AgentEvent` envelope, `agent_id:
  "budget"`, payload `{task_id, execution_id, budget}`) so clients refresh.
- Engines stay on `GET /plane/engines`, `PUT /plane/chat-engine`,
  `PUT /plane/engine`; `PUT /plane/engine` re-renders the whole `execution`
  block, `budgets` included. The owner-identity refusals of these routes now
  carry a human `message` beside `error`.

### LLM routing

Session-authenticated `GET /llm/routing`, `PUT/DELETE /llm/routing/{operation}`
(per-operation profile override), and `PUT/DELETE /llm/routing/{operation}/engine`
(`{"engine": "parent" | "pinned"}`; 400 otherwise, 404 unmapped; DELETE reverts
to the config selector).

- While preference alignment enforces, a profile override of
  `action_alignment_judge` that names an undeclared profile, or a clear that
  would fall back to an unbound selector, is refused with 409
  `{code: "alignment_enforce_route_required", property, message}`
  (`config::preference_alignment_route_check`), the code a config load gives.

- GET mirrors the router's staleness rule (an override naming a removed profile
  reports the default plus `stale_override`) and the flow-scoped rule:
  `affinity_scope: "flow"`, `driving_engines: {chat, run}` (`magician` is
  `null`), `engine_pins`, and per operation `engine`, `engine_source`,
  `follows_parent`, `local_floor`, `parent_profiles: {chat, run}`,
  `routing_source` (`override` | `parent` | `config`). It lists `op-harness-pi`
  as a harness profile with Pi install state. The overview is a pure function
  (`routing_overview`) of router config and `RoutingState`.
- The parent engine is the flow's, never the agent's or client's:
  `OperationRoutingOverrides` built from an agent definition carry no
  `parent_engine`, and `POST /api/magician/v3/tasks/{id}/execute`,
  `POST /api/magician/v3/monitors/{task_id}/run` and the v2 direct-execution
  routes strip it before sealing; launch admission drops it again.
- See [model-routing-panel.md](../unified-ui/model-routing-panel.md) and
  [llm-routing-overrides.md](../magician/llm-routing-overrides.md).

### Agents

`PUT /agents/{id}` (YAML) and `PATCH /agents/{id}` (merge patch) share
`update_agent_definition_with_store`, which after the trust-policy check runs
`validate_definition_llm_routing`: any newly introduced `llm_routing` profile the
router does not define or `coding_profile` not in `coding.profiles[].id` is
`422 unknown_pinned_profile` (`details.unknown`). Existing stale pins never block
unrelated edits; an unreadable config skips the check. Success writes
atomically, updates the runtime's in-memory definition (`upsert_definition`) and
invalidates the agent-list cache, so pins apply to the next run without restart.

### Components, skills and setup

- `GET /components/catalog` returns the compiled component graph, host
  description, fresh server-side observations and status (probes run beside
  Magician so remote Desktop clients get server truth). `POST /components/plan`
  takes capability ids and returns the ordered setup plan and projected report;
  unknown ids fail closed.
- Setup-token-gated: `POST /components/{id}/install` (graph component ids only;
  serialized jobs; target/script from the graph),
  `GET /components/installations/{job_id}` (bounded progress + authoritative
  post-install probe), `GET /components/admin-access` (side-effect-free token
  check), `PUT /components/{id}/configuration-file` (only for components whose
  setup descriptor declares that file primitive; catalog-owned size,
  destination and validator; response reports configured state only).
- `GET /skills/catalog` enumerates Skillshub, extra, scope-only and compiled
  skills with version, installability, setup guidance, the typed auth shape
  (kind, requirement, provider, credential names, lifecycle availability), the
  declarative setup driver from `metadata.magician.setup` (never inferred from a
  provider name), and the request's current scope. No credential values.
  Folders the loader refuses — including a governed runtime package whose
  frontmatter parses but whose typed actions or runtime contract do not
  compile — are rows with `kind: "invalid"` and the reason in
  `invalid`; `copied_scopes` lists scopes holding a skillshub skill as a plain
  copy rather than the linked layout.
- Setup-token-gated `POST /skills/install` links `skillshub:<name>` into the
  scope (live, repaired at boot) and copies `path:<abs>`;
  `POST /skills/{name}/uninstall` marks the skill removed so boot-time scope
  repair does not reinstall it (purge also deletes `.skill-state/<name>`).
- Setup-token-gated `POST /skills/catalog/{name}/oauth/start` and
  `GET .../oauth/status` manage governed remote-MCP authorization with the same
  resource/issuer/profile binding and encrypted vault as MCP execution; start
  returns the authorization URL only to the native setup caller. A configured
  `mobile_access.public_origin` is the callback origin for a remote engine,
  else loopback.
- Setup-token-gated per-skill (`/skills/catalog/{name}/env`) and per-bot env
  endpoints return key names and set/unset state and accept write-only updates.
  Bot auth adds a private interactive-input endpoint (Telegram 2FA); the QR
  endpoint serves WhatsApp's PNG or renders Telegram's `tg://` login URL as SVG.
  Raw QR payloads and 2FA values are never logged or returned.
- Localhost-only `POST /secrets/app-data-root-key/rotate` requires the setup
  token plus a verified scoped identity and returns only the successor key ID.
- Secret approvals (`secret_vault_api.rs`, `SecretApprovalsResponse`) serialize
  MagicVault's `PendingSecretApproval` with `domains` beside legacy `domain`
  (a host set, or `["*"]` for all sites; omitted when empty).

### Auth and enrollment

- `DELETE /auth/workspaces/{id}[?purge=true]` (`auth_api::delete_workspace_handler`):
  after the registry row is gone it cancels the scope's wakes
  (`WakeUpQueue::cancel_scope`), evicts the agent service's per-scope caches
  and cancels running goal pipelines (`AgentApiServices::evict_scope`, reached
  through the `MagicianV2Api` app data), revokes sessions, API tokens and `plt_`
  grants engraved for the workspace, revokes its bot tokens, unpairs its
  devices (`DevicePairingStore::unpair_scope`) and removes its push
  registrations, then retires attention-learning and resurfacing rows. Each
  step is best effort and logged; none can fail the delete. The harness wake
  dispatcher consumes a deleted workspace's wake before resolving the scope,
  and `resolve_scheduler` / `resolve_approval_service` refuse it, so nothing on
  this path can re-create the directory.
- `POST /auth/login` (bearer session; first login on an empty install bootstraps
  the owner under `allow_signup`), `POST /auth/logout`, `GET /auth/session`,
  workspace CRUD, `GET /auth/social/{google|github}/start` →
  `GET /auth/callback/{provider}`, `POST /auth/link/{provider}/start`,
  `POST/GET /auth/tokens`, `DELETE /auth/tokens/{id}`, `POST/GET /auth/grants`,
  `DELETE /auth/grants/{id}` (session-only minting, `NEVER_ON_THE_PLANE`-floored
  allowlist, `ttl_hours` 1..=2160), `GET /auth/admin/orphaned-scopes` (behind
  `MAGICIAN_ADMIN_SECRET`), `POST /auth/identities` (owner provisions members).
- In `auth.mode: open`, a request without a bearer gets the anonymous scope
  **only while no identity exists**. Password doors are throttled (429 +
  Retry-After). Provider secrets resolve via
  `AuthProviderConfig::effective_secret(provider)` with no cross-provider
  fallback. Password login writes a credential-free audit event; the bearer gate
  records a bounded reason when `/auth/session` rejects verification.
- Bot tokens (`mag_bot_`, injected as `MAGICIAN_BEARER_TOKEN`): `GET
  /auth/session` answers with the engraved scope (`identity: null`,
  `method: "bot_token"`, `bot`, empty `workspaces`); session-only doors refuse
  with `session_required`, logout with `not_a_session`. See
  [auth.md](../magician/auth.md#bot-tokens).
- `POST /chat/enroll`, `/enroll/approve` (admin secret), `/enroll/revoke`,
  `/enroll/cancel`. `GET /chat/invoke-grammar` publishes the tutor, app_copilot,
  brainstorm and vibedev invoke grammar as one versioned, sha256-etagged catalog.

### Devices, mobile and Edge

- `GET /devices` includes `pairing_available` (an empty list does not imply the
  pairing owner is ready; no keyring diagnostics exposed) and server-owned
  `connection_options`: `same_wifi` (private-LAN origin when reachable, read
  from the current default interface on each request),
  `remote` (this machine's HTTPS origin, `https://<machine>.<zone>` with one
  tunnel per machine) and `machine_name` (`MAGICIAN_MACHINE_NAME`, so the
  panel can say which machine a QR pairs with).
- The enrollment exchange also returns `machine_name` and `machine_origins`
  (this machine's public origin and current same-Wi-Fi origin). The server keeps
  one pairing per device, so pairing the same machine by its other route
  replaces the earlier pairing; apps drop their stale profile for any origin in
  `machine_origins` or with the same `machine_name`.
- `GET /remote-machines` serves the owner's machine registry,
  `$MAGICIAN_ROOT_DIR/remote-machines.yaml` (written by
  `scripts/ensure-machine-tunnel.sh`; sample
  `magician_data_v3/remote-machines.example.yaml`): `{self, machines: [{name,
  api_origin, ui_origin, is_self}]}`. Origins only, never webhook origins or
  secrets. `self` is the entry whose `api_origin` is this server's public
  origin, else `MAGICIAN_MACHINE_NAME`. The file is re-read on mtime change;
  an invalid entry (name not a DNS label, non-https or path-bearing origin,
  duplicate) is dropped and logged, and at most 32 machines are served. It
  is a separate file because `magician-config.yaml` rejects unknown keys.
  Enrollment accepts only the matching `connection_mode`; a browser cannot submit
  an address. The origin is retained on the one-time ticket and returned by
  exchange. Pairing begin distinguishes an unavailable signing owner (503) from a
  missing/consumed exchange (404). ESP pairing requires verified local or
  Cloudflare evidence.
- Mobile connection and Android Apps enrollment payloads use only `magican://`.
  Hardware-key Apps attestation requires package `ai.magicbeans.magican`.
  - A Play build pins signer and version codes in `mobile_access`
    (`android_apps_signing_sha256`, `android_apps_version_codes`); a pinned policy
    checks the version code at enrollment.
  - A private/self-hosted build without those keys pins at owner approval: the
    chain is verified to the reviewed roots (config's, or Google's two published
    roots), with every hardware-enforced claim and the proof; the approval shows
    package, version code and signing digest, and the identity records the policy
    pinned to that build. Reconnect resolves it from the identity
    (`reviewed_policy_for_identity`), so other or later builds fail closed.
    `GET /devices/apps-automation/trust-options` reports the private build ready
    when only the address is set.
  - Refusals log content-free reasons: `[ANDROID-ATTESTATION] rejected reason=…`
    (`pins_mismatch`, `claims_mismatch`, `root_not_reviewed`, `chain_order`,
    `chain_path`, `leaf_key_curve`, `software_enforced_key`,
    `hardware_authorization`, `attestation_application_id`),
    `Apps enrollment rejected` (error class, trust mode, declared package/version,
    chain length), and `refused at stage stage=…` (`bounds`, `spki`,
    `signature`, `chain_decode`, `chain_x509`, `chain_validate`,
    `attestation_extension`, `key_description`) for `Malformed`.
- `GET /edge/bridge` — outbound desktop-capability socket. Accepts only a paired
  `desktop` credential carrying `edge_client`, binds to the roster-owned
  principal/workspace/device, and requires a versioned Edge hello as the first
  text frame. Binary, continuation, malformed, stale-generation, oversized and
  post-lease traffic closes it; pairing rotation or unpairing revokes it.
- `GET /edge/devices` lists live desktop ids and capability descriptors;
  `POST /edge/devices/{device_id}/invoke` addresses one. Paired-device
  credentials cannot call either. The server derives capability generation and
  byte ceilings from the session, mints a short-lived grant, and returns
  succeeded/failed/unavailable/cancelled. The request names execution id/epoch,
  idempotency key, capability/operation, bounded payload and a 100–120000 ms
  deadline — never a local URL or credential.
- **Browser Web Push** (`web_push_api.rs`): `GET /web-push/config` →
  `{enabled, vapid_public_key}`; `PUT /web-push/subscription`
  `{subscription:{endpoint, keys:{p256dh, auth}}, label?}` → `{id}`
  (idempotent per endpoint); `DELETE /web-push/subscription` `{endpoint}` →
  204. Scope is the request's own headers; the body never names one. Only an
  https endpoint on a browser push service is stored (400 otherwise, so the
  runtime never POSTs to an arbitrary URL); 503 `web_push_not_configured`
  without VAPID keys. Rows live in `MobilePushStore` as platform `web`, so
  attention pushes, withdrawals and the Settings test reach browsers through
  the same dispatcher. Shapes: [v2-api-guide.md](../magician/v2-api-guide.md#browser-web-push).

### Chat, voice and screen

- Chat SSE sends keepalive comments every 15 s while tools or human responses are
  pending. Disconnect cancels by default; `continue_on_disconnect: true` lets an
  authenticated mobile stream keep its accepted turn running for recovery. See
  [Chat SSE streaming](../magician/chat-sse-streaming.md).
- Chat and contextual-writing usage expose nullable USD costs and optional bucket
  availability, so unpriced harness turns stay unknown.
- `GET /chat/sessions/{sid}/turns/{tid}/events` returns only the newest ~1 MiB of
  rows, so it also returns `usage`, computed over the whole turn file up to the
  byte the page ended at (streamed off the reactor; only rows naming
  `LLMResponseReceived` / `llm.succeeded` are parsed): `{calls, input_tokens,
  output_tokens, cost_usd: number|null, cache_read_tokens: number|null,
  cache_creation_tokens: number|null}`. Only the turn's OWN calls count — rows
  whose `execution_id` (payload, else `correlation`) is absent or the turn id;
  a spawned/delegated task's rows are excluded (clients add task usage
  separately). One call per distinct `llm_call_id` (payload, else
  `correlation`) for tokens and cost (the chat call's `llm.succeeded` +
  `LLMResponseReceived` pair counts once); id-less rows count individually;
  `usage_availability: false` fields are unreported. A missing file returns
  zero calls; `usage` is absent only if the scan itself failed.
- `PUT /media/preferences` accepts `realtime_voices` (per-profile speakable
  voice), loaded by the control socket at connect.
- Backend-proxied voice injects `delegate_to_chat.chunk` text into the upstream
  Live/Gemini session so Magician's result is heard. GPT Live 1
  `session.instructions` is `voice_live_mouth_system`; the Magician-brain catalog
  snapshot is still taken so `delegate_to_chat` can authorize, but Live gets no
  Realtime tool list.
- Realtime-call accounting on the voice control socket is spawned on the
  runtime, never the actor (a socket closed right after `session.end` would
  cancel it and drop usage). `session.end` asks the provider to close and waits
  `PROVIDER_CLOSING_REPORT_GRACE`, because for a duration-billed provider the
  closing report is the whole bill. A speech start counts as cancellation only
  when assistant audio was still playing.
- Owner media registration creates a process-only, single-move credential behind
  a server-random correlation. Only direct personal-assistant voice on an
  identified owner surface with an attested backend-proxied,
  no-fallback/no-provider-storage profile receives governed app tools or
  credential-bound `local_only` context; reconnect, rotation, expiry,
  substitution, meetings, hands-free, P2P and unknown owners fail closed. On
  backend-proxied calls, user transcripts, function calls and catalog acks are
  accepted only from the server-owned provider channel. `session.ready`, first
  upstream PCM and PTT-release audio size are logged. Initial resume compaction
  is capped at 2 s.
- A silent `user.text` seed (`inject: true` without `request_response`) stays in the live session. It is not written to the chat thread. A `user.text` that requests a response still is.
- A backend-proxied `ptt.release` with less than ~100 ms of routed PCM
  (`ptt_release_lacks_provider_audio`) clears the provider input and emits
  `transcript.user.ignored` (`reason: "no_audio"`) instead of commit +
  `response.create`, which made the model reply with no user turn.
- Gemini Live and backend-proxied GPT Realtime transcriptions are forwarded as
  `transcript.user.partial` and cumulative `transcript.assistant.delta` before
  the turn completes. Gemini 3.8 Live Extended Thinking's `interactionStatus` is
  forwarded as `interaction.status` (`in_progress` | `idle`) after
  `response.done`, because the model may still be running a non-blocking tool;
  clients map it to an `assistantWorking` flag.
- Composer queue (`chat_api/queue_actions.rs`): `POST /chat/sessions/{id}/queue`
  enqueues a `SendMessageRequest` without disturbing the running stream (202
  `{queued, session_id}`); plan-mode/plan-addressed bodies get 400, a
  client-selected protected `source_surface` 403, and an inactive or voice
  coordinator session 409. `POST /chat/sessions/{id}/queue/{message_id}/action`
  takes `{"action":"parallel"}` (200 `{request}`) or `{"action":"stop_and_send"}`
  (200 `{ok:true}`). `GET`/`DELETE …/queue` and `DELETE …/queue/{message_id}`
  list, clear (`{cleared}`) and delete (`{deleted}`). The voice socket reports
  concurrent requests as `voice.request.accepted` / `voice.request.cancelled`
  (`{count}`) / `voice.request.failed` (`{message}`). The 2026-10-01 edits to
  these handlers, `plane_api.rs`, `transcript_claims_api.rs`,
  `voice_control_handler.rs` and `websocket_handler.rs` are rustfmt-only; the
  contract is unchanged.
- Screen capture accepts optional display-only `source_app` /
  `source_window_title` (sanitized, ≤ 200 chars). The clip toggle records via
  CuaDriver `start_recording` / `stop_recording` on one held-open
  `cua-driver mcp` session (the daemon ends a recording when its client
  disconnects) and stages the mp4 in `last_video_path`. Describe inlines the
  front window tree (`get_window_state`, `max_elements` 400) and logs driver
  `{code, suggestion}` refusals. See [screen-capture-and-ask.md](../magician/screen-capture-and-ask.md).
- `GET /contextual-writing/catalog` mirrors the desktop catalog flag-for-flag,
  including screenshot policy ([contextual-writing.md](../magician/contextual-writing.md)).

### Settings

- `GET /settings/privacy` reports effective `privacy.processing.mode` and the
  routing it produces; `PUT` dry-runs full config validation (a `mode: cloud`
  that would not load is 400) then reloads.
- `GET /settings/local-generation` reports the kitty (`qwen3.8-ud2-mtp` /
  `gemma4:12b` / `woof-4b`), host RAM, the recommendation, Ollama install status
  and RAM-tier warnings. `PUT {selected, reload_ollama?}` writes
  `selected: &local_generation_model <id>` surgically (as `make setup-ollama`
  does), reloads config and by default runs `scripts/run-ollama.sh`. Out-of-rule
  models still pin (`rule_violated`); missing models are not auto-pulled.
  Desktop onboarding treats these two endpoints as the authority for locality and
  model choice.

### Preference alignment (`preference_alignment_api.rs`)

All three need the owner's principal/workspace scope.

- `GET /hitl/{correlation_id}/alignment?surface=web|ios` is the owner-private
  detail of one hold.
  - It returns the reason, the held act, the cited preferences, the choices
    with their kinds and, on a wording stage, the exact readback and its
    token.
  - Only `web` and `ios` get the detail, as the v1 canary's private
    surfaces. Any other or missing `surface` gets the public projection.
  - It reads a snapshot of the live pause
    (`FullPauseStore::alignment_ask_snapshot`) and claims nothing.
  - Another owner's pause, or none, is `404 alignment_ask_not_found`. An ask
    suffix that is not the current ask is `409 superseded_ask`.
- `GET`/`PUT /preference-alignment/policy` is the owner's intervention
  policy.
  - `PUT {expected_revision, global, intentions, replace_unreadable?, surface?}`
    replaces the whole policy.
  - A stale revision is `409 stale_policy` with the current policy.
  - An unreadable file is `409 policy_unreadable` until it is written with
    `replace_unreadable: true`.
- `GET /preference-alignment/notices?surface=web|ios` lists the owner's notices: nudges that let an
  act through, with their remember offers, and holds that expired unanswered. A non-private surface
  gets only the open count. `POST /preference-alignment/notices/{id}/answer` takes
  `{answer: "remember", token}` or `{answer: "dismiss"}`.
  - A remember needs the notice's token and writes through the revision-checked lasting write.
  - A wrong token is `409 wording_not_confirmed`; an answered notice is `409 already_resolved`.
- **Answer policy on respond.** For an alignment hold owned by the caller's scope, the respond handler
  applies `alignment_answer_policy` before the resume. It decides on what the reply resolves to (a
  typed label counts as the choice it names). The channel comes from `effective_answer_channel`: a
  bot bearer is `bot` and a plane grant is `plane`, whatever they claim. It is stamped on
  `AgenticResumeRequest.answer_channel`, and the resume enforces the same rule on every path:
  - a `voice` answer must name its ask (`409 alignment_ask_identity_required`);
  - consent and lasting changes are taken only from `web`/`ios`/`android`/`desktop`
    (`403 private_surface_required`).
- **Expiry sweep.** `expire_alignment_holds` runs every 60 s as the `alignment_hold_expiry_loop`
  supervisor task.
- **Voice answers.** `ApiVoiceReviewSink` (installed at startup) applies a spoken answer from a live
  call. It runs the same answer policy under channel `voice` with the full correlation id, then the
  same admitted-then-detached resume as the respond route. `voice_control_handler.rs` announces a
  hold (`alignment.review_requested`) with the generic notice and answers only an utterance that is
  exactly one of the review's choices (cancel, retry, use_alternative, keep_as_is). Spoken approval
  is pointed to the phone or the web.
- `GET /preference-alignment/status` returns the mode in force
  (`effective`), the saved file's dry-run verdict (`saved`) and `in_sync`.
  - The dry run is `config::dry_run_magician_config_from_path`; it installs
    nothing.
  - A rejected file reports `{code, property, reason}`, for example
    `alignment_enforce_route_required` with
    `llm.router.operation_mapping.action_alignment_judge`.

### Coding

`GET /coding/profiles` projects Pi, Codex, Grok, Claude Code and Antigravity.
Observation is filesystem plus cached receipt/overlay; GET never spawns a CLI.
`POST /coding/engines/{grok_acp,claude_code,agy_cli}/refresh` return 202
`checking`, coalesced 2 s. `POST /vibedev/runs/{task_id}/checkpoints/{id}/revert`
queues an engine-tagged pending resume that `run_coding_task` binds only when the
engine matches.

### App authoring catalog

`GET /apps/authoring/{tools,tools/{name},agents,personalities,procedures}` run bounded,
live filesystem discovery on the blocking pool (`off_worker`). Pure skill
classification and tool-descriptor validation reuse exact-content caches;
unchanged sources no longer repeat YAML parsing for each endpoint. The server
prewarms registered default-workspace catalogs alongside app/history startup,
before HTTP readiness, even when system package admission is disabled. Each
request still binds its authenticated scope and resolves current roots, source
directories and name collisions. Changed or removed files take effect on the
next request. Responses remain `private, no-store`; cached validation grants no
execution or publication authority. See [authoring discovery](../magician/app-authoring-cli.md).

### VibeDev run list paging

`GET /vibedev/projects/{id}/runs?limit=&cursor=&offset=&query=` returns one page of the project's runs, newest
first (`limit` default 10, max 50): `tasks` lists each root run followed by its threaded follow-up
turns (`vibedev-threaded` with its `Parent task:` present), and `pagination` carries `runs`,
`total_runs`, `offset`, `has_more` and `next_cursor` (pass it back as `cursor` for older runs).
`offset` gives numbered pages (ignored with a `cursor`); `query` keeps the runs whose root or any
folded turn has it in its title or description (case-insensitive), and `total_runs` then counts
only those (`page_runs_matching`). `links` maps the page's task ids to their typed run links
(`parent_task_id`, `crew_id`, `lane`) when recorded. `POST /vibedev/runs` takes optional
`crew_id` (letters, digits, `-_.`, up to 64) and `lane` (one line, up to 80, needs a crew)
to start a run as a crew lane; anything else is 400 `vibedev_invalid_crew`. It reads only
the project's own tasks (`run_task_ids` + `active_root_task_id`,
`ArtifactV2Service::list_task_items_by_ids_archive_filtered`), never every internal task, and drops tasks whose
`VibeDev project:` line names another project (`vibedev::run_pages`).

`GET /work/live` is "Magician at work" (`magician::magician_v2::work_live`): the
owner's coding runs, crews and Work tasks as ranked items (`featured`, `next`,
`counts`, `items`, `followed`); `POST /work/follow` `{item_id}` follows one
(at most 3, 409 `follow_limit`) and `DELETE /work/follow/{item_id}` stops.
`POST /v3/tasks` takes an optional `source` (e.g. `ios_siri_intent`), recorded
with who started the task in the task folder's `origin.json`.

`GET /vibedev/projects/{id}/crews` lists the project's crews (newest first); `?state=open` returns
the crews still in play, `?state=settled&offset=&limit=` a page of the rest (most recently changed
first, 5 by default, at most 50), and every answer carries `total`, `settled_total` and `has_more`
(`crews::page_crews`); `POST` with
`{goal, lanes?, budget_usd?, max_lanes?, start?}` proposes one on a trusted project (400
`vibedev_crew_invalid` otherwise): the given lanes (`lane`, `brief`, `owns`, `coding_profile_id`) or the
planner's split, then either starts it or asks the owner in Attention. `POST …/crews/{crew}/start`
starts any lane without a run; `POST …/crews/{crew}/stop` stops running lanes and cancels the crew;
`POST …/crews/{crew}/integrate` merges the lanes again after a conflict; `POST
…/crews/{crew}/resolve` starts an integrator lane for a conflict (400 once three have run or the
crew's budget is spent); `POST …/crews/{crew}/choose`
`{lane_index}` keeps one finished race attempt (`null` keeps none); the create body also takes
`mode` (`split`|`race`), `racers`, `profiles` and `review`; `POST
…/crews/{crew}/pull-request` (owner terminal only, like the other landing actions) opens a pull
request for the crew's merged branch; it is the only way to open one, since an Attention answer to
the crew's pull-request question only records the owner's intent. A crew reports `spent_usd`, `branch`, `conflict` and
`pull_request_url` (`vibedev::crews`).

Mobile run detail (`vibedev::run_detail`, shapes pinned by `magician/tests/fixtures/vibedev/*.json`,
which the iOS and Android tests decode): `GET /vibedev/runs/{id}/turns` returns a run's root and
follow-ups from any of its task ids (request text from the prompt fence, mode, engine, status, cost,
summary, and for trusted runs the turn's commit and changed-file count); `GET
/vibedev/runs/{turn}/timeline?limit=` folds that turn's durable `coding.*` log into compact cards
(`message`, `reasoning`, `action` with `running|ok|error`, `error`, `completed`, `notice`), newest
`limit` (default 200, max 400); `GET /vibedev/runs/{id}/changes?turn=&path=` lists the files the
run or one turn changed with counts, and `path`'s patch when asked (trusted: the branch range;
sandboxed: the turns' stored proposals, newest file version wins); `GET /vibedev/repos/suggest`
lists repositories a new project could use (existing projects', then git repos in the workspace
home).

`GET /vibedev/runs/live?recent_minutes=` lists, across every open project, the runs that are going,
waiting on the owner, or ended within the window (default 30 minutes, at most a day), one `state`
each: `needs_you` (a pending user request names a turn, a diff waits for review, or a live turn has a
pending question), `working`, `paused`, `failed`, `stopped` or `done`. Rows come needs-you first, then
going, then ended; `live_task_id` is the id `/vibedev/runs/{id}/control` takes, `crew_id` and `lane`
name a crew lane, `cost_usd` is the root and live turn's metered coding spend,
`approval_id` (a run waiting on a diff approval) is an unclaimed proposal id of the turn that waits,
which the HITL respond call answers, `started_at` is the root's creation time, and `counts` totals
the states. State is derived from the stored tasks and pending requests on every read
(`vibedev::live_runs`), never from events. A read covers each project's newest 150 recorded runs,
its active run, its open crews' lanes and its runs a pending request names, plus the turns they
continue.

### Archived rows in list endpoints

Every list of an archivable thing takes `archived=exclude|include|only`
(`magician_v2::archived_filter`); an unknown value is `400 invalid_archived_filter`. Each list keeps
its old default: chat sessions (`GET /chat/sessions`), chat threads (`GET /ui-threads`) and VibeDev
projects (`GET /vibedev/projects`) default to `include`; tasks (`GET /v3/tasks`), internal tasks
(`GET /v3/tasks/internal`) and VibeDev runs (`GET /vibedev/projects/{id}/runs`) default to `exclude`.
On the task lists `status=archived` alone implies `only`. A non-default value pages chat sessions
and threads, and gives the task lists the paginated body. `GET /vibedev/projects` also takes
`limit` (default 50, max 200) and `offset`; with any of the three it adds `pagination`
(`total`, `limit`, `offset`, `has_more`).

### VibeDev project checks and build caches

`PATCH /vibedev/projects/{id}` also takes `checks` — a list of `{kind, run, timeout_secs?}` (shell
commands run from the project root; an empty list means no checks) or `null` to return to
detection — and `build_caches`, a list of `{env, path?}` toolchain cache variables (absolute path,
or blank for a Magician-managed folder). Both are owner-only; invalid lists are refused with
`invalid_checks` / `invalid_build_caches`. `GET /vibedev/projects/{id}/info` returns the resolved
`checks` (with `timeout_secs`), `checks_source` (`project` | `repo_file` | `task_runner` | `preset` |
`none`) and `checks_source_detail`, the stored `configured_checks` and `build_caches`, and
`build_env` (every cache variable resolved to a path). `POST /vibedev/projects/{id}/check` and the
`run_project_checks` tool run the resolved checks; trusted checks run with `build_env` and empty a
cache below `coding.build_dir_min_free_gb` first. Resolution order and presets:
[verification controller](../magician/verification-controller.md#policy).

The same PATCH takes `sandbox_write_paths` — absolute folders a coding run may also write beyond
its allow-list (owner-only; system folders, the home folder and credential folders are refused
with `invalid_sandbox_write_paths`). The info endpoint returns the list. Approving a
`vibedev.sandbox_write_approval` question adds its folders here
([coding-engine isolation](../magician/coding-engine-isolation.md)). Setting the list marks every
earlier allowed answer as applied, so a folder removed here is not added back from history.

### VibeDev trusted repo mode

`PATCH /vibedev/projects/{id}` accepts `trust` (`sandboxed` | `trusted`) and `base_branch` (empty
clears). Both are owner-only (the Workbench terminal rule); `trusted` is refused with
`vibedev_project_not_trustable` unless the project path is a git repository root. The project
response carries `trust` (omitted while `sandboxed`) and `base_branch`. Runs of a trusted project
report `trusted_branch` in the coding result and `coding.branch_updated` events. See
[coding-engine isolation](../magician/coding-engine-isolation.md#trusted-repo-mode-no-shadow).

Review and landing, keyed by the chain's **root** task id:

| Route | Who | Does |
|---|---|---|
| `GET /vibedev/runs/{id}/disk` | scope | `{workspace_bytes, build_dir_bytes, free_bytes}` — the clone, the shared build dir, free disk; cached 5 min per chain |
| `GET /vibedev/runs/{id}/branch` | scope | `BranchSummary`: branch, base, head, commits (newest first — the chain's checkpoints), per-file diff of `base..HEAD` (a file over 256 KB reports counts only), `behind_base`, `dirty`, `merged`, `pull_request_url` |
| `POST /vibedev/runs/{id}/merge` | owner | Fast-forward, else a `merge-tree`/`commit-tree` merge commit; refuses conflicts, a checked-out base with tracked changes, an empty branch, and a branch whose diff trips the G19 secret scan. Checked-out base → `git merge --ff-only` (working tree follows); otherwise a compare-and-swap `update-ref`. Records `merged` |
| `POST /vibedev/runs/{id}/pull-request` | owner | `{title, body}` → push the branch from the clone to `origin`, `gh pr create --head <branch> [--base <base>]`, with Magician's runtime `.env` keys (its `GITHUB_TOKEN` included) removed so `git`/`gh` use the owner's own credentials. Same secret scan. Idempotent once a URL is recorded |
| `POST /vibedev/runs/{id}/rewind` | owner | `{commit}` between base and head → `git reset --keep` in the clone and a forced republish of the branch; queues the matching turn's engine session as a pending resume under the chain root (`session_resume_queued`, `resume_engine`) |
| `POST /vibedev/runs/{id}/discard` | owner | Delete the clone and record and (unless `delete_branch: false`) the branch in the owner's repo; refused while the owner has that branch checked out |

The landing actions take the chain's admission lock without waiting and answer `409 vibedev_run_busy`
while a turn is running. Refusals are `400 vibedev_trusted_action_refused` with a `message`.

### Interactive sessions (Workbench terminals)

`/interactive-sessions` (start, list, stdin, `{id}/resize`, buffer, diff,
close), `/interactive-sessions/cli-runtimes`, `/filesystem/directories` and
`POST /filesystem/worktrees` (create a git worktree beside the repo) are
**owner-only** (`require_terminal_operator`: the first login identity by
session or API token; bots, `plt_` grants and members get `403
terminal_owner_only`). `start` refuses programs outside
`interactive_process.operator_cli_programs`, reads the per-program cap from the
live config, removes Magician's `.env` keys and Makefile build knobs from the
child, and hosts the program in a private tmux server when
`interactive_process.session_host` allows. `list` re-attaches a restarted
process's tmux sessions under their old ids and reports `persistent`. See
[Developer Mode Workbench](../magician/developer-mode-workbench.md).

### Tasks, recipes and channel assist

- `POST /api/magician/v2/executions` with `skip_planning=false` creates a V3
  task plan (`create_execution_via_task_planning`) and returns 202
  `{execution_id: "planexec_…", execution, initial_message_enqueued,
  skip_planning: false, task_id, plan, planning_execution_id}`. Questions,
  approval and execution then follow the task plan. Engagement-scoped
  requests with planning return `engagement_planning_unsupported`.
- `POST /api/magician/v2/executions/{id}/message` is direct-only: with
  `skip_planning` false or omitted it returns `400 legacy_planning_retired`
  (the legacy V2 planning entry is gone); with `skip_planning: true` it
  accepts a direct run (`202`).
- Recurring App task details expose schedule state and cursor-paginated history
  (25 default, ≤ 100). Recurring behaviors keep one task with per-occurrence run
  references; historical reads and cancellation select the exact root execution.
  `DELETE /apps/maintenance/action-runs/{run_ref}` removes only terminal legacy
  scheduled task shells after resource settlement. See
  [Recurring App tasks](../magician/recurring-app-tasks.md).
- Task list projections classify canonical app runs as internal; generic
  deletion is denied (results and receipts belong to the Apps lifecycle).
- Channel-assist `stats.distill.queue` and `sync/status.distill_queue` expose
  the history cutoff and disjoint eligible, history-excluded, expired and
  retry-exhausted counts; `by_state` covers retained rows. Reads never retire
  work or call a model ([Distillation](../magician/mail-assist.md#distillation)).
- Task Recipe metadata includes `version`; dashboard replay sends
  `expected_version`, and a newer compiled recipe returns 409 before any request.
  Running recipes recheck the mining switch and write authority before later
  requests, including retries. Scope mismatches are rejected. After replay, a
  statistics-save failure or mining disable keeps the result with
  `state_persistence_warning`. An uncertain write returns 409 with
  `effect_uncertain=true`, `retryable=false`, `browser_retry_allowed=false`.
  A held recipe lock returns 409 `recipe_busy` without dispatch (never queued
  behind another run's approval wait).

#### API mining and Task Recipes

Mounted under `/api/magician/v2/api-mining`, resolving the principal/workspace
into one scoped store. Payloads carry request shapes and non-secret auth status
only; captured cookies, tokens and headers stay in the encrypted SecretStore.

| Method + path | Contract |
|---|---|
| `GET /overview` | One bounded dashboard payload: recipe summaries, capabilities grouped by parent site and relevance, telemetry-hidden trace count, auth metadata, active grant count, and activity/health counters. |
| `GET /recipes` / `GET /recipes/{id}` | Recipe metadata or the complete versioned DAG; neither includes captured session values. |
| `GET /recipes/{id}/runs?limit=N` | Newest-first content-free durable run ledger. |
| `POST /recipes/{id}/replay` | Direct, non-interactive replay. An ungranted write returns 409 before any request is sent; a sent write with an unverified outcome also returns 409 and must not be retried. Generated skills add `published_only=true&expected_version=N`. |
| `GET /replay-grants` / `DELETE /replay-grants/{id}` | Audit and revoke durable request-shape grants. |
| `GET /registry?relevance=...&include_hidden=...` | Relevance-filtered learned-capability registry. |
| `GET /recipe-metrics` | Task-start lookup, replay, fallback, grant, approval, and auth-heal counters. |
| `GET /settings` / `PUT /settings` | Read or change the live scope override; only the owner can change the process ceiling. |
| `POST /settings/disable-and-purge` | Requires the exact typed confirmation, disables the scope first, then removes mining data, projection rows, grants, generated recipe catalog rows, and emitted recipe skills. |

`api_mining.enabled` is the process ceiling; a scope override can only be
stricter. While off, capture, auth drains, mining, compile/replay, generated
recipe tools and mutations are inert (mutations 409); inspection and purge stay
available. Origin purge removes every cross-origin recipe touching that origin as
one DAG (ledger, indexes, grants, generated pack, emitted skill), reports counts
separately, and is serialized against compiler publication and in-flight replays.
The optional verification worker is default-off and limited to Trusted,
read-only recipes with typed recurring-Monitor provenance. See
[API Mining Pipeline](../magician/api-mining-pipeline.md).

### Storage

- `GET /storage/activation` reports Gates 1/2/3, qualified operations and
  blocking reasons. `POST /storage/activation/cutover` requires
  `CUT OVER STORAGE` and returns 409 unless every precondition holds.
  `GET /storage` (compaction inventory) is separate.
- `GET /storage/maintenance` returns the workspace's latest Channel Assist and
  Feed maintenance state, last completion and reclaimed bytes from bounded status
  files, without opening databases ([maintenance visibility](../magician/storage-governance.md#maintenance-visibility)).
- `POST /storage/actions/app-store` accepts only typed `verify`, `optimize` or
  `reclaim` with matching confirmation, authenticates through the App transport
  scope, rejects a different query workspace, and calls the App registry owner
  (never a generic SQLite connection). Missing stores are 404 without being
  created ([App database maintenance](../magician/storage-governance.md#app-database-maintenance)).
- Attention semantic health separates active invalid/missing/dead work from
  historical queue failures ([attention worker recovery](../magician/attention-routing-funnel.md#incremental-history-maintenance-and-worker-recovery)).

### Spend and fleet

- Ceiling upsert clones under `system_ceilings.write`, then samples the ledger
  without holding ceilings. `POST /resource-authority/reservations` admits
  fail-closed ([resource-authority-api.md](../magician/resource-authority-api.md)).
- `GET /fleet-state` projects Town Square through `SocialApi::fleet_projection`
  (one reader of those rows). The section id stays `social_store` for wire
  compatibility; reason codes are `town_square_unconfigured`,
  `town_square_not_installed` (distinct: not degraded), `town_square_unreadable`.
  A failed feed read is `availability.social: unavailable`, never an empty array
  ([fleet-state-api.md](../magician/fleet-state-api.md)).

## Apps host API

### Public inventory and errors

Route registration consumes the supported-public inventory, including opaque
generation-bound logical-run cancellation and reviewed server-side action-result
composition. Inventoried routes canonicalize non-success responses to
`AppErrorEnvelope`; typed disposition owns retry and uncertainty. The signed
native owner channel stages only `snapshot`, `screenshot`, `launch`, `close`,
`tap`, `type`, `key`, `scroll`. Lifecycle transitions, grant revocation, update
abort and migration-coordinator operations return `app_installation_not_found`
404 for installations outside the scope. Unexpected governed workflow errors are
logged in full but stay opaque in responses; cancellation settles on the
structured execution runtime, off the request stack.

Terminal commits accept empty mutation batches as a receipt-free no-change
result; record-revision preconditions with an empty batch are 400.
`AppEntityMutationError::ReservedRecordId` maps to `400
invalid_app_data_mutation`.

### Entity mutations

`AppMutationOperation::Create` carries optional `record_id`, passed through by
the owner-mutation surface so a manifest can address a row by name. It is
`skip_serializing_if = "Option::is_none"`, so bodies without it keep their
canonical-JSON digest (existing idempotency keys still match). Errors:
`RecordAlreadyExists` (never an upsert), `ReservedRecordId` (`rec_` prefix is the
store's minting namespace), and a contract rejection when two operations in one
batch name the same record.

### Writing against the app data plane

Constraints that bite any caller of `owner_query` / `owner_mutate`:

- **Predicates are validated flat**: every arena node must be admissible even on
  an untakeable branch. Ordering comparisons are legal only on integer, decimal
  and timestamp — narrow on a timestamp and finish a text keyset in Rust.
- **Idempotency keys are `AppReference`s** (alphanumerics and `_-.:/@#`); hash
  user content rather than interpolating it.
- **A stable key needs a stable payload**: the replay check digests the whole
  command, so a fresh `created_at` under a fixed key is a permanent
  `IdempotencyConflict`. Make the payload deterministic or check existence first.
- **One page is 200 rows**; follow `next_cursor`.
- **Ties order by `record_id`** (opaque), so over-fetch then order when a window
  is sized exactly to a page.
- **`limit` is part of a cursor's identity** (`query_identity_digest`); keep page
  size constant and bound by row count.
- **One `In` predicate holds at most `max_collection_items` (256)**; chunk
  larger id lists.

### `/social/*` — Town Square data plane

`magician-api/src/social_api.rs` serves `/social/*` from the `town-square` app
package's entity store through the governed owner data plane. It lives here
because `authenticated_app_scope` does (one kernel, dependency one way).

- Responses are built from the original `social::types` structs (`post` is
  flattened into its reactions wrapper, `member` is not flattened into its mood
  wrapper, `next_before` is emitted as `null`).
- Writes use `owner_mutate`, not the package's `auto`-runner workflows, so a POST
  stays a synchronous 201 with a post id.
- `SocialApi` borrows `AppPlatformApi`'s `entity_adapter()`, `registry()` and
  `background_behaviors()` handles (a second registry would be a second set of
  caches).
- `/social/policy` reports `chatter_ready` and `scope_policy.{enabled,paused,state}`
  from the scope's pause control and the `ambient_turn` head; a failed health
  read reports "not configured", never healthy. `GET /social/health` remains
  (the Town Square page reads operator policy from it); `worker_global` is the
  `ambient_turn` behavior head.
- The members projection includes opted-out agents for the owner's roster;
  opt-out still gates mentions, groups and ambient selection.

### Background behaviors

- `GET /apps/background-behaviors` returns a ≤ 256-item metadata-only live health
  page; continue with all three of `before_updated_at`,
  `before_installation_id`, `before_behavior_id` (best-effort, not a snapshot).
- `PUT /apps/background-behaviors/policy` takes
  `{ "expected_revision": <u64>, "paused": <bool> }` (revision-CAS scope pause).
- Both return 503 only while `app_platform.background_behaviors.enabled` is false
  or scheduler construction failed. `worker_running` is owned by the scheduler
  (the supervisor marks it per attempt; the route returns the snapshot
  unmodified); during capped restart backoff claims pause while health and pause
  control stay available. Health holds only closed states/codes, counters, opaque
  fire refs, corruption counts and a bounded event feed — never entity values,
  prompts, provider output or raw errors.
- `POST /apps/background-behaviors/{installation_id}/{behavior_id}/retry` with
  `expected_installation_generation` and `expected_revision` requeues only a
  lease-free `workflow_launch_blocked` head; stale, running or other blocked
  states are 409. Due time, fire identity, starts, failures, budgets and grants
  are unchanged, and the worker rechecks policy, source, installation,
  permissions and capacity. `retry_scheduled` acknowledges the request, not
  success.
- App events and owner notifications have no public ingress or response route.
  A boot-installed Artifact canonical observer accepts only sealed foreground app
  root `agentic.execution_completed` facts (`succeeded|failed`) into the registry
  (V26 debt, V28 replay tombstones, V29 quarantine, V30 raw compaction cursors).
  The shared scope worker runs schedule, event and owner-notification lanes;
  owner delivery and terminal-payload maintenance run even while the
  schedule/event master is off. See the
  [threat model](../magician/app-events-owner-notifications-threat-model.md).
- Background scope sweeps admit up to four workspaces concurrently; the
  registry's fair background admission still bounds database work and preserves
  foreground headroom. Each workspace's four debt lanes (schedule, events,
  notifications, retention) keep independent budgets. Lifecycle changes make the
  next reconciliation sweep due without resetting cadence, starts, grants or
  budgets.

### Widgets, indicators and slots

- `POST /apps/widgets/render-batch` takes ≤ 12 `(installation_id, widget_id)`
  targets plus the closed native-client capability set — never a query or binder
  contract. Each target installation is read once and its snapshot reused; a
  scope epoch fences later lifecycle changes. Returns schema-v1 native models with
  per-item revisions and a batch ETag. `If-None-Match` yields an empty 304 when
  revisions are unchanged, with the ETag and the next foreground deadline in
  `X-App-Widget-Refresh-After` (CORS-exposed). The deadline is ≥ 5 s past
  `rendered_at`, and items within `max(5 s, refresh/5)` of expiry are recomputed.
  Cache keys include installation generation; the lifecycle hide hook evicts
  synchronously. Limits: 32 KiB request, 64 KiB per model, 1 MiB response.
- `GET /apps/indicators` reads ≤ 32 current, expiring materializations; it never
  evaluates an app or fans out to entity stores. Picker suggestions keep page,
  region and system-default intent with injective page-qualified slot ids.
- `GET /apps/slots/{slot_id}` is the single-slot read. `POST
  /apps/slots/resolve-batch` takes 1–12 unique page-qualified slot ids (≤ 16 KiB),
  preserves order, and returns ≤ 128 KiB from one inventory snapshot and one
  slot-state load; it requests only referenced installations
  (`AppSlotInventoryResolver::snapshot_for_installations`), releases its disk
  worker before awaiting package admission, and queues under caller cancellation.
  `GET /apps/slot-assignments` is the bounded picker page and takes the write
  fence; `POST` requires that fence, the expected revision and the exact picker
  candidate binding. Picker pages carry an inventory revision; reopen pagination
  if it changes.
- Native widget action launches may add a rendered-installation precondition
  (`generation` + `package_revision_ref`); a stale click is 409. An exact
  already-started response-lost retry recovers only control/result state; a
  sealed pending/ready retry revalidates live authority under the start permit.

### Custom surfaces v1

`/apps/installations/{id}/custom-surface-v1/host`, `.../assets/{digest}/{tail}`,
`.../bridge`, and `POST .../sessions/{session_ref}/reload-note`. All serving
routes answer the host route's 404 when `app_platform.custom_surfaces_v1.enabled`
is off. Host-open compiles a plan only for an entry point in
`granted_custom_surface_entry_points`.

- The minted `entry_url` carries the live session reference as the first segment
  after `assets/`: the one credential a sandboxed frame's opaque origin can
  present, and relative subresources inherit it by URL resolution (a query would
  be dropped by relative script references). It is a bearer credential for
  read-only serving of that installation's reviewed members; the bridge still
  needs host-page authentication and the full admitted envelope.
- `get_scripted_surface_asset_handler` resolves scope through
  `authenticated_app_scope` first and, only on the workspace-missing refusal (the
  hosted-web Cloudflare Access shape), through the live session
  (`session_bound_app_scope`, re-checking the identity's principal). Unknown or
  torn-down sessions answer `SessionGone`; wrong principals `app_scope_mismatch`.
  The bridge POST has no session fallback. See
  [custom-surfaces-v1.md](../magician/custom-surfaces-v1.md#session-bound-scope-hosted-web-cf-access-fix).
- `reload-note` is called by the host page or native shell (never the frame, whose
  CSP pins `connect-src 'none'`) for every reload and renderer crash; it reads no
  body and uses the same auth posture as assets. The kernel refuses a session of
  another installation, an expired session, and `ReloadBudgetExceeded` (3
  reloads/crashes), mapped to 409 `app_custom_surface_denied`; quarantine evicts
  the session. Any `%` in `{session_ref}` is the asset route's early 404. A leaked
  ref can at worst drive that one session to eviction.

### Staged-ingest apply

`POST /apps/installations/{installation_id}/claims-ingests/apply` is the host
half of claims-review's `stage_ingest` workflow (the package can only write an
`ingest_request` row). It reads the row's live head under one
installation/grant/schema snapshot, applies it through `TranscriptIngestion`,
and stamps `apply_state`, `actor_ref`, `act_ref` back with a revision
expectation. Body: `record_id`, `record_revision`, `effective_speaker`, `ours` /
`counterparty`, `occurred_at`, optional `consequence_class` (`private_local` and
unknown values refused). No desktop signature: recording that a room happened is
what `POST /transcripts/ingest` already allows an owner session; *settling* a
claim needs the paired desktop. Admission order: interactive owner session,
enabled installation, live source head as the final registry read, scope
liveness re-sampled at the mutation boundary. A failed stamp answers
`staged_row_stamped: false` with the act ref; apply is idempotent per
`ingest_id`. See [commitments.md](../magician/commitments.md).

### Meeting-control owner decisions

`POST /apps/installations/{installation_id}/meeting-controls/owner-decisions` is
the signed destination for the meetings console's `listen`, `join`, `pause`,
`resume`, `stop`. Admission order (as the claims-decision sibling): interactive
owner session, enabled installation, envelope and command validation,
route-authority match, package/workflow/action re-resolution, active desktop
pairing, then the live `control_request` head as the final read, with the
decision timestamp re-sampled at the mutation boundary (a start's gesture expiry
measures exactly that). `/meetings/*` routes and JSON are unchanged for their
four consumers (web, iOS `MeetingsAPI.swift`, Android, the `meeting` tool); they
append to the shared capture-control audit (refusals too), and
`/meetings/upcoming` delegates to the single calendar owner. A `wire_parity` test
pins row key sets and liveness rules. See
[meetings-surface.md](../magician/meetings-surface.md).

### Older-data cleanup

`GET /apps/installations/{id}/data-cleanup` (indexed timestamp options and the
owner's latest job), `POST .../data-cleanup/preview` (exact age selection), and
`POST .../data-cleanup/{job_ref}/control` (confirm, pause, resume, cancel) or
`/advance`. Confirm requires the preview digest and literal
`confirmation: "delete_older_app_data"`; unknown fields and cross-owner/scope
requests are rejected. Expired previews 410, changed installation/state 409, busy
final WAL checkpoint 503. Advance is idempotent after completion and commits
removal and progress atomically. Owner maintenance only, never a sandbox SDK or
bridge capability ([entity-store cleanup](../magician/app-entity-store.md#owner-approved-age-cleanup)).

### Memory access

`GET /apps/installations/{installation_id}/memory-access` returns the app's
owner-memory request, the grant made at review, the grant in force
(`interactive` / `background`, `null` = nothing), the CAS `edit_revision`,
enablement, and the user-tier catalog with `ordinary` / `sensitive`
readability. `POST` takes `{expected_edit_revision, interactive, background}`
(unknown fields refused); a stale revision or selection outside the request is
`409 app_memory_access_conflict`. Applies on the app's next memory read
([app-memory-access](../magician/app-memory-access.md)).

### Transcripts and Envoy claims

- `GET /transcripts/claims/{claim_id}/pending-confirmation` (owner-only) returns
  an admitted-but-unapplied confirmation (`decision_id`, `expected_revision`,
  `by`, `note`, `prepared_at`) or `{ "confirmation": null }`; it never applies
  it (`POST .../confirm` rechecks under the destination lock). Bot, API-token and
  grant bearers cannot read it.
- Claims Review exposes bounded status/search/cursor listings and delivery
  provenance. The Envoy delivery endpoint requires a runtime-minted bearer for
  the persisted session's bot and scope. Its request carries
  `binding: {attempt_id, channel_type, channel_address, payload_sha256}`, shared
  by all receipt phases; a missing or mismatched binding on a prepared reply is
  409. Begin retries may resume only the same unsent attempt; a fresh attempt
  gets `send: false` once dispatch began.
- Statement confirmation/rejection and commitment confirmation require an
  interactive owner session (or verified paired-device/local boundary);
  `by` is attribution only; workspace selectors must match the authenticated
  scope. See [Claims Review](../unified-ui/claims-review.md).

### `AppPlatformApi::admit_system_packages_at_boot` (2026-09-03)

Admits the deployment's `distribution: system` packages at startup, publishing
inert `ready_for_review` installations only. Which scopes it touches:
[system-package-boot-admission-scopes.md](system-package-boot-admission-scopes.md).
Failures are logged per package and never propagate (one malformed seed must not
cost the others). Trust argument:
[system-boot-admission](../magician/system-boot-admission.md).

It returns `SystemPackageBootSummary` per scope: the admission report plus
`enablement_failures`, kept apart because admission failure is bad bytes and
enablement failure is good bytes the host would not grant.
`enable_boot_admitted_package` runs the reviewer's two steps (`review`, then
`approve` presenting the shown digest). With `enable_at_boot`, admission mints a
non-transport host grantor bound to that one admitted installation; it cannot
execute an app or approve a neighbour, and ordinary system workers still cannot
approve. The schema default is off; the shipped config opts in; opted-out
deployments use `POST /apps/installations/{id}/approve`.

Boot admission refreshes the scope's widget registrations before minting system
slot defaults. The projection worker gates each drain on its own repair: a failed
personal-agent retrieval or memory-contribution repair does not skip entity,
lifecycle, capability-overlay or widget-registration drains, and repair failures
log on the first three passes then once per sixty (`app boot repair will
retry`).

## Tests

- **Fixtures run on the stateless driver.** It is the default; scopes need no
  seal or activation step (the per-scope legacy-writer seal was retired
  2026-10-08). **Do not set `MAGICIAN_EXECUTION_DRIVER=inprocess`** to pass
  tests — it hides failures by not exercising the production driver. Steers are driver-specific (`Inprocess`
  pushes to `steer_queue`; `Stateless` writes a durable inbox), so tests
  asserting on the queue gate on `ExecutionDriver::from_env()`.
- **`apps_api.rs` synchronous tests** must use `#[::core::prelude::v1::test]`:
  its `use actix_web::{.., test, ..}` imports actix's `#[test]` macro, which
  requires an `async fn`.
- Boot enablement coverage:
  `configured_boot_enablement_surfaces_every_system_package_and_pins_defaults`,
  `an_owner_can_enable_every_boot_admitted_system_package`,
  `admission_without_enablement_leaves_every_package_inert`.
- `make test-plane-elicitation` — plane door/adapter contracts and the official
  SDK client regression, without the monolith fixture feature.

Decision model observability: owner-session authenticated, middleware-scoped `GET /api/magician/v2/plane/decision-metrics`, `/cases`, and revision-checked `POST /review` expose bounded incremental summaries and private sampled review inputs; query/body fields cannot choose a principal or workspace.

Quality continuation uses `POST /api/magician/v2/relentless/runs/{id}/budget-resets`
with `reset_id`, `expected_generation`, `expected_contract_hash`, and a `reset`
containing only exhausted `cost_microusd`, `wall_clock_secs`, and/or `attempts`.
The verified owner boundary records one idempotent approval on the existing run;
ordinary budget grant routes continue to refuse externally controlled roots.
The existing run cancellation endpoint also closes budget-paused work without
granting another allowance; repeated cancellation remains idempotent.
Run inspection exposes effective ceilings, independent current-window usage,
cumulative ledger totals, exhausted limits and owner-reset history.

## LLM call analytics reads

`POST /analytics/llm_calls/query` and `/query_batch` (and the crew-health
rollup) read the `llm_calls` view from each day's compacted object plus its
uncompacted tail, for both halves of the view: the call facts and the provider
attempts joined in for provider and model
(`governed_attempts_read_source_sql`). The attempts were read raw before —
every `part-provider_attempt-*` revision, 42,110 files for three busy days, 8.4 s
against 0.01 s for the compacted objects — so every cost query (the VibeDev
rail's lifetime and per-run spend, `/llm`) took 6–14 s and held the shared
analytics DuckDB guard meanwhile, timing out the queries queued behind it.
Choosing the files is cached per partition in process (see
[storage governance](../magician/storage-governance.md)), so after the first
read only partitions that changed are re-validated.

Scheduling adds `POST /work/negotiations/{id}/bind-offer`,
`POST /work/negotiations/inbound` and the paginated owner read
`GET /work/follow-through?kind=scheduling|delivery&offset=0&limit=20` (limit 1–100).
Holds require `calendar_act_ref`; held close/reschedule requires
`release_act_ref`. Actual provider evidence is checked in the authenticated
scope; arbitrary event strings and captured dispatches are refused. Signed
AgentMail statuses resolve the original send ID automatically, retain original
provider bytes in private attempt history and use recipient-scoped ledger keys.

### Optional app recording configuration

Authenticated `GET /api/magician/v2/runtime/client-config` returns only
`clarity_project_id` with `Cache-Control: no-store`. It reads the active private
runtime env on each request, validates a tracking ID and accepts an explicit
empty value as off. It never projects export tokens or other environment values.
The ordinary runtime env endpoint remains status-only and setup-token gated for
writes. See [Clarity setup](../../setup/clarity.md).

## Scoped speaking voices

`GET /media/providers` supplies TTS and realtime voice catalogs.
`PUT /media/preferences` accepts optional partial `tts_voices` maps keyed by
configured binding ID, alongside `realtime_voices` keyed by profile ID. Unknown
bindings/voices return 400; empty/auto/default values reset one binding. Both
TTS synthesis endpoints apply scoped choices unless a request supplies a voice.
Fallback providers use their own saved choice. Single synthesis returns the
actual `X-Tts-Voice`. The additive fields preserve preferences schema 3.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).
