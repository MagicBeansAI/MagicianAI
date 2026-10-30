# App tool bind + contain

A grant alone runs nothing. Dispatch
(`magician/src/magician_v2/apps/app_tool_bind.rs`):

```
grant → refuse divergent operation aliases → classify IO from pack/skill shape
     → require a wired binder → require a wired contain profile
     → bind params (close the surface) → prove arguments → lower
     → RE-CLASSIFY from the lowered action and refuse disagreement
     → mint a one-shot receipt → execute → label
```

## The call must mean one thing

- **One operation name.** `operation`, `__action_name`, `action` and `method`
  may each name it; `divergent_operation_alias` refuses disagreement
  (`{operation: "get", method: "DELETE"}`), never normalises. `http_get` = `get`.
- **Classified twice.** `classify_io` (params) and `classify_lowered_action`
  (`ExecutableAction`) must agree, in both
  `prepare_attested_local_compiled_dispatch` and `attest_app_tool_target`, so a
  receipt never vouches for a different real effect.
  `app_compiled_action_matches_tool` proves only the action kind. Refusals are
  reported with their specific reason, not flattened.
- **Closed surface where lowering is opaque.** `classify_lowered_action` returns
  `None` for `DuckDb` and `Pack` (`DuckDbAction` is an opaque `{ sql }`). App
  duckdb calls refuse free-form `select` / `where_clause` and a missing
  `__action_name`, which is forced to the planned operation
  (`classify_duckdb_operation` defaults to `preview`, `lower_duckdb_action` to
  `query`: keep the check); files seeds `action` likewise. Agents keep raw
  `duckdb.query`.
- **Alias parity.** `classify_file_operation` must accept every spelling
  `lowering::canonical_file_tool` does (`remove`→delete, `rename`→move,
  `create_dir`/`create_directory`→mkdir), or a mutating alias classifies as a
  runnable read; pinned by
  `every_mutating_file_alias_the_executor_accepts_classifies_as_a_write`. Add to
  both sides together.
- **Least privilege.** `allow_delete` only for a lowered delete; headers
  `x-http-method-override`, `x-http-method`, `x-method-override`, `host`,
  `transfer-encoding`, `content-length` are refused (bound HTTP also refuses
  `proxy-authorization` and `proxy-connection`).

**Schema-driven, owner-gated.** Classification reads the compiled pack
(`execution.sandbox`, `categories`, `composition_category`, parameter names) or
skill `runtime_catalog` / `runtime_contract`, never tool names or a
`list_`/`search_` family. Providers opt in via
`CapabilityProvider::prove_app_tool_args` (default false) with exact argument
proof and their own implementation identity; `app_effect_owner_supported` is
the single runnability predicate (descriptor projection, installation review,
workflow admission, executor). YAML categories never suffice.

Pure-transform compiled allow-list: only `time_math`, whose implementation-plan
digest binds the immutable pack, action plan, provider identity and the
provider/lowering build bytes (portable across equivalent builds; source change
→ re-review).
`catchup_merge` stays blocked (runtime duration bytes in a "pure" result;
unbounded O(n²) clustering).

`dependencies.tools[].actions` narrows a lock to exact names or
content-addressed refs, enforced at dispatch; omitted or empty locks all
actions (review says so); a nonempty selector never expands.

## What is not per-tool Rust

New skills or packs reuse an existing IO class:

| IO class | Recognized by | Wired today | Result label |
|---|---|---|---|
| Pure transform | `time_math` `date_range`; category `merge` (`catchup_merge`) | `time_math`; authority-free reviewed CLI/USR actions via OS jail | inherit input |
| Trusted local clock | `time_math` `now` | `time_math` only | join as public fact |
| Bound HTTP | `url` / `candidate.url` / `requests[].url`; categories `http`/`api`; non-commerce MCP | exact built-in `http.get` (DNS/connect-IP/Host/SNI held every hop); non-commerce MCP actions (`status`, `list_tools`, …) via governed MCP | introduced remote content |
| Bound file | files pack: `sandbox: file` or category `files` | `files.read` (reviewed root, no-follow, bounded UTF-8) | introduced local content |
| Bound write | files pack write/append/delete/copy/move/mkdir | `files.write` (create-new or compare-exchange, atomic fsynced publish) | local effect result |
| Bound table | structured DuckDB | `duckdb.preview`, `duckdb.describe`, one bounded CSV/JSON/Parquet via `/dev/fd` or `/proc/self/fd` | introduced local content |
| Bound host-read | composition `read`/`observation`; categories `introspection`/`evidence` | scoped binders below | introduced local content |
| Bound side-effect | other compiled packs; commerce/messaging/media skills; HTTP POST/PUT/PATCH/DELETE | commerce MCP (`protocol: mcp` + commerce categories) via governed MCP: `status` / `auth_start` / `list_tools` / `call_tool` / `clear_auth` | — |
| Device | categories `macos`/`android`/`screenshot`/`meeting`/`ui_automation`/`desktop_operations`/`device` | none | — |
| Unbound | `sandbox: shell`, open DuckDB `query`/`export`/`attach`, unknown shape | refused | — |

