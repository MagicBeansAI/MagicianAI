# Magician Docs

Generation routing uses the [model catalog](../magicllm/model-catalog.md):
provider connections, one capability/limits record per model, shared workload
budgets, and model/thinking assignments. Historical saved choices convert once
on clients; runtime aliases are rejected. Parent-engine, locality, sealed-route
and structured chunking rules govern the resolved physical calls.
Native coding engine choices derive from the shared catalog. Their model and
effort are pinned in the dispatch journal and passed to the corresponding CLI;
readiness and isolation eligibility still come from the qualified engine.

**Current development version:** `0.7.142`.

Atlas owns scoped runtime inspection; God-mode HTTP and file-search approvals retain authentication and privacy fences.

Landing page for the `magician` runtime. Dated updates:
[CHANGELOG](../../../magician/CHANGELOG.md). Canonical docs live under
`docs/components/magician/` (`execution/`, `plans/`, `research/`).

## Current contract

- **Identity.** Product **Magican**; wake words `magical`, `magician` (server
  also admits the canonical name); app links only `magican://`.
  Tunnel/Access/Notes/webhook hosts live on the operator's Cloudflare zone (`MAGICIAN_TUNNEL_ZONE`, no default); public
  site `next.magican.ai`. APNs topic defaults to `com.magicbeans100x.magican`;
  `MAGICIAN_APNS_TOPIC` overrides it for a custom bundle.
- **Credentials.** Custody, policy, redaction helpers and neutral session types
  come from `magicvault-core` behind the secrets facade (incl.
  `ProvisionedSecretMetadata`), pinned by git rev in the workspace
  `Cargo.toml` with `magicvault-effect`/`-protocol`/`-primitives` at the same
  rev; Magician keeps keychain identities, scope paths, signing/bootstrap,
  action traversal and browser/process ownership. No vault daemon or storage
  migration. Source-attested app approvals fail closed on
  changed implementation identity. Background:
  Phase 1,
  Phase 2.
- **Storage.** Profile `local_embedded`; remote adapters and the migration
  coordinator stay unselected at startup. Rollback invalidates forward evidence after
  `RollbackPending`. `SecretRef` accepts slash-separated `credentials_ref`. The
  skill-working envelope does not steal CLI CWD. JSONL commit markers stay on the
  file provider. See [storage-abstraction.md](storage-abstraction.md),
  [jsonl-durability](jsonl-durability.md).
- **Notes** live at
  `$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>`
  ([notes-provider.md](notes-provider.md)).
- **Execution driver.** `MAGICIAN_EXECUTION_DRIVER` defaults to `stateless`;
  `inprocess` is explicit rollback. Canary: `GET /health/execution-driver`. See
  [flat-loop](execution/FLAT_LOOP.md). Terminal evidence validation and deep
  stateless polls (inside the lease-heartbeat boundary) run under the per-poll
  stack-headroom wrapper without enlarging worker stacks or delaying renewals;
  evidence rejection stays strict; a segment never crosses a task spawn. Root pauses
  recover owner and review requirements after restart only from the
  authenticated terminal receipt; one-time grants and delegated authority are
  never recovered.
- **Chat mouth.** `chat.harness_engine` (default `magician`) selects who speaks;
  hands stay on the [plane](plane.md). Native-tool strip is per engine
  (`stripped` / `sandboxed`); each engine resumes its own native session per
  conversation; mouth tools are bridged. See [chat-mode.md](chat-mode.md).
- **Harness credentials.** `execution.harness_auth` decides whether each CLI
  runs on its own sign-in or Magician's provider keys; see
  [harness-auth.md](harness-auth.md).
- **Document tools.** Selection guidance prefers local document parsing for text,
  OCR for scan transcription, and image analysis for visual review; complete text
  and images already attached to a vision-capable model are reused.
