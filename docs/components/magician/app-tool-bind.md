# App tool bind + contain

Apps may lock and grant the same tool names agents use. That is not enough
to run them. Dispatch is:

```
grant → refuse divergent operation aliases → classify IO from pack/skill shape
     → require a wired binder → require a wired contain profile
     → bind params (close the surface) → prove arguments → lower
     → RE-CLASSIFY from the lowered action and refuse disagreement
     → mint a one-shot receipt → execute → label
```

Module: `magician/src/magician_v2/apps/app_tool_bind.rs`.

## The call must mean one thing

A call names its operation **once**. `operation`, `__action_name`, `action` and
`method` can each name it; `divergent_operation_alias` refuses any two that
disagree (`{operation: "get", method: "DELETE"}` would otherwise classify as a
read and delete). Refused, never normalised — picking a winner runs a call the
app did not write. (`http_get` and `get` are the same operation.)

Classification happens **twice, from two representations**: `classify_io` from
the parameter map before dispatch, and `classify_lowered_action` from the
lowered `ExecutableAction` after. Disagreement is refused in
`prepare_attested_local_compiled_dispatch` and again in
`attest_app_tool_target`, so a receipt never vouches for a call whose real effect
differs. (`app_compiled_action_matches_tool` proves only the action kind.)

`classify_lowered_action` answers `None` for `DuckDb` and `Pack`, whose lowered
forms do not settle the class (`DuckDbAction` is an opaque `{ sql }`). Those are
contained by closing the parameter surface: duckdb **refuses free-form `select`
/ `where_clause`** and **forces `__action_name` to the planned operation**; files
**seeds `action` from the planned operation**. These refusals are app-path only;
agents keep raw `duckdb.query`.

Execution is least-privilege: `allow_delete` only when the lowered action is a
delete, and an HTTP header denylist (`x-http-method-override`, `x-http-method`,
`x-method-override`, `host`, `transfer-encoding`, `content-length`) refuses
headers that change which request is performed or where it lands.

**Alias parity is load-bearing.** `lowering::canonical_file_tool` accepts several
spellings (`remove`→delete, `rename`→move, `create_dir`/`create_directory`→mkdir);
`classify_file_operation` must carry the same set, or a mutating alias classifies
as a runnable read. `every_mutating_file_alias_the_executor_accepts_classifies_as_a_write`
pins it; add to both sides together.

- The classifier and executor **default opposite ways for duckdb**
  (`classify_duckdb_operation` assumes `preview`; `lower_duckdb_action` assumes
  `query`). The executor default is right for agents, so the app path refuses to
  emit a duckdb call with no `__action_name`. Do not remove that check.
- Refusals are **reported, not flattened**: the executor carries the specific
  reason out of `prepare_attested_local_compiled_dispatch`.

Classification is **schema-driven**: it reads the compiled pack
(`execution.sandbox`, `categories`, `composition_category`, parameter names) or
the skill `runtime_catalog` / `runtime_contract`, never a per-tool name allowlist
or `list_`/`search_` naming family.

Classification is class-level; runnability is an explicit physical-owner
decision. `CapabilityProvider::prove_app_tool_args` defaults to false; an
in-process provider must opt into exact argument proof and a provider-owned
implementation identity. `app_effect_owner_supported` is the single predicate
used by descriptor projection, installation review, workflow admission and the
executor. YAML categories alone never make a provider app-runnable.

The pure-transform compiled allow-list is only `time_math`; its
implementation-plan digest binds the immutable pack, action plan, provider
identity and build-time bytes of the provider/lowering modules (portable across
equivalent builds; a source change forces re-review). `catchup_merge` stays
blocked: it emits runtime duration bytes while claiming a pure transform and its
O(n²) clustering lacks hard input/work ceilings.

`dependencies.tools[].actions` may narrow a dependency to exact action names or
content-addressed refs; the lock persists only that subset and dispatch requires
membership. An omitted/empty selector locks all descriptor actions, and review
says so explicitly; a nonempty selector is never silently expanded.

## What is not per-tool Rust

A new skill or pack of an **existing** IO class reuses the class:

| IO class | How it is recognized | Wired today | Result label |
|---|---|---|---|
| Pure transform | `time_math` `date_range`; category `merge` (`catchup_merge`) | `time_math` in-process; authority-free reviewed CLI/USR actions through the OS-jail owner | inherit input |
| Trusted local clock | `time_math` `now` | `time_math` only | join as public fact |
| Bound HTTP | `url` / `candidate.url` / `requests[].url`; categories `http`/`api`; non-commerce MCP. | exact built-in `http.get` only, with DNS/connect-IP/Host/SNI retained through every hop; non-commerce MCP product actions (`status`, `list_tools`, …) through the governed-MCP owner | introduced remote content |
| Bound file | Closed files pack only: `sandbox: file` or category `files`. | exact built-in `files.read` only; a reviewed root and no-follow component/file descriptors survive through the bounded UTF-8 read | introduced local content |
| Bound write | same files pack + write/append/delete/copy/move/mkdir. | exact built-in `files.write` only; create-new and compare-exchange replace publish a bounded fsynced temporary file atomically | local effect result |
| Bound table | structured DuckDB only. | exact built-in `duckdb.preview` and `duckdb.describe` for one bounded CSV/JSON/Parquet descriptor; provider receives only `/dev/fd` or `/proc/self/fd` | introduced local content |
| Bound host-read | composition `read`/`observation`, or categories `introspection`/`evidence`. The tool name is not consulted. | the action-exact scoped binders below; every other host-read tool remains classified, not wired | introduced local content |
| Bound side-effect | remaining compiled packs; commerce/messaging/media skills; HTTP POST/PUT/PATCH/DELETE | commerce MCP (`protocol: mcp` + commerce categories) through the governed-MCP owner (`status` / `auth_start` / `list_tools` / `call_tool` / `clear_auth`); other side-effect skills stay classified, not wired | — |
| Device | categories `macos`/`android`/`screenshot`/`meeting`/`ui_automation`/`desktop_operations`/`device`. The tool name is not consulted. | classified, not wired | — |
| Unbound | `sandbox: shell`, open DuckDB `query`/`export`/`attach`, unknown skill shape | refused | — |

## Operation splits

- `http` GET → Ready Bound HTTP; HEAD and mutation methods blocked.
- `files` read → Bound file; write → Bound write. List, exists, append, delete,
  copy, move, mkdir, `grep`, `glob` blocked.
- `duckdb` preview/describe of one exact relative file → Bound table;
  list_tables, `read_parquet` globs, query, export, attach, persistent databases,
  session tables and SQL fragments blocked.
- Host-read packs expose only their named actions; every other operation of the
  pack classifies `Unbound`, and an operation-less call defaults to the pack's
  narrowest bounded read, never a broad default like `catalog`.

## Contain profiles

| Profile | Who | Wired today |
|---|---|---|
| In-process compiled | Explicit provider opt-ins | `time_math`, exact `http.get`, `files.read`, `files.write`, `duckdb.preview`, `duckdb.describe`, the two `internal_data` learning review reads, and the scoped host-read binders (`thinking_maps_data`, `evidence_data`, `meetings_data`, `agent_roster_data`, `tasks_data`, `notes_data`, `memory_data`) |
| OS jail | skillshub / CLI / USR | authority-free PureTransform actions with exact reviewed source, lock, private artifact and finite transport bounds; Bound HTTP skills that declare egress hosts (`metadata.magician.app_egress`) and in-place skills run in the brokered-egress jail — see [app-os-jail-egress](app-os-jail-egress.md) |
| Governed MCP | skillshub official-SDK MCP | Zepto/Swiggy (and other `protocol: mcp` skills) through `dispatch_governed_mcp`. Checkout INR uses the same `spend_session::admit` writer as chat. Not OS-jail. |

### Governed MCP runtime seam

`apps::governed_mcp` is the app effect owner for reviewed `protocol: mcp`
ToolSkills (no jailed child). Prepare attests exact source, projected product
actions and finite transport ceilings; execute calls the same
`dispatch_governed_mcp` chat uses, so OAuth, live discovery, HITL, cart
cross-check and INR `spend_session::admit` stay inside it. Do not attach
pack/skill `spend_gate` on this owner (double reservation). The dispatcher fails
closed without principal, workspace or agent identity.