Host-read and device classes ignore the tool name. Classified but unwired
tools are refused.

**Operation splits.** Only these run: `http` GET; `files` read and write;
`duckdb` preview/describe of one relative file; a host-read pack's named actions
(no operation → narrowest read, never e.g. `catalog`).
Blocked: HEAD and mutating methods; list, exists, append, delete,
copy, move, mkdir, `grep`, `glob`; list_tables, `read_parquet` globs, query,
export, attach, persistent databases, session tables, SQL fragments.

## Contain profiles

| Profile | Who | Wired today |
|---|---|---|
| In-process compiled | provider opt-ins | `time_math`, `http.get`, `files.read`, `files.write`, `duckdb.preview`, `duckdb.describe`, two `internal_data` learning reads, host-read binders `thinking_maps_data`, `evidence_data`, `meetings_data`, `agent_roster_data`, `tasks_data`, `notes_data`, `memory_data` |
| OS jail | skillshub / CLI / USR | authority-free PureTransform (exact reviewed source, lock, private artifact, finite transport bounds); skills declaring `metadata.magician.app_egress` and in-place skills via brokered egress ([app-os-jail-egress](app-os-jail-egress.md)) |
| Governed MCP | skillshub official-SDK MCP | Zepto/Swiggy and other `protocol: mcp` skills via `dispatch_governed_mcp`. Not OS-jail. |

### Governed MCP runtime seam

`apps::governed_mcp` attests source, actions and transport ceilings, then
calls chat's `dispatch_governed_mcp` (OAuth, discovery, HITL, cart check, INR
`spend_session::admit`); adding `spend_gate` would double-reserve. Fails closed
without principal, workspace or agent.

Once checkout may have committed it returns non-retry `Ok` for:
over-64 KiB results (stub), sidecar failure (recover from the labeled
checkpoint, never re-dispatch), `InputRequired` / `Task` after send, ambiguous
transport, a stub still over a tiny ceiling, settlement or labeling faults
after `CommitObserved`. Labeled recovery without a completion intent requires
`disclosure_digest` to match the attempt's canonical input. Locks
storing an MCP skill as OS-jail or blocked fail review (contain-profile
mismatch) until republished.

### OS-jail runtime seam

`tool-runtime-core::governed_process_jail` and `apps::os_jail` (non-public)
run the authority-free PureTransform subset; egress is in
[app-os-jail-egress](app-os-jail-egress.md).

- **Admission** binds skill bytes, locked action/schema digest and typed
  lowering; refuses caller executable, host path, env, raw argv, cwd and
  runtime-control input. Approval, grant and resource sets are empty, so
  metadata cannot create authority.
- **Executor** resolves, hashes, snapshots and revalidates the exact
  executable, gives a finite env plus declared credentials, bounds and redacts
  output, kills/reaps the process group, writes the execution audit, and works
  in one canonical `0700` dir removed on every path (any other cwd refused).
- **Sandbox.** macOS: deny-default `sandbox-exec` (no Mach lookup or fork,
  literal-only exec of the snapshot, read-only runtime roots, writes only in the
  invocation dir). Linux: bubblewrap, all namespaces unshared, no host command
  dirs, read-only loader/library roots, snapshot at `/app`, only `/work`
  writable. No direct network or ambient env; missing launchers or unsupported
  hosts fail closed.
