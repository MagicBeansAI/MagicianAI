# Magician Docs

**Current development version:** `0.7.89`

Landing page for the `magician` runtime. Dated development updates live in
[CHANGELOG](../../../magician/CHANGELOG.md). Canonical Magician docs live under
`docs/components/magician/` (with `execution/`, `plans/` and `research/`
subtrees).

## Current contract

- **Identity.** Product identity is **Magican**. Constrained wake clients arm
  `magical` and `magician`; the server also admits the canonical name. App links
  use only `magican://`. Tunnel/Access/Notes/webhook hosts live on the
  operator's Cloudflare zone (`MAGICIAN_TUNNEL_ZONE`, no default); public site
  `next.magican.ai`. Mobile APNs delivery defaults to the
  `com.magicbeans100x.magican` bundle topic; `MAGICIAN_APNS_TOPIC` overrides it
  for a custom bundle identifier.
- **Credentials.** Custody, policy, redaction helpers and neutral session types
  come from pinned `magicvault-core` behind the existing secrets facade
  (including the additive `ProvisionedSecretMetadata` projection). Magician
  keeps its keychain identities, scope paths, signing/bootstrap, action
  traversal and browser/process ownership; no daemon, `secure_fill` dependency
  or storage migration. Source-attested app approvals fail closed on changed
  implementation identity. Background: Phase 1,
  Phase 2.
- **Storage.** Profile `local_embedded`; remote adapters and the migration
  coordinator stay unselected at startup. Rollback invalidates forward evidence
  after `RollbackPending`. `SecretRef` accepts slash-separated
  `credentials_ref`. The skill-working envelope does not steal CLI CWD. JSONL
  commit markers stay on the file provider. See
  [storage-abstraction.md](storage-abstraction.md) and
  [jsonl-durability](jsonl-durability.md).
- **Notes** live at
  `$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>`. See
  [notes-provider.md](notes-provider.md).
- **Execution driver.** `MAGICIAN_EXECUTION_DRIVER` defaults to `stateless`;
  `inprocess` is explicit rollback. Canary: `GET /health/execution-driver`. See
  [flat-loop](execution/FLAT_LOOP.md).
- **Chat mouth.** `chat.harness_engine` (default `magician`) selects who speaks;
  hands stay on the [plane](plane.md). Native-tool strip is per engine
  (`stripped` / `sandboxed`); each engine resumes its own native session per
  conversation; mouth tools are bridged. See [chat-mode.md](chat-mode.md).
- **Run engine.** `execution.harness_engine` (default `magician`) selects who
  thinks inside a run's loop; `claude_code` / `codex` / `grok` / `agy` replace
  decide+execute with one harness turn and inherit the run's tool catalog (a
  skill pack's leaf actions are dispatchable when its pack is).
  - A harness turn that reports token usage is charged against the execution
    token cap; **one that cannot is unmetered** (the CLI bills its own
    subscription, and codex reports usage on a `turn.completed` line a truncated
    stream never carries). Magician's own metered providers still fail closed.
  - Jev's step judge, the structured-decision gate and anything else built into
    the decide phase do not run under a foreign harness.
  - A harness CLI's own time limit must not end a turn: agy exits 0 with partial
    output when its print timeout fires, so it runs with `--print-timeout 0` and
    Magician's ceiling and idle watchdog bound the turn.
  - The harness framing is the store-managed prompt `harness_execution_preamble`
    (`data/magician_v2/prompts/`, `{identity_line}` variable) with the compiled
    literal as `rendered_prompt_or`'s fallback. Goal, task id, environment
    knowledge and history stay assembled in code. `rendered_prompt_or` returns
    the fallback verbatim, so a caller with placeholders must substitute them.
- **Plane delegation tools.** `delegate_to_agent` / `find_agents_for_capability`
  / `get_agent_details` (`PLANE_DELEGATION_HOT`) are advertised only to a grant
  whose agent has a delegation target (same rule as `flat_loop::catalog`); an
  operator overlay cannot reintroduce them, since a tool whose every call must
  fail is not a configuration choice. The harness prompt names the agent and
  says when it is the worker.
- **Desktop.** The desktop skill's `action_name` enum is generated from the
  installed driver by `scripts/sync_desktop_action_enum.py` (`--check` for CI).
  The plane's `http` tool names the reachable desktop relay
  (`127.0.0.1:3017/host/ax/…`) and hints that the skill is the better door. The
  gateway logs every AX action that crosses it (`host gateway AX action`,
  addressing fields only, never typed text).