Once checkout may have committed, the owner returns a non-retry `Ok` rather than
failing: a remote success larger than the 64 KiB projection becomes a non-retry
stub; completion-intent sidecar failure still returns the observed result
(recovery uses the labeled checkpoint, never re-dispatch); the same covers
`InputRequired` / `Task` after send, ambiguous transport, a stub still over a
tiny ceiling, and settlement/labeling faults after `CommitObserved`. Labeled
recovery without a completion intent requires `disclosure_digest` to match the
attempt's canonical input. Locks that stored an MCP skill as OS-jail or blocked
are refused at review (contain-profile mismatch) until republished.

### OS-jail runtime seam

`tool-runtime-core::governed_process_jail` and `apps::os_jail` are the
non-public containment owner. ToolSkill dispatch is ready for the authority-free
PureTransform subset:

- Admission binds reviewed skill bytes, locked action/schema digest and typed
  lowering; caller executable, host path, env, raw argv, cwd override and
  runtime-control input are refused.
- The governed executor resolves, hashes, snapshots and revalidates the exact
  executable, rebuilds a finite child env, materializes declared credentials,
  bounds stdout/stderr, kills/reaps the process group, redacts output and writes
  the execution audit.
- One canonical `0700` invocation directory, removed on every path; any other
  cwd is refused.
- macOS: deny-default `sandbox-exec` (no Mach lookup, no fork, literal-only exec
  of the snapshot, read-only runtime roots, writes only in the invocation dir).
  Linux: bubblewrap with all namespaces unshared, no host command dirs,
  read-only loader/library roots, snapshot at `/app`, only `/work` writable.
  Missing launchers or unsupported hosts fail closed.
- Direct network and ambient env are denied; networked skills need the mediated
  egress owner ([app-os-jail-egress](app-os-jail-egress.md)).
- Authority-free: ordinary base policy and empty additive approval, grant,
  resource-scope and resource-authority sets per action, so skill metadata
  cannot create authority.
- `runtime.limits.stdout_bytes` and `stderr_bytes` must be authored; their sum,
  worst-case JSON expansion and envelope must fit the 56 KiB raw-result lane
  (under the 64 KiB labeled-result limit) and are retained in descriptor, lock,
  plan digest and physical target. Oversized contracts stay discoverable but
  blocked; the 16 MiB jail hard stop is not app policy.
- CPU, RSS/address-space, wall, process, open-file, file-size and output
  ceilings; Linux `RLIMIT_NPROC`, macOS no-fork. A lost observation after spawn
  is effect-uncertain unless exit/reap proves completion.
- Artifact hashing, reopening and lowering run on the four-worker bounded
  blocking pool before task/resource guards; the final fence reattests source,
  lock, action, plan and target; the executor verifies the BLAKE3 digest from the
  opened descriptor just before snapshot and launch. Review verifies one locked
  action at a time.
- The workflow reserves one of four process-wide blocking slots and waits until
  that worker runs before taking final guards and recording dispatch-start (30 s
  deadlines for capacity and handshake; unlaunched slots expire after 30 s). The
  move-only action and one-shot provider authorization are handed to the waiting
  worker synchronously.
- Redacted stdout parses as bounded JSON or UTF-8 text (JSON-looking bytes cannot
  fall back to text). Labels come only from the common effect/disclosure owner.
- The governed audit receipt records jail platform, guarantees, ceilings and
  executable provenance. The workflow seals the canonical receipt, digest,
  invocation/effect correlation, disposition and result digest in the run-state
  sidecar with the completion intent (uncertain failures store correlation and
  any receipt; worker loss records receipt absence). Recovery refuses legacy or
  digest-only completions without a matching sealed audit record.
- Every post-start worker loss, cancellation without an observed terminal result,
  or unpersistable result is uncertain; only pre-I/O abort proof releases the
  reservation. Descriptor and lock share one implementation-plan digest covering
  source identity, jail-profile versions, input schema, argv mapping, policy and
  transport ceiling.