- **Run engine.** `execution.harness_engine` (default `magician`); `pi`,
  `claude_code`, `codex`, `codex_app_server`, `grok` and `agy` replace
  decide+execute with one harness turn using the run's tool catalog (a pack's
  leaf actions are dispatchable when it is).
  - A harness run carries no token meter (`run_budget.rs`); each settled turn's
    CLI-reported `cost_usd` counts against
    `execution.budgets.harness_max_cost_usd` (default 100) plus owner grants,
    and a turn that reports no cost charges nothing. See
    [plane](plane.md#pi-harness).
  - Jev's step judge, the structured-decision gate and the rest of the decide
    phase do not run under a foreign harness.
  - agy runs with `--print-timeout 0` (its timeout exits 0 with partial
    output): Magician's ceiling and idle watchdog, not the CLI, bound the turn.
  - Framing is the store-managed prompt `harness_execution_preamble`
    (`data/magician_v2/prompts/`, `{identity_line}`); `rendered_prompt_or`
    returns the compiled fallback verbatim, so callers substitute placeholders.
    Goal, task id, environment knowledge and history are assembled in code.
- **Plane delegation tools.** `delegate_to_agent` / `find_agents_for_capability`
  / `get_agent_details` (`PLANE_DELEGATION_HOT`) appear only when the agent has a
  delegation target (as `flat_loop::catalog`); an operator overlay cannot add
  them back. The harness prompt names the agent and says when it is the worker.
- **Desktop.** `scripts/sync_desktop_action_enum.py` (`--check` for CI)
  generates the skill's `action_name` enum from the installed driver. The plane
  `http` tool names the relay (`127.0.0.1:3017/host/ax/…`) and points to the
  skill; the gateway logs every AX action (`host gateway AX action`, addressing
  fields only, never typed text).
- **Typed input.** Form-capable terminal MCP sessions get text, choices, forms
  and approvals via
  [plane typed input](plane.md#typed-input) (`request_user_input`,
  `wait_for_run`); [tests](../../testing.md#plane-typed-elicitation).
- **Auth.** General V2/V3 routes accept only workspace-bound session/API bearers;
  terminal grants stay on `/plane/mcp`. Open mode ends anonymous bootstrap when
  the first identity is created.
- **Spend.** Chat dispatch, Zepto/Swiggy checkout, REST reserve and
  compiled/OS-jail tool I/O share
  `spend_session::admit`; daily ceilings count live in-flight holds; YAML edits need
  a restart. See
  [quickstart §6](../../quickstart.md),
  [resource-authority-api.md](resource-authority-api.md).
- **OPC** is a caller of generic primitives, not an owner. Outward dispatches
  are recorded, not sent (`outward_actions.capture_only`, default `true`);
  `approval_envelopes.mode` defaults to `off`.
- **Process spawning.** Every spawn site that overrides `PATH` (skills runner,
  pack/compiled providers, CLI-template dispatcher, preflight probes, meeting
  calendar, verification runner, plane engines) resolves its program with
  `runtime_core::process::resolve_program` against that PATH; a bare name would
  make Rust `fork` instead of `posix_spawn` and can hang in macOS atfork
  handlers (port bound but never serving, a second `magician.bin` at 100% CPU). See
  `docs/components/runtime-core/runtime-core.md`.

## Apps runtime

- **Recurring behaviors** reuse one internal task with independently governed
  occurrences; scheduling waits for settlement; recovery, results and
  cancellation keep each occurrence's exact binding
  ([Recurring App tasks](recurring-app-tasks.md)).
- **Threat models.** Scheduled behaviors are interval-only, own-store-only,
  deny-by-default; each durable fire rechecks scope, generation, grant, kill
  switches, compiled effect ownership, content taint/provenance and resource
  ceilings before unattended work.
  See
  [background behaviors](app-background-behaviors-threat-model.md) and
  [events and owner notifications](app-events-owner-notifications-threat-model.md).
- **Meetings** (system-class app) uses the read-only six-action
  `meetings_data` binder and owner-signed `magician.meeting-control`: only first-party join/listen create captures; START needs a fresh
  gesture and is refused while a capture is live; controls are audited per
  scope. The watchdog allows 32 bridge messages per session. See
  [Meetings surface](meetings-surface.md), [App tool bind](app-tool-bind.md).
- **Runtime compatibility.** Recipe and compiled effect owners use stable
  reviewed contracts; [legacy baselines](app-runtime-compatibility.md) preserve
  immutable package locks.
- **Task creation recovery** restores pristine `internal` (and legacy
  `persistent`) App task shells only with the exact server-authored marker,
  scope and installation, pending state and no execution, output or published
  binding; a started or one-sided binding is never reconstructed. Owner retry of
  a background launch keeps the original fire and counters and re-enters
  admission (registry schema V34, `retry_requested` audit event). Startup clears
  a failed root's cleanup hint only if it never entered resource admission and
  has no effect/model/result evidence, registry root or ledger identity.
- **Resource cleanup** respects each node's recorded close time. An uncertain
  reservation keeps its full token/cost bound past terminal execution; crash,
  committed-store and no-effect recovery charge active time only within the
  node's journal-owned lifetime; startup recipe recovery settles in the same
  boot; a root whose deadline expired while down is terminalized directly with
  no fresh deadline.
- **Lock ordering.** Task before resource; App model dispatch releases the
  resource mutex before disclosure/recipe-claim callbacks (a non-owning clock
  bound to the root lease keeps admission time and detects release) and
  rechecks reservation expiry before provider I/O. Live admission samples time
  after taking the resource root lock, so a queued participant cannot append an
  older observation.
  Journal reads use one SQLCipher read transaction; contextual rounds match
  receipts by entity and record ID. Lane:
  `make test-app-runtime-concurrency-regressions`.
- **Recipe claim renewal** records request time before the task lock; a
  heartbeat requested within its lease renews only if owner and epoch match and
  cancellation has not won; the deadline is
  checked after the wait.
- **Stack/worker use.** Goal-trigger admission runs as an abort-on-drop
  execution job before creating Artifact/Runtime shells, so startup hydration,
  scheduler wakes and chat never poll it on their own stacks; workflow primitive
  discovery runs on the blocking pool.
- **Task projections.** Apps action runs use the Internal lifecycle; legacy
  runs keep their paths and are reclassified in list projections and index
  rebuilds (since list index schema 4; current `LIST_INDEX_SCHEMA_VERSION` 5). Installation lifecycle changes invalidate the
  discovery snapshot; each completed sweep's start is acknowledged, so changes
  behind an active cursor surface next sweep.
- **Deleted-task debt** is suppressed at terminal settlement only under a valid,
  deletion-lock-protected marker whose only survivors are known generated output
  paths (kept); anything else keeps the debt pending.

## Headline capabilities

Magician is a **runtime for autonomous personal-agent operations**:

- **Agents-as-data.**
  `magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`
  (persona, tools, delegation, memory tiers, prompt pipeline, constraints); see [`agent_templates/README.md`](../../../magician_data_v3/system/agent_templates/README.md),
  [Agent Definition Reference](agents/agent-definition-reference.md).
- **Self-improvement loop.** Extraction → retrieval → feedback → promotion →
  growth evaluation; see [Learning Procedures](learning-procedures.md).
- **Per-agent memory shape.** Typed memory tiers (schema, scope, retention,
  render format) over a fixed Parquet lakehouse + LanceDB hybrid index.
- **Drop-in skills.** `<name>/SKILL.md` in `skillshub/`;
  `make skills-install-scope SCOPE=<principal>/<workspace>` symlinks them in
  (edits are live) and validates. Discovery:
  `scope_loader::procedure_tool_infos_for_agent`. `tool_schema.yaml` only for
  unmigrated external packages. See [`skillshub/README.md`](../../../skillshub/README.md),
  [skills-spec](skills-spec.md), [skills-quickstart](skills-quickstart.md).
- **Channel adapters.** Implement the six-method
  `ChannelAdapter<TTarget, TContext>` in
  [`@magician/bot-sdk`](../../../skillshub/bots/sdk/README.md); each bot is a
  scope-aware daemon under `magic-supervisor`; enrollment-backed chat identity
  is active; control under `/api/magician/v2/bots/*`.
- **Capability tokens + double-entry ledger.** Every call carries a token;
  over-budget calls are refused.
  Every `CapabilityProvider::lower()` honours `execution.spend:` via
  `maybe_wrap_with_spend_gate`; dispatch goes through
  `execution::compiled_dispatch` ([Resource Authority](resource-authority-api.md)).
- **Generative UI (MUIJ).** Agents emit layout documents rendered as charts,
  cards, feeds and interactive surfaces. `/chat`, `/hud` and the `/t/[name]`
  Chat tab share one `ChatPanel`.
- **Delivery and retention.** `published_surface.changed` → `data_delivery`
  cards; `routine.result_published` → `routine:<task_id>` cards (`task_id` is
  provenance). Transient work is `lifecycle: internal`, swept with its chat
  session; `track_as_task` keeps it.
- **Continuation.** Completed tasks emit continuation-context outputs;
  follow-ups pass `reference_task_ids`; `get_task_details` returns continuation
  plus bounded previews.
- **Internal tasks.** `/tasks?type=internal` reads only `internal_tasks/<id>/`,
  recovers stale executions and projects HITL pauses into attention.
- **Out-of-process executor.** `magicutor` runs browser and heavy tools;
  `magic-supervisor` enforces restart policies.
- **Scoped event lakehouse** per `(principal, workspace)`, retained by the
  `transport_log` compactor:
  - Appends sharded by task, ≤16 concurrent, `write + flush + sync_all`;
    `events.jsonl.commit` is the durable byte boundary.
  - Handoff ≤4,096 events / 64 MiB; saturation backpressures, never
    drops; an oversized event goes alone after prior work drains.
  - Recent views 72 MiB; sequence recovery 128 MiB. Records over 64 MiB, past
    JSON depth/node ceilings or over 16,384 output refs fail closed.
  - Finalizers verify SHA-256/UTF-8; voice previews 32 KiB; cards read ≤256 KiB.
    Artifact V2 retries only `ENFILE`/`EMFILE` on scoped hot paths.
- **Thread deletion** (non-`#general`) tombstones it (kept for sync) and drops its
  sessions and memory.
- **Harness personal agents** (scoped CEO, CTO, CMO, CRO with focus-area
  goals) are a capability on
  `kind: personal`; structural changes are proposal-backed.
- **HITL.** `POST /api/magician/v2/hitl/{approval_id}/respond` with
  `source: "approval"`; `/approvals/{approval_id}/resolve` is `410 Gone`
  ([HITL](hitl-attention.md)).
- **Live config reload.** `POST /api/magician/v2/settings/magician-config/reload`
  refreshes the operation router, multi-LLM service, native-tool policy, tool
  authorization, complexity routing and Workbench CLI catalog; restart-only sections are reported.
- **`supervisor:` config section.** `supervisor.manage_desktop` (default
  `true`) is read by magic-supervisor at its start; Magician only declares the
  section (`SupervisorRuntimeConfig`) so the strict root accepts it, and
  ignores unknown keys inside it. The repo seed ships it commented out (absent
  = on) because Desktop seeds that file into data folders an older backend
  (which rejects unknown root sections) may read; deploy a backend that knows
  the key before adding it to a live config. See the
  [supervisor guide](../magic-supervisor/supervisor.md#desktop-app-macos).
- **Inner-loop packs** (`duckdb`, `imessage`) register twice (pack-defined, then
  `CapabilityRegistry::register_override` for compiled dispatch); a
  `[CAPABILITY] Overwriting existing provider` log flags an accident.
- **Container Messages reads.** Linux `imessage` without a local database uses
  the desktop's private `/host/imessage/query` relay (Mac opens it read-only
  with query/time/result limits; needs Full Disk Access for Magican); a macOS
  backend refused `chat.db` uses the loopback gateway with
  `<runtime root>/run/host-imessage.token`. See the
  container audit.
- **Remote desktop.** An enrolled desktop's outbound Edge socket carries
  bounded CUA actions, Magicutor/CDP health, target discovery, one-shot commands
  and read-only Messages queries; the desktop never publishes its loopback
  gateway, CDP URL, Messages path or device credential.
- **Governed stdio MCP.** Reuses SDK discovery/calls inside a scoped OS sandbox; reviewed `mcp_bindings` govern file paths, recipients, redaction and receipts. God-mode approval supports once/session/always while those fences remain enforced.
- **Remote MCP account setup.** The OAuth broker begins a governed binding with
  no browser on the engine; a setup-token-gated native client opens the URL;
  the tool declares whether callbacks use loopback or the engine's reviewed
  public origin; tokens stay in the engine's vault. See [browser sign-in](browser-signin.md).
- **Spatial surfaces.** The canvas lane is retired; spatial observation,
  coordinate-native browser actions, iframe reprojection and post-action
  evidence stay in the normal loop.

## DuckDB analytics

See [duckdb-analytics.md](duckdb-analytics.md).

- **Agent tool.** Compiled `DuckDbCapabilityProvider` (per-cycle in-memory
  session; CSV/JSON/Parquet + temp tables) for `simple-data-analyst` and `internal-system-analyst`. Pack:
  `magician/src/magician_v2/execution/embedded_pack_defs/duckdb.yaml`
  (`include_str!`; guide edits need a rebuild).
- **Internal analytics.** Schemaless `events` in
  `magician_data_v3/scopes/<principal>/<workspace>/analytics/analytics.duckdb`;
  `analytics::emit()` is fire-and-forget; tracing captures INFO/WARN/ERROR.
  Retention: last 24 h plus at least the newest 1000 per event type. `GET /api/magician/v2/analytics/schema`,
  `POST /api/magician/v2/analytics/query`.
- **Diagnostics.** Compiled `internal_data` (`catalog`, `schema`,
  `query_events`, `query_llm_calls`, `tail_logs`, task/execution listing,
  output/`events.jsonl` reads); `internal-system-analyst` uses it before
  `duckdb`; `personal-assistant` also has it.

Related skills: `sheets`, `rg`. Gmail/Sheets auth:
`gws auth login -s gmail,sheets,drive,docs,calendar`.

## Browser engines

CRO and Presentation Maker allow CDP for owner-authorized signed-in workflows,
alongside headed and headless browsing; agent templates carry the same grants.

Finished and cancelled runs close every browser session they used (only the
tabs a task created; paused runs and sibling leases keep theirs). Session ids
stay reserved until teardown succeeds, failed closes are retried, and an owner
handoff keeps the pages. Authenticated retrieval reads release their tabs too;
a failed cleanup keeps the gathered content (`session_outcome: cleanup_failed`).
Cancelling a run this process never saw browse closes its default session id
best effort, without retries or a lasting reservation.

**Cloak Browser** (`skillshub/cloak-browser`, stealth wrapper `0.5.3`, licensed
Chromium 150) sets `AGENT_BROWSER_EXECUTABLE_PATH` for `headless`/`headed`;
`cdp` mode (user Chrome via Magicutor) is unaffected.

- The CloakBrowser Python SDK is never used at runtime (Playwright leaks); only
  `skillshub/cloak-browser/scripts/resolve.py` imports it, returns a JSON envelope and never launches.
  Smoke: `skillshub/cloak-browser/scripts/smoke-test.sh`.
- An unresolvable explicitly configured engine fails the operation. Exit `76`
  (concurrent session) closes the daemon and retries once with bundled Chrome
  for Testing; `77`/`78`/`79`, crashes and page failures never do. No engine configured means bundled Chrome.
- CLI fork `agent-browser 0.41.0-Magician.0` (adds `webmcp`, `pushstate`,
  `vitals`); `AGENT_BROWSER_IDLE_TIMEOUT_MS=0`. Verify
  `make -C skillshub verify-agent-browser`; install
  `make -C skillshub setup-cloak-browser` → `~/.cloakbrowser/`
  (`CLOAKBROWSER_LICENSE_KEY` picks the build; never bundled). See
  [Browser Engine Observability](browser-engine-observability.md).

**Lightpanda** (`skillshub/lightpanda`) runs via `agent-browser` with
`AGENT_BROWSER_ENGINE=lightpanda` (`lightpanda agent` is never invoked). `content_acquisition.browser.public_read_engine: lightpanda`
prefers it for isolated public headless reads; failures close the session and
fall back to `content_acquisition.browser.engine` (`cloak-browser` shipped) then
bundled Chrome; authenticated and side-effecting work is never replayed. Headed
work, screenshots/PDF/recording, visual grounding, profiles, extensions,
authenticated state, files, meetings, previews and fingerprint-sensitive sites
stay off it; pixel commands fail clearly.
`make -C skillshub setup-lightpanda` / `verify-lightpanda`;
`LIGHTPANDA_EXECUTABLE_PATH`; telemetry and crash dumps off.

## Username OSINT tool

`skillshub/find-details-by-username` wraps Maigret (`maigret==0.6.1`, `--ai`
off) as `findDetailsByUsername` for Vera (`executive-assistant`), Aide
(`company-assistant`) and Sleuth (`web-researcher-opc`). Reports go to
`<scope>/workdirs/find-details-by-username/<query>-<timestamp>` when
`MAGICIAN_SKILL_DIR` is set. Matches are public leads, not verified identity.

## Adding a capability pack

See [skills-spec](skills-spec.md) and the
[Pack Authoring Guide](../capabilities/authoring-guide.md). Sources, earlier
wins on collision:

1. `<storage_root>/scopes/<principal>/<workspace>/skills/<skill>/SKILL.md`
   (`make skills-install-scope`).
2. `magician/src/magician_v2/execution/embedded_pack_defs/<name>.yaml`.

`magician_data_v3/system/capability_templates/packs/` and
`<scope>/capabilities/packs/` are decommissioned. Packs list via
`GET /api/magician/v2/skills` (compiled as `kind: 'compiled'`,
`layer: 'built-in'`); `/api/magician/v2/capabilities/packs` is retired. Validate:
`make test-capabilities`.

### Parameter schemas

Each `parameters:` entry resolves to a canonical JSON Schema the catalog emits
verbatim. Scalars
(`param_type: string|integer|boolean`, optional `enum_values:`) desugar;
arrays, objects, regex, formats and `oneOf` need `schema:`:

```yaml
- name: evidence_refs
  required: true
  schema:
    type: array
    items: { type: string }
    minItems: 1
```

Catalogs read it via `derive_param_schema_for_emission`. A leaf exposes only
its action's `parameters` (`parameter_overrides` wins, else the pack schema's
types, enums and nested constraints).
Grant/denial rules cover a pack's `<pack>__<action>` leaves; an exact leaf
denial removes only that leaf from deferred discovery, loaded tools and eager
catalogs while siblings remain; unrelated prefixes do not match. Sealed
agents grant discovery and task controls explicitly; Ambassador's company-only catalog includes `tool_search`, `yield` and
`need_user_input` without the universal rail.

## Canonical references

- Accounts: [Browser sign-in across devices](browser-signin.md)
- Runtime / API: [V2 API Guide](v2-api-guide.md),
  [V3 Complete Flow](v3-complete-flow.md),
  [V2 WebSocket Events](v2-websocket-events.md),
  [Captured Auth For API Replay](auth-capture-and-replay.md),
  [API Explorer](api-explorer.md), [API Mining Pipeline](api-mining-pipeline.md),
  [API Mining Projections](api-mining-projections.md),
  changelog `v0.6.430` is a UI-paired no-op (see
  `docs/components/unified-ui/README.md`)
- Apps: [Town Square app](town-square-app.md),
  [Stranded app workflow tasks](app-workflow-stranded-tasks.md)
- Chat / media: [Chat Profile Routing](chat-profile-routing.md),
  [Chat SSE Streaming](chat-sse-streaming.md),
  [Realtime Media Rails](realtime-media-rails.md),
  [FluidAudio Phase 9](fluid-audio-phase9-rollout.md),
  [Offline Audio Evals](fluid-audio-offline-evals.md),
  [Personal Tutor](personal-tutor.md)
- Storage / memory / notes: [Storage Governance](storage-governance.md),
  [Storage V2 Format](storage-v2-format.md),
  [Workspace File Provider](workspace-file-provider.md),
  [Taste Profile](taste-profile.md), [Memory Index](memory-index.md),
  [Memory Retrieval Evals](memory-evals.md),
  [Vector Toolkit](vector-toolkit.md),
  [Ollama Logical-context Chunking](ollama-logical-context-chunking.md),
  [Durable Artifact Tag Index](execution/DURABLE_ARTIFACT_TAG_INDEX.md)
- Agents / skills / learning: [Agent Updates](agent-updates.md),
  [Agent-Owned Identities](agent-owned-identities.md) (`self_identity` tier;
  the agent's own address is never an owner),
  [Trust-Tiered Agent Task Dispatch](agent-task-dispatch.md),
  [Skills Authoring](skills-authoring.md), [Plan Flow](plan-flow.md),
  [Work Evidence Graph](work-evidence-graph.md)
- Surfaces / channels: [Today Feed](today-feed.md),
  [Content Sources](content-sources.md),
  [Town Square Fleet-State API](fleet-state-api.md),
  [Fleet Social Network](fleet-social-network.md) (superseded; kept for the
  `social:` config block), [Crew Health](crew-health.md),
  [Consumer Channels](../../consumer_channels.md),
  [File Context Carry-through](file-context-carry-through.md),
  [Developer Mode Workbench](developer-mode-workbench.md),
  [CLI Delegate Ecosystem](cli-delegate-ecosystem.md),
  [Local Resource Governor](local-resource-governor.md)
- Execution: [Execution Docs](execution/README.md),
  [Execution Archive](execution/archive/README.md),
  Research, Plans
- Workspace: [Architecture V2](../../ARCHITECTURE_V2.md),
  [Capabilities](../capabilities/README.md)

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

## Scope auth and review evidence

- **Scope `auth/`** directories are owner-only (`0700`); bootstrap repairs
  legacy ones without following a leaf symlink. OAuth readiness requires a
  configured, identity-checked profile.
- **Final review** reads accepted HITL answers (collected-input redaction) and
  failed outcomes through the
  indexed tool-evidence path; protected app pauses keep their labelled path;
  failed receipts never establish external success.
- **Governed CLI auth** binds the reviewed package entrypoint, not per-action
  executables (for the pinned SDK's first-bin planner); install/PATH review and
  auth, injection and policy fields are unchanged.
- **Claim review** adds exact tool-result values as labelled evidence beside
  keyword/semantic matches, so related reports cannot crowd out a recorded
  reference or owner answer; owner input never proves an external effect and
  acceptance rules are not bypassed. **Claim grounding** applies its numeric
  boundary to hyphenated references, still checking genuine negatives; a missing
  figure blocks even model-supported text and reports the exact quantity.