- **Limits.** `runtime.limits.stdout_bytes` and `stderr_bytes` must be
  declared (a skill with no stream limit is refused). App runs clamp them to
  7 KiB / 1 KiB (`APP_OS_JAIL_STDOUT_BUDGET` / `APP_OS_JAIL_STDERR_BUDGET`) so
  the result, with JSON expansion and envelope, fits the 56 KiB raw lane
  (64 KiB labeled); a larger answer fails as invalid output, never truncated.
  The ceiling is retained in descriptor, lock, plan digest and physical target.
  The jail also enforces CPU, memory, wall, process, task, open-file, file-size
  and output ceilings: on Linux the task ceiling is `RLIMIT_NPROC` inside the
  jail user namespace (`tool-runtime-core::governed_process_jail`); on macOS
  the strict profile denies fork and a watchdog samples tasks.
- **Scheduling.** Hashing, reopening and lowering run on the four-worker
  blocking pool before task/resource guards; review verifies one locked action
  at a time. Dispatch reserves one of four slots and waits for the worker before
  final guards and dispatch-start (30 s capacity/handshake deadlines, unlaunched
  slots expire at 30 s); the move-only action and one-shot provider
  authorization pass to it synchronously. The final fence reattests source,
  lock, action, plan and target; the BLAKE3 digest is re-verified from the
  opened descriptor just before snapshot and launch.
- **Results and audit.** Redacted stdout parses as bounded JSON or UTF-8 text;
  JSON-like bytes never fall back to text. Labels come only from the common
  effect/disclosure owner. The run-state sidecar seals receipt, digest,
  invocation/effect correlation, disposition and result digest with the
  completion intent (uncertain failures store correlation and any receipt;
  worker loss records receipt absence); recovery refuses legacy or digest-only
  completions without a matching sealed audit record. The receipt records platform, guarantees, ceilings
  and executable provenance; descriptor and lock share one plan digest (source,
  jail-profile versions, schema, argv mapping, policy, transport ceiling).
- **Uncertainty.** Post-start loss, unobserved cancellation or an unpersistable
  result is effect-uncertain (a lost observation unless exit/reap proves
  completion); only pre-I/O abort proof releases the reservation.

## Receipts

`AttestedAppToolTarget` is evidence, not authority; the move-only
`AppEffectPermit` binds installation/grant/package lock, schema,
primitive/action/implementation digests, physical target, invocation and
canonical input. Task then resource-root guards span the durable
`EffectDispatchStarted` checkpoint and first provider poll: cancel before it
releases proven-unspent; after it, loss, expiry or cancel settle
outcome-uncertain. `Settled` keeps the effect-binding digest plus the raw
`ActionResult` digest and byte length (content stays with disclosure/retention),
so a crash before the labeled-result checkpoint cannot detach result from
effect.

Bound HTTP mints `from_trusted_external_dispatcher` with `destination:{host}`
(still subject to the grant); bound file/table mint
`from_trusted_local_dispatcher` only from the move-only capability-directory
owner, whose target binds reviewed root and device/inode/metadata, normalized
relative path, operation/format, byte ceilings and atomic publish, re-walked
no-follow at the final fence and held through I/O. Introduced content joins Ordinary /
RemoteAllowed; the session can only raise classification or narrow processing.

## Scoped host-read binders

Each binder has its own pack identity, implementation identity and runtime ref
(changing one never rotates another lock). All:

- classify only their named operations (rest `Unbound`), guard the receipt
  mint by name, and need a target when none is defaultable; newer providers
  reject mutating action names at compile time;
- take scope only from executor-injected `__principal`/`__workspace` (model
  `__*` keys stripped; public `principal` / `workspace` are equality checks;
  path components scope-id validated);
- prove arguments closed; `bind_parameters_for_call` re-seeds `__action_name`;
- get the builtin witness only for the embedded definition (a disk or scope
  override stays unwitnessed even with equal bytes);
- label `from_trusted_local_dispatcher` (`IntroducesLocalContent`) and fail
  closed on oversize, never truncating mid-record;
- never mutate: widening one is a reviewed kernel change, not YAML.