## Receipts

`AttestedAppToolTarget` is physical target evidence, not launch authority. The
move-only `AppEffectPermit` binds installation/grant/package lock, schema,
primitive/action/implementation digests, physical target, invocation and
canonical input. The workflow holds task then resource-root guards across the
durable `EffectDispatchStarted` checkpoint and first provider poll. Before the
checkpoint, cancellation releases proven-unspent; after it, lost responses,
expiry or cancellation settle outcome-uncertain.

The resource `Settled` event keeps the effect-binding digest plus the raw
`ActionResult` digest and byte length (content stays with disclosure/retention),
so a crash before the labeled-result checkpoint cannot detach result from effect.

Bound HTTP mints `from_trusted_external_dispatcher` with `destination:{host}`
(disclosure still requires that destination on the grant). Bound file/table mint
`from_trusted_local_dispatcher` only from the move-only capability-directory
owner, whose target binds reviewed root and device/inode/metadata, normalized
relative path, operation/format, byte ceilings and atomic publish semantics,
re-walked no-follow at the final fence and held through I/O. Introduced content
joins Ordinary / RemoteAllowed with the session (which can only raise
classification or narrow processing).

## Scoped host-read binders

Each host-read binder is action-exact, has its own pack identity, implementation
identity and runtime ref (so changing one never rotates another installed lock),
and shares these properties:

- **Explicit classification.** `classify_io` names exactly the binder's
  operations and returns `Unbound` for everything else in the pack; the receipt
  mint is name-guarded. Where a pack has no defaultable target, the call must
  name one. A compile-time assertion in newer providers refuses mutating action
  names.
- **Runtime-owned scope.** The executor strips model-origin `__*` keys and
  injects `__principal`/`__workspace` from the authenticated engagement; the
  provider derives the store scope from those alone (public `principal` /
  `workspace` fields are equality assertions). Path components are scope-id
  validated.
- **Closed argument proof** and `bind_parameters_for_call` re-seeding
  `__action_name` from the classified operation.
- **Witness.** The builtin witness is minted only when the loaded definition is
  the embedded one (a disk or scope override stays unwitnessed even with equal
  bytes).
- **Label.** `from_trusted_local_dispatcher` (`IntroducesLocalContent`) — the
  conservative floor. Oversized results fail closed at settlement rather than
  truncating mid-record.
- No binder admits mutation; widening one is a reviewed kernel change with its
  own argument proof, not a YAML edit.

### The first host-read binder: scoped learning-substrate reads (plan 2.5)

`internal_data`: `list_learning_candidates` (optional closed candidate state,
limit ≤ 25; operation-less default) and `read_learning_candidate` (one id
≤ 128 bytes). The pack's diagnostics vocabulary (catalog, SQL, telemetry, logs,
audio notes, procedures) stays agent-only. `authorize_runtime_scope` refuses an
unscoped call (unlike later binders, it retains a default-scope fallback for
missing injected scope). Ceilings: input 2 KiB, result 512 KiB. The consuming
package declares `classification_floor: personal` /
`model_processing: local_only`. Learning-candidate transitions stay on the
owner-gated first-party API.

### The second host-read binder: scoped thinking-map reads (Phase 4 Brainstorm re-open)

`thinking_maps_data` (identity
`magician.compiled-provider.thinking-maps-read.v1`): `list_maps` (optional
lifecycle `active`/`paused`/`archived`/`deleted`, limit ≤ 25; default) and
`read_map` (one id ≤ 128 bytes). Mutation (create, apply, patch, delete,
interpret, consolidate) is absent and stays on `/thinking-maps`. The provider
reads the same durable artifacts as the REST handlers
(`scopes/<p>/<w>/thinking_maps/<map_id>/{manifest.json, snapshot.json}`) with the
store's own types (`magician-surfaces` re-exports `thinking_map_models`) and never
writes. Corrupt manifests are skipped; unknown lifecycle filters error; missing
injected scope fails closed. Ceilings 2 KiB / 512 KiB.