- **Typed input.** Form-capable terminal MCP sessions support text, choices,
  forms and approvals through [plane typed input](plane.md#typed-input)
  (`request_user_input`, `wait_for_run`). Tests:
  [focused lane](../../testing.md#plane-typed-elicitation).
- **Auth.** General V2/V3 routes accept only workspace-bound session/API
  bearers; terminal grants stay on `/plane/mcp`. Open mode ends anonymous
  bootstrap when the first identity is created.
- **Spend.** Chat dispatch, Zepto/Swiggy checkout, REST reserve, and
  compiled/OS-jail tool I/O share `spend_session::admit`. Daily ceilings count
  live in-flight holds. YAML ceiling edits apply after restart. See
  [quickstart §6](../../quickstart.md) and
  [resource-authority-api.md](resource-authority-api.md).
- **OPC** modules are generic primitives; OPC is a caller, not an owner.
  Capture mode is ON; approval envelopes default to `off`.
- **Process spawning.** Every spawn site that overrides the child's `PATH`
  (skills runner, pack and compiled providers, CLI-template dispatcher,
  preflight probes, meeting calendar, verification runner, plane engines)
  resolves its program with `runtime_core::process::resolve_program` against
  that PATH. A bare name plus a `PATH` override makes Rust `fork` instead of
  `posix_spawn`, and the forked copy can hang in macOS atfork handlers (symptom:
  port bound but never serving, a second `magician.bin` at 100% CPU). See
  `docs/components/runtime-core/runtime-core.md`.

## Apps runtime

- **Recurring behaviors** reuse one internal task with independently governed
  occurrences; scheduling waits for settlement, and recovery, results and
  cancellation keep each occurrence's exact binding. See
  [Recurring App tasks](recurring-app-tasks.md).
- **Threat models.** Scheduled behaviors are interval-only, own-store-only and
  deny-by-default; durable fires reopen scope, generation, grant, kill switches,
  compiled effect ownership, content taint/provenance and resource ceilings
  before unattended work. See the
  [app background behaviors threat model](app-background-behaviors-threat-model.md)
  and the
  [app events and owner notifications threat model](app-events-owner-notifications-threat-model.md).
- **Meetings** is a system-class app over the read-only six-action
  `meetings_data` binder and the owner-signed `magician.meeting-control`
  destination (only first-party join/listen create captures; START needs a
  fresh surface gesture and is refused while a capture is live; every control
  is audited per scope). The scripted-surface watchdog allows 32 bridge messages
  per session, so the console spends a bounded polling budget. See
  [Meetings surface](meetings-surface.md) and [App tool bind](app-tool-bind.md).
- **Runtime compatibility.** Recipe and compiled effect owners use stable
  reviewed contracts; see [Runtime compatibility](app-runtime-compatibility.md)
  for the legacy baselines that preserve immutable package locks.
- **Task creation recovery** restores pristine `internal` (and legacy
  `persistent`) App task shells only with the exact server-authored App marker,
  scope, installation and pending/no-execution/no-output state and no published
  binding; it cannot reconstruct a started or one-sided binding. Owner retry of
  a background launch keeps the original fire and counters and re-enters
  ordinary admission (registry schema V34 carries the `retry_requested` audit
  event). Startup cleanup clears a failed root's cleanup hint only when it never
  entered resource admission and has no effect/model/result evidence and no
  registry root or ledger identity.
- **Resource cleanup** respects each execution node's recorded close time. An
  uncertain reservation survives terminal execution with its full token/cost
  bound; crash, committed-store and no-effect recovery charge active time only
  within the node's journal-owned lifetime. Startup recipe recovery settles
  reservations in the same boot when it terminalizes a root; a root whose
  deadline expired while the process was down is terminalized directly and gets
  no fresh deadline.
- **Lock ordering.** Native commits take task before resource. App model
  dispatch therefore releases the resource mutex before disclosure/recipe-claim
  callbacks; a non-owning clock bound to the root lease keeps admission time and
  detects release, and the physical reservation's expiry is rechecked right
  before provider I/O. Live resource admission samples time after taking the
  resource root lock, so a queued participant cannot append an older
  observation. Accepted journal reads use one SQLCipher read transaction.
  Contextual rounds match rows to mutation receipts by entity and record ID.
  Regression lane: `make test-app-runtime-concurrency-regressions`.
- **Recipe claim renewal** records its request time before waiting for the task
  lock; a heartbeat requested within its lease renews afterwards only if owner
  and epoch still match and cancellation has not won, and the active deadline is
  checked after the wait.
- **Stack/worker use.** Goal-trigger admission crosses the abort-on-drop
  execution-job boundary before creating Artifact or Runtime shells, so startup
  hydration, scheduler wakes and chat never poll it on their own stacks.
  Workflow primitive discovery runs on the blocking pool at its validation
  boundaries.
- **Task projections.** Canonical Apps action runs use the Internal task
  lifecycle; legacy runs keep their paths and are reclassified in list
  projections and index rebuilds (list index schema 4). Background discovery
  treats installation lifecycle changes as an invalidated inventory snapshot and
  acknowledges each completed sweep's start, so changes behind an active cursor
  surface next sweep.
- **Deleted-task debt.** Terminal loop settlement may suppress it only when a
  valid deletion marker is protected by the task deletion lock and the only
  survivors are known generated execution output paths (which are kept);
  anything else keeps the debt pending.

## Headline capabilities

Magician is a **runtime for autonomous personal-agent operations**:

- **Agents-as-data.** Every agent is a YAML file in
  `magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`
  (persona, tools, delegation, memory tiers, prompt pipeline, constraints). See
  [`agent_templates/README.md`](../../../magician_data_v3/system/agent_templates/README.md)
  and [Agent Definition Reference](agents/agent-definition-reference.md).
- **Closed self-improvement loop.** Procedure feedback drives extraction →
  retrieval → feedback → promotion → growth evaluation. See
  [Learning Procedures](learning-procedures.md).
- **Per-agent memory shape.** Agents declare typed memory tiers (schema, scope,
  retention, render format); the backend is fixed (Parquet lakehouse + LanceDB
  hybrid index).
- **Drop-in skills.** A skill is `<name>/SKILL.md` in `skillshub/`.
  `make skills-install-scope SCOPE=<principal>/<workspace>` symlinks skills into
  the runtime tree (source edits are live). Validation runs on install; runtime
  discovers via `scope_loader::procedure_tool_infos_for_agent`. Legacy `tool_schema.yaml`
  is accepted only for unmigrated external packages. See
  [`skillshub/README.md`](../../../skillshub/README.md),
  [skills-spec](skills-spec.md), [skills-quickstart](skills-quickstart.md).
- **Channel adapters.** New platforms implement the six-method
  `ChannelAdapter<TTarget, TContext>` in
  [`@magician/bot-sdk`](../../../skillshub/bots/sdk/README.md); each bot runs as
  its own scope-aware daemon under `magic-supervisor`. Enrollment-backed chat
  identity is active; bot control lives under `/api/magician/v2/bots/*`.
- **Capability tokens + double-entry ledger.** Every call carries a token; the
  runtime refuses over-budget calls. Every `CapabilityProvider::lower()` honours
  `execution.spend:` via `maybe_wrap_with_spend_gate`; dispatch routes through
  `execution::compiled_dispatch`. See
  [Resource Authority Admin API](resource-authority-api.md).
- **Generative UI (MUIJ).** Agents emit layout documents rendered as charts,
  cards, feeds and interactive surfaces.
- **Shared ChatPanel.** `/chat`, `/hud` and the `/t/[name]` Chat tab mount the
  same `ChatPanel`.
- **Curated delivery and task retention.** Published dashboards/briefings
  project from `published_surface.changed` into `data_delivery` feed cards;
  scheduled task terminals publish `routine.result_published` into
  `routine:<task_id>` cards. Delivery `task_id` is provenance. Transient work runs
  as `lifecycle: internal` tasks swept with their chat session; `track_as_task`
  keeps one as a persistent task.
- **Progressive continuation.** Completed tasks emit continuation-context
  outputs; follow-ups pass `reference_task_ids`; `get_task_details` returns
  continuation plus bounded previews.
- **Internal tasks.** `/tasks?type=internal` reads only canonical
  `internal_tasks/<id>/` rows, recovers stale active executions from terminal
  events, and projects persisted HITL pauses into attention summaries.
- **Out-of-process executor.** `magicutor` runs browser automation and heavy
  tool calls; `magic-supervisor` enforces restart policies.
- **Scoped event lakehouse.** Events are partitioned by `(principal, workspace)`
  and retention-windowed by the `transport_log` compactor. Limits:
  - Durable appends sharded by task, ≤16 concurrent writes, `write + flush +
    sync_all`; `events.jsonl.commit` is the durable byte boundary.
  - Handoff bounded to 4,096 events / 64 MiB; saturation backpressures, never
    drops; an oversized event goes alone after prior work drains.
  - Recent views 72 MiB; sequence recovery 128 MiB. Records over 64 MiB or past
    JSON depth/node ceilings, or more than 16,384 output refs, fail closed.
  - Finalizers stream-verify SHA-256/UTF-8; voice metadata keeps a 32 KiB
    preview; cards read at most 256 KiB. Artifact V2 retries only transient
    `ENFILE`/`EMFILE` on scoped hot paths.
- **UI-thread deletion.** Non-`#general` threads: tombstone the record, drop
  chat sessions, clear thread-scoped memory, keep tombstones for sync.
- **Harness personal agents.** Scoped CEO, CTO, CMO, CRO with focus-area goals;
  structural changes are proposal-backed. Harness is a capability on
  `kind: personal`.
- **HITL.** `POST /api/magician/v2/hitl/{approval_id}/respond` with
  `source: "approval"`; `/approvals/{approval_id}/resolve` is `410 Gone`. See
  [HITL](hitl-attention.md).
- **Live config reload.** `POST /api/magician/v2/settings/magician-config/reload`
  refreshes the operation router, multi-LLM service, native-tool policy, tool
  authorization, complexity routing and Workbench CLI catalog; restart-only
  sections are reported.
- **Inner-loop packs.** Provider-backed packs (`duckdb`, `imessage`) register
  twice: pack-defined for outer routing, then
  `CapabilityRegistry::register_override` for compiled dispatch. A
  `[CAPABILITY] Overwriting existing provider` log line means an accidental
  replacement.
- **Container Messages reads.** On Linux without a local Messages database,
  `imessage` delegates reads to the desktop's private `/host/imessage/query`
  relay; the Mac opens its database read-only with query/time/result limits
  (Full Disk Access for Magican). See the
  container audit.
- **Remote desktop.** An enrolled desktop keeps an outbound Edge socket; the
  server can dispatch bounded CUA actions, Magicutor/CDP health, target
  discovery, one-shot commands and macOS read-only Messages queries. The desktop
  never publishes its loopback gateway, CDP URL, Messages path or device
  credential.
- **Remote MCP account setup.** The OAuth broker begins a governed binding
  without a browser on the engine; a setup-token-gated native client opens the
  authorization URL locally, callbacks return to the engine's HTTPS public
  origin, and tokens stay in the engine vault. Local engines keep the loopback
  callback.
- **Spatial surfaces.** The canvas specialist lane is retired; spatial
  observation, coordinate-native browser actions, iframe reprojection and
  post-action evidence stay in the normal agentic loop.

## DuckDB analytics

Two independent uses (see [duckdb-analytics.md](duckdb-analytics.md)):

- **Agent tool.** Compiled `DuckDbCapabilityProvider` (in-memory session per
  cycle; CSV/JSON/Parquet + temp tables) for `simple-data-analyst` and
  `internal-system-analyst`. Pack:
  `magician/src/magician_v2/execution/embedded_pack_defs/duckdb.yaml`
  (`include_str!`; guide edits need a rebuild).
- **Internal analytics.** Schemaless `events` in
  `magician_data_v3/scopes/<principal>/<workspace>/analytics/analytics.duckdb`.
  `analytics::emit()` is fire-and-forget; tracing captures INFO/WARN/ERROR.
  Retention: rows newer than 24 h plus at least the newest 1000 per event type.
  API: `GET /api/magician/v2/analytics/schema`,
  `POST /api/magician/v2/analytics/query`.
- **Diagnostics.** `internal_data` is a compiled diagnostic capability
  (`catalog`, `schema`, `query_events`, `query_llm_calls`, `tail_logs`,
  task/execution listing, output and `events.jsonl` reads).
  `internal-system-analyst` uses it before `duckdb`; `personal-assistant` also
  has it.

Related skills: `sheets` and `rg` in `skillshub/`. Gmail/Sheets auth:
`gws auth login -s gmail,sheets,drive,docs,calendar`.

## Browser engines

**Cloak Browser** (`skillshub/cloak-browser`): stealth Chromium for the
`browser` tool (wrapper `0.5.3`, licensed Chromium 150). The installed runtime
sets `AGENT_BROWSER_EXECUTABLE_PATH` for `headless` and `headed`; `cdp` mode
(user Chrome via Magicutor) is unaffected.

- Never use the CloakBrowser Python SDK at runtime (Playwright re-introduces
  JS-layer leaks); it is only a library in
  `skillshub/cloak-browser/scripts/resolve.py`, which returns a JSON envelope
  and never launches. Smoke: `skillshub/cloak-browser/scripts/smoke-test.sh`.
- An explicitly configured engine that cannot resolve fails the operation.
  Exit `76` (concurrent session) closes the daemon and retries once with
  bundled Chrome for Testing; `77`/`78`/`79`, crashes and page failures never
  do. Omitting the engine uses bundled Chrome.
- CLI fork `agent-browser 0.38.1-Magician.0`, also exposing `webmcp`,
  `pushstate`, `vitals`. Default `AGENT_BROWSER_IDLE_TIMEOUT_MS=0`. Verify:
  `make -C skillshub verify-agent-browser`. Install:
  `make -C skillshub setup-cloak-browser` → `~/.cloakbrowser/`;
  `CLOAKBROWSER_LICENSE_KEY` selects the build. Magician never bundles the
  binary. Evidence: [Browser Engine Observability](browser-engine-observability.md).

**Lightpanda** (`skillshub/lightpanda`): headless DOM engine behind the same
`browser` tool, started by `agent-browser` with
`AGENT_BROWSER_ENGINE=lightpanda` (`lightpanda agent` is never invoked).
`content_acquisition.browser.public_read_engine: lightpanda` is first
preference for isolated public headless reads; failures close the session and
fall back Lightpanda → `content_acquisition.browser.engine` (`cloak-browser`
shipped) → bundled Chrome. Authenticated and side-effecting work is never
replayed. Headed work, screenshots/PDF/recording, visual grounding, profiles,
extensions, authenticated state, files, meetings, previews and
fingerprint-sensitive sites stay off Lightpanda; pixel commands fail clearly.
`make -C skillshub setup-lightpanda` / `verify-lightpanda`; honours
`LIGHTPANDA_EXECUTABLE_PATH`; telemetry and crash dumps disabled.

## Username OSINT tool

`skillshub/find-details-by-username` wraps Maigret (`maigret==0.6.1`) as Vera's
`findDetailsByUsername` helper. Reports land under
`<scope>/workdirs/find-details-by-username/<query>-<timestamp>` when
`MAGICIAN_SKILL_DIR` is set. Maigret `--ai` stays disabled. Matches are public
leads, not verified identity facts.

## Adding a capability pack

See [skills-spec](skills-spec.md) and the
[Pack Authoring Guide](../capabilities/authoring-guide.md). Catalog sources,
earlier wins on name collision:

1. Scope skills: `<storage_root>/scopes/<principal>/<workspace>/skills/<skill>/SKILL.md`
   (materialized by `make skills-install-scope`).
2. Embedded compiled defs:
   `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml`.

`magician_data_v3/system/capability_templates/packs/` and the
`<scope>/capabilities/packs/` fallback are decommissioned. The pack list is
served via `GET /api/magician/v2/skills` (compiled packs as `kind: 'compiled'`,
`layer: 'built-in'`); `/api/magician/v2/capabilities/packs` is retired.
Validate with `make test-capabilities`.

### Parameter schemas

Each entry in a pack's `parameters:` resolves to a canonical JSON Schema the
tool catalog emits verbatim. Scalars (`param_type: string|integer|boolean`,
optional `enum_values:`) auto-desugar; arrays, objects, regex, formats and
`oneOf` need an explicit `schema:` block:

```yaml
- name: evidence_refs
  required: true
  description: "Non-empty list of evidence references."
  schema:
    type: array
    items: { type: string }
    minItems: 1
```

Both catalog generators read it through `derive_param_schema_for_emission`. A
native action leaf exposes only the names in that action's `parameters`; an
explicit `parameter_overrides` schema wins, otherwise the leaf inherits the
canonical pack schema (types, enums, nested constraints). Flat-loop catalogs
apply exact tool denials to each expanded leaf as well as its pack; a denied
leaf stays out of deferred discovery, loaded tools and eager catalogs while
allowed siblings remain.

## Canonical references

- Runtime / API: [Plane](plane.md), [V2 API Guide](v2-api-guide.md),
  [V3 Complete Flow](v3-complete-flow.md),
  [V2 WebSocket Events](v2-websocket-events.md),
  [HITL / Attention Contract](hitl-attention.md) (single envelope, endpoint and
  modal for every human-in-the-loop ask),
  [Captured Auth For API Replay](auth-capture-and-replay.md),
  [API Explorer](api-explorer.md), [API Mining Pipeline](api-mining-pipeline.md),
  [API Mining Projections](api-mining-projections.md),
  [Resource Authority](resource-authority-api.md),
  [JSONL durability](jsonl-durability.md),
  [Changelog](../../../magician/CHANGELOG.md) (`v0.6.430` is a UI-paired no-op;
  see `docs/components/unified-ui/README.md` for theme details)
- Apps: [Town Square as an internal app](town-square-app.md),
  [Diagnosing a stranded app workflow task](app-workflow-stranded-tasks.md),
  [App Background Behaviors Threat Model](app-background-behaviors-threat-model.md),
  [App Events And Owner Notifications Threat Model](app-events-owner-notifications-threat-model.md),
  [Recurring App tasks](recurring-app-tasks.md)
- Chat / media: [Chat Mode](chat-mode.md),
  [Chat Profile Routing](chat-profile-routing.md),
  [Chat SSE Streaming](chat-sse-streaming.md),
  [Realtime Media Rails](realtime-media-rails.md),
  [FluidAudio Phase 9](fluid-audio-phase9-rollout.md),
  [Offline Audio Evals](fluid-audio-offline-evals.md),
  [Personal Tutor](personal-tutor.md)
- Storage / memory / notes: [Storage Abstraction](storage-abstraction.md),
  [Storage Governance](storage-governance.md),
  [Storage V2 Format](storage-v2-format.md),
  [Workspace File Provider](workspace-file-provider.md),
  [Notes Provider](notes-provider.md),
  [Taste Profile](taste-profile.md) (owner-edited taste note snapshotted for
  prompt injection), [Memory Index](memory-index.md),
  [Memory Retrieval Evals](memory-evals.md),
  [Vector Toolkit](vector-toolkit.md),
  [Ollama Logical-context Chunking](ollama-logical-context-chunking.md),
  [Durable Artifact Tag Index](execution/DURABLE_ARTIFACT_TAG_INDEX.md)
- Agents / skills / learning:
  [Agent Definition](agents/agent-definition-reference.md),
  [Agent Updates](agent-updates.md),
  [Agent-Owned Identities](agent-owned-identities.md) (the agent's own
  Google/AgentMail/WhatsApp identities, `self_identity` tier; the agent's own
  address is never an owner),
  [Trust-Tiered Agent Task Dispatch](agent-task-dispatch.md),
  [Skills Spec](skills-spec.md), [Skills Quickstart](skills-quickstart.md),
  [Skills Authoring](skills-authoring.md),
  [Learning Procedures](learning-procedures.md),
  [Plan Flow](plan-flow.md) (how the planner finds tools),
  [Work Evidence Graph](work-evidence-graph.md)
- Surfaces / channels: [Today Feed](today-feed.md),
  [Content Sources](content-sources.md),
  [Town Square Fleet-State API](fleet-state-api.md),
  [Fleet Social Network](fleet-social-network.md) (superseded; kept for the
  `social:` config block), [Crew Health](crew-health.md),
  [Consumer Channels](../../consumer_channels.md) (Telegram, WhatsApp),
  [File Context Carry-through](file-context-carry-through.md),
  [Browser Engine Observability](browser-engine-observability.md),
  [Developer Mode Workbench](developer-mode-workbench.md),
  [CLI Delegate Ecosystem](cli-delegate-ecosystem.md),
  [Local Resource Governor](local-resource-governor.md)
- Execution: [Execution Docs](execution/README.md),
  [Flat loop](execution/FLAT_LOOP.md),
  [DuckDB Analytics](duckdb-analytics.md),
  [Execution Archive](execution/archive/README.md),
  Research, Plans
- Workspace: [Architecture V2](../../ARCHITECTURE_V2.md),
  [Quick Start](../../quickstart.md), [Capabilities](../capabilities/README.md),
  [Pack Authoring Guide](../capabilities/authoring-guide.md)

Archived (not current contracts):
V2 flow,
Progressive continuation,
FluidAudio Phase 0,
Clarification batch,
Clarification state,
Task index,
JIT lowering,
Execution persistence,
Delegation V2,
Work evidence design,
UX research,
VFS TODO,
Canvas board,
Canvas design,
Canvas arbitration,
Resource authority design,
Resource authority L2.