| Binder | Identity / ref | Input / result | Actions (default first) |
|---|---|---|---|
| `internal_data` | — | 2 KiB / 512 KiB | `list_learning_candidates`, `read_learning_candidate` |
| `thinking_maps_data` | `magician.compiled-provider.thinking-maps-read.v1` | 2 KiB / 512 KiB | `list_maps`, `read_map` |
| `evidence_data` | `magician.compiled-provider.evidence-data-read.v1`, `runtime:compiled:evidence-data-read:v1` | 8 KiB / 512 KiB (also enforced in provider), 30 s | `list_pending_claims`, `read_claim`, `list_evidence_records`, `list_entities`, `list_commitments` |
| `meetings_data` | `magician.compiled-provider.meetings-data-read.v1`, `runtime:compiled:meetings-data-read:v1` | 8 KiB / 512 KiB, 30 s | `active_session`, `list_threads`, `read_thread`, `read_takeaways`, `upcoming_meetings`, `search_meeting_memory` |
| `agent_roster_data` | `magician.compiled-provider.agent-roster-read.v1`, `runtime:compiled:agent-roster-read:v1` | 4 KiB / 256 KiB, 30 s | `list_members`, `read_member` |
| `tasks_data` | `magician.compiled-provider.tasks-read.v1`, `runtime:compiled:tasks-read:v1` | 4 KiB / 512 KiB, 30 s | `list_tasks {status?, limit?, after?}`, `read_task {task_id}` |
| `notes_data` | `magician.compiled-provider.notes-read.v1`, `runtime:compiled:notes-read:v1` | 4 KiB / 768 KiB, 30 s | `search_notes {query, limit?, provider?}`, `read_note {source_ref}` (an unnamed call is `search_notes`, which needs `query`) |
| `memory_data` | `magician.compiled-provider.memory-read.v1`, `runtime:compiled:memory-read:v1` | 4 KiB / 512 KiB, 30 s | `search_memory {query, limit?}`, `read_entry {key}` |

Every binder mints the `ReviewedScopedHostRead` receipt; every pack's default
timeout is 30 s. List limits: ≤ 25 for learning and map lists (ids ≤ 128
bytes), ≤ 50 for evidence, meetings, notes and memory, ≤ 200 for tasks and the
agent roster.

### Learning-substrate reads

`list_learning_candidates` takes an optional closed candidate state. The rest of
the pack (catalog, SQL, telemetry, logs, audio notes, procedures)
stays agent-only. `authorize_runtime_scope`
refuses unscoped calls but falls back to the default scope when injected scope
is missing. Consumers declare `classification_floor: personal` /
`model_processing: local_only`; transitions stay on the owner-gated API.

### Thinking-map reads

Read-only over
`scopes/<p>/<w>/thinking_maps/<map_id>/{manifest.json, snapshot.json}` with the
store's types (`magician-surfaces` re-exports `thinking_map_models`); mutation
stays on `/thinking-maps`. Lifecycle filter
`active`/`paused`/`archived`/`deleted` (unknown errors); corrupt manifests
skipped; missing injected scope fails closed. `list_maps` filters (unsafe names
dropped first) and sorts names, reads at most `LIST_MAPS_SCAN_BUDGET` (2048) manifests,
then sets `scan_truncated: true` and `next_cursor`; `after_map_id` is a keyset
cursor that survives deletion. Pack 1.1.0 (`^1.0` accepts it). Brainstorm
(`magician_data_v3/system/thinking_map/app`, `brainstorm-canvas`) locks
`actions: [list_maps, read_map]`, calls them from `sync_maps`, and
`surfaces/canvas.html` reads via the bridge
([custom-surfaces-v1](custom-surfaces-v1.md)).

### The evidence-data host-read binder

- Evidence and entities need a validated `agent_id` (no cross-agent union).
  Claims take `audience_kind` + `audience_id` both or neither; commitments need
  both and read one shard.
- Loads only through `AgentMemoryService::load_scoped_evidence` (sensitive
  records suppressed, tombstones excluded, duplicate ids fail closed); entities
  derive from that and show only normalized identity, display and
  first/last-seen.
- Projections only: claims (words, speaker, audience, status, extraction time,
  id, revision; detail adds extractor and bounded utterance keys), evidence (id,
  status, summary, first-seen), commitments without source or supersession.
  Provenance, actions, artifacts, facets, keys, scores, sensitivity and producer
  metadata never cross.