`list_maps` bounds scan work: names are filtered (unsafe dropped first) and
sorted; only a window of `LIST_MAPS_SCAN_BUDGET` (2048) manifests is read; an
over-budget window adds `scan_truncated: true` and `next_cursor`. Optional
`after_map_id` is an order-based keyset cursor (a deleted map still resumes).
Pack version 1.1.0 (additive; `^1.0` accepts it). Packages narrow to
`actions: [list_maps, read_map]`; the Brainstorm package
(`magician_data_v3/system/thinking_map/app`, `brainstorm-canvas`) uses them from
`sync_maps`, and `surfaces/canvas.html` reads through the bridge
([custom-surfaces-v1](custom-surfaces-v1.md)).

### The evidence-data host-read binder

`evidence_data` (identity `magician.compiled-provider.evidence-data-read.v1`,
ref `runtime:compiled:evidence-data-read:v1`, input 8 KiB, result 512 KiB, 30 s
timeout, result ceiling also enforced in the provider): `list_pending_claims`
(default), `read_claim`, `list_evidence_records`, `list_entities`,
`list_commitments`. Confirmation, rejection, transcript ingestion, commitment
recording, evidence correction and entity mutation are absent.

- Evidence and entity reads need an explicit validated `agent_id`; the binder
  never unions a workspace's agents.
- `audience_kind` + `audience_id` are both-or-neither for claims; commitments
  need both and read one audience shard (the full enumerator folds every hashed
  shard, so its I/O could not be bounded).
- Evidence loads only via `AgentMemoryService::load_scoped_evidence`, keeping its
  sensitive-record suppression. Tombstones are excluded before projection;
  duplicate evidence ids across lanes fail closed. Entities are rebuilt only from
  that suppressed projection and expose normalized identity, display and
  first/last-seen only.
- Projections, not exports: claims expose words, speaker, audience, status,
  extraction time, id, revision (detail adds extractor and bounded utterance
  keys); evidence exposes id, status, summary, first-seen; commitments omit
  source and supersession internals. Provenance, actions, artifacts, facets,
  keys, scores, sensitivity and producer metadata never cross.
- Lists order by timestamp + unique id; cursors name the prior boundary id
  resolved in the current view; limits ≤ 50; claim text filters ≤ 512 bytes;
  fixed scan windows report `scan_truncated` + `next_cursor`.

### The meetings-data host-read binder

`meetings_data` (identity `magician.compiled-provider.meetings-data-read.v1`,
ref `runtime:compiled:meetings-data-read:v1`, input 8 KiB, result 512 KiB, 30 s
timeout): `active_session` (default; narrowest, no target), `list_threads`,
`read_thread`, `read_takeaways`, `upcoming_meetings`, `search_meeting_memory`.
Capture control is a separate owner-signed destination; `classify_io` refuses
`listen`/`join`/`pause`/`resume`/`stop` by name.

- Threads and transcripts resolve through the scope's chat index, takeaways
  through the scoped memory service, upcoming meetings through the scope's
  capability auth root. `active_session` is the one process-global read
  (capture state must never be hidden from the operator) and projects less than
  `GET /meetings/active` (no rolling summary).
- Thread ids must have the `meeting-` prefix (else it is a general chat reader);
  an explicit `session_id` must belong to the thread; text filters ≥ 2 chars;
  limits ≤ 50; `read_thread` and `search_meeting_memory` need a target.
- `list_threads` resolves `after_thread_id` to the current
  `(created_at, thread_id)` order and refuses unknown cursors; `read_thread`
  uses `ChatStore::get_messages_before_exact` (the forgiving paginator's
  unknown-cursor fallback would loop a pager).
- Only plain-text turns cross; tool cards, attachments and task-status rows do
  not. Keyword search covers transcript bodies, takeaways and titles under fixed
  session/message budgets reported as `scan_truncated`.

### The tasks-data host-read binder

`tasks_data` (identity `magician.compiled-provider.tasks-read.v1`, ref
`runtime:compiled:tasks-read:v1`, input 4 KiB, result 512 KiB, 30 s timeout,
`ReviewedScopedHostRead` receipt): `list_tasks {status?, limit?, after?}`
(default) and `read_task {task_id}`. Agent tools `list_tasks` /
`get_task_details` have no app identity; this is the narrow app face.