- Timestamp + unique id order; cursors name the prior boundary id resolved in
  the current view; claim filters ≤ 512 bytes; fixed scan windows report
  `scan_truncated` + `next_cursor`.

### The meetings-data host-read binder

Capture control is a separate owner-signed destination; `classify_io` refuses
`listen`/`join`/`pause`/`resume`/`stop`.

- Threads and transcripts resolve through the scope's chat index, takeaways
  through the scoped memory service, upcoming meetings through the scope's
  capability auth root. `active_session` (no target) is the one process-global
  read (capture must stay visible), showing less than `GET /meetings/active`
  (no rolling summary).
- Thread ids need the `meeting-` prefix and `session_id` must belong to the
  thread; filters ≥ 2 chars; `read_thread`, `search_meeting_memory` need a target.
- `list_threads` pages in `(created_at, thread_id)` order and refuses unknown
  `after_thread_id` cursors; `read_thread` uses
  `ChatStore::get_messages_before_exact` (the lenient paginator would loop).
- Only plain-text turns cross (no tool cards, attachments or task-status rows).
  Keyword search covers transcript bodies, takeaways and titles under fixed
  session/message budgets reported as `scan_truncated`.

### The tasks-data host-read binder

App face of agent tools `list_tasks` / `get_task_details`.

- Projects id, title, status, agent, priority, due date, tags, timestamps,
  `is_blocked`, `awaiting_owner` (a bit) and outcome; `read_task` adds
  description and summary (≤ 4 KiB each). Plans, executions, chats,
  schedules and routing never cross.
- Reads via `V3ReadApi::list_tasks` (hidden tasks stay hidden), never the write
  guard (`get_task` triggers write-replay recovery).
- Newest first, id tie-break; `next_cursor` (`<created_at>|<task_id>`) survives removal.
- The boot-time global registry has no artifact service and fails loudly;
  per-scope registries attach one.

### The notes-data host-read binder

`search_notes` / `open_note` stay agent-only (`open_note` exposes host paths).
`read_note` takes a hit's `source_ref`
(`notes:<local_markdown|silverbullet>:<relative markdown path>`; `silverbullet` is
the persisted id of the notes folder); the proof
checks provider and `is_readable_note_path` (relative, no parent escape,
markdown), and `NotesSettingsStore::read_observation_note` re-confines to the
canonical provider root and refuses symlinks. Search uses Observe's
boundary-safe roots; hits and reads carry `open_url`, the canonical relative
`/notes?path=…&workspace=…` link (never a host path). Queries
≥ 2 chars and ≤ 256 bytes, ≤ 8 matches per hit, body ≤ 256 KiB; `scan_truncated` /
`more_available` pass through.

### The memory-data host-read binder

Grant model: [app-memory-access](app-memory-access.md). No caller-chosen tiers.

- The executor stamps `__app_installation_id` and `__app_run_mode` from
  `AppWorkflowService::app_memory_read_context`; without them the call is
  refused, so the binder is unusable outside an app workflow. The grant is
  re-read from the scope's app registry every call.
- Drops engagement and meeting memory; keeps app-sourced records only if every
  source is this still-eligible installation, owner memory only if its tier
  (user scope) or agent (agent/agent-goal scope) is selected for the run mode; drops superseded records (read-only temperature snapshot).
- Writes no temperature or retrieval usage. Results carry an opaque key,
  scope, tier, agent, bounded value, timestamps and origin (`granted` /
  `own_app`); `read_entry` answers `present: false` for gone and not-granted
  alike.

## Agents, procedures, personalities

Off this pipeline (the grant selects the runner or injects instructions); tools their runners call still go through bind + contain.

## Adding something

No kernel change: an HTTP-search skill declaring `search`/`research` or a `url`
parameter (a `-search` name is not enough; apps need its hosts in
`metadata.magician.app_egress` and a `destination:<host>` grant); a compiled
pack with `url` or `http`/`web` categories (Bound HTTP); a merge/math pack
(PureTransform). A first side-effect or device binder, or a new host-read
binder, is a reviewed kernel change with its own argument proof and
implementation identity. Open SQL, host paths, raw argv and
`sandbox: shell` stay unbound.

See [app-authoring-cli](app-authoring-cli.md) and
[app-platform-contract-kernel](app-platform-contract-kernel.md).