- Projection: id, title, status, owning agent, priority, due date, tag names,
  created/updated, `is_blocked`, `awaiting_owner` (a bit, not the question),
  completion outcome; `read_task` adds description and completion summary
  (≤ 4 KiB each). Plans, executions, chats, schedules and routing never cross.
- Both read via `V3ReadApi::list_tasks` (hides non-user-visible tasks, so a read
  cannot reveal what the list hides) and never take the task write guard
  (`get_task` would trigger write-replay recovery).
- Newest first, id tie-break; `next_cursor` is `<created_at>|<task_id>` and stays
  valid if the task is removed.
- The process-wide registry has no artifact service at boot and fails calls
  loudly; per-scope registries used by apps attach it.

### The notes-data host-read binder

`notes_data` (identity `magician.compiled-provider.notes-read.v1`, ref
`runtime:compiled:notes-read:v1`, input 4 KiB, result 768 KiB, 30 s timeout,
`ReviewedScopedHostRead` receipt): `search_notes {query, limit?, provider?}` and
`read_note {source_ref}`, both needing a target. Agent tools `search_notes` /
`open_note` stay agent-only (`open_note` returns absolute host paths).

- `read_note` takes a hit's `source_ref`
  (`notes:<local_markdown|silverbullet>:<relative markdown path>`); the proof
  checks provider and `is_readable_note_path` (relative, no parent escape,
  markdown), and `NotesSettingsStore::read_observation_note` re-confines to the
  canonical provider root and refuses symlinks. Search uses Observe's
  boundary-safe roots; `open_url` is dropped (it can carry host paths).
- Queries 2–256 chars; ≤ 50 hits, ≤ 8 matched lines each; note body ≤ 256 KiB;
  `scan_truncated` / `more_available` pass through.

### The memory-data host-read binder

`memory_data` (identity `magician.compiled-provider.memory-read.v1`, ref
`runtime:compiled:memory-read:v1`, input 4 KiB, result 512 KiB, 30 s timeout,
`ReviewedScopedHostRead` receipt): owner-granted memory reads, grant model in
[app-memory-access](app-memory-access.md). Actions `search_memory {query,
limit?}` and `read_entry {key}`; callers cannot pick tiers.

- The executor stamps `__app_installation_id` and `__app_run_mode` from the
  task's app workflow binding (`AppWorkflowService::app_memory_read_context`)
  after stripping model `__*` keys; without them the call is refused, so the
  binder is unusable outside an app workflow.
- The effective grant is re-read from the scope's app registry on every call.
- Per candidate: engagement- and meeting-labelled memory is dropped; app-sourced
  records are kept only if every source is this installation and still eligible;
  owner memory only if its tier (user scope) or agent (agent/agent-goal scope) is
  selected for this run mode; superseded records dropped via lifecycle and a
  read-only temperature snapshot.
- Truly read-only: never syncs the temperature overlay or records retrieval
  usage. Results carry an opaque key, scope, tier, agent, bounded value,
  timestamps and origin (`granted` / `own_app`); `read_entry` answers
  `present: false` for both gone and not-granted.

## Agents, procedures, personalities

Not on this pipeline: the grant selects the runner or injects instructions; tools
those runners call still go through bind + contain.

## Adding something

- New HTTP-search skill: Bound HTTP if it declares `search`/`research` (or a
  `url` parameter) — a `-search` name is not enough. To run in apps it declares
  its HTTPS hosts under `metadata.magician.app_egress` and the app is granted
  `destination:<host>`; no kernel change.
- New compiled pack with a `url` parameter or `http`/`web` categories: Bound
  HTTP, no kernel change.
- New merge/math compiled pack: PureTransform, no kernel change.
- First side-effect or device binder, or a new host-read binder: a reviewed
  kernel change with its own argument proof and implementation identity.
- Open SQL, host paths, raw argv, `sandbox: shell`: stay unbound.

See [app-authoring-cli](app-authoring-cli.md) and
[app-platform-contract-kernel](app-platform-contract-kernel.md).
