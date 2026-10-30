# Workspace File Provider

`WorkspaceFileProvider` is the **local compatibility adapter** for
`ArtifactV2Workspace`. It is not a remote-storage boundary and not a
migration tool. Typed capabilities in `magician-storage` plus
[storage-catalog.yaml](storage-catalog.yaml) own durable placement.
Copying a scope directory is not a supported migration or remote-acceptance
path. See [Storage Abstraction](storage-abstraction.md).

This is separate from the user-facing Notes Provider. Notes are human-readable
projections. The workspace file provider owns the local file tree used by
tasks, internal tasks, chat sessions, execution runs, and their outputs.

## Provider

The one provider is `local_file`. It uses this logical runtime layout under the
**provider root**:

```text
scopes/{principal}/{workspace}/
  tasks/
  internal_tasks/
  executions/
  ui/chat_sessions/
  ui/chat_turn_events/
  programs/
system/
```

`local_file` roots at the bootstrap/runtime store (`MAGICIAN_ROOT_DIR`,
defaulting to the live MagicianNotes root). Existing task, chat, and
execution URLs continue to resolve to the same relative paths.

The retired notes-folder backend (`silverbullet_space`, or `silverbullet`)
already behaved exactly like `local_file` rooted at its folder, so a config that
still selects it reads as `local_file`: with no `silverbullet.space_path` (the
shipped shape) the root is the runtime root, and a legacy `space_path` keeps
rooting the store at that folder. Nothing moves. Under `local_file` a stray
legacy `space_path` stays inert, and a save that omits it keeps the stored one.
Typed storage bootstrap profiles map `local_silverbullet_space` to
`local_embedded` the same way.

Do not place the runtime root under an iCloud-synced folder: reads of dataless
CloudDocs placeholders block while materializing and can stall startup
(`scripts/install.sh` warns). Use a plain local folder such as
`~/MagicianNotes`.

Program specs (harness focus areas) are **per-scope** user-authored markdown
in the scope's `programs/` dir, beside machine-owned `state/`:

```text
{provider-root}/scopes/{principal}/{workspace}/programs/
  program.md
  daily_ops.md
  engineering_strategy.md
  state/              # mutable, machine-owned runtime state (JSON, per run)
```

`ArtifactV2Workspace::program_specs_root == programs_root` (specs `.md` at
the root, state `.json` under `state/`). Not a sibling `Programs/` folder:
case-insensitive macOS collides it with `programs/`. Reader
(`harness::ProgramLoader`) and auto-apply writer
(`learning::work_ledger_program_state`) both resolve through
`program_specs_root`; a missing named spec fails loud, no fallback.

## Settings

Provider selection is the `workspace_storage:` section of the runtime-root
`magician-config.yaml`, not a separate settings document. The settings API
reads and load-edit-saves that section. Path:

```text
{bootstrap_root}/magician-config.yaml
```

(`WorkspaceStorageSettingsStore::settings_path()`.) Exposed through:

```text
GET /api/magician/v2/workspace-storage/settings
PUT /api/magician/v2/workspace-storage/settings
```

Web Settings shows the resolved values read-only. Minimal payload:

```json
{ "provider": "local_file" }
```

`resolved` carries `provider`, `local_root` and `active_runtime_root`. The root
is chosen before runtime services are constructed, so a change needs a
restart; nothing is deleted or migrated.

## Boundary

These storage roots resolve through the provider-backed workspace root;
their canonical readers/writers use provider-owned file operations:

- user-visible and internal tasks, task outputs, task executions, scoped
  executions
- chat sessions, chat output files, chat turn event logs
- task output preview/download/open-file/open-folder and portable export
- progress, attention, published-surface, execution-artifact, and pipeline
  projection stores under scoped task/execution roots
- progress-channel durable state and append-only event logs
- API-mining sequence, workflow, capability, registry, origin-policy,
  trace, maintenance, and projection records
- learning and skill-evolution candidates, events, procedures, evidence,
  evaluation reports, audit counts, and growth-eval runtime probes
- scoped agent runtime records (definitions, proposals, approvals,
  scheduler state, wake-up queues, update journals, memory correction and
  consolidation logs, isolated/shared memory copy/delete)
- system storage: `system/`

Provider operations: `create_dir_all`, `read` (plus bounded/prefix/range/
tail/to_string variants), `write`, `write_atomic`, `append`, `remove_file`,
`remove_dir_all`, `rename_sync`, `metadata`, `symlink_metadata`,
`exists_path_sync`, `canonicalize`, `read_dir` — async and sync where present.

The provider still exposes filesystem paths because the service, API
download/open handlers and UI links are path-based; future providers can slot in
behind `ArtifactV2Workspace` without task/chat code knowing.

`ArtifactV2Workspace` path-based wrappers check the resolved path is inside the
provider root, then delegate in provider-relative form. They reject paths that
do not strip under the root and relative paths with parent, root or prefix
components, and accept both macOS root spellings (`/var` vs `/private/var`).
Startup services receive an already-resolved `ArtifactV2Workspace`.

Verified file-backed JSON values use a same-handle streaming admission/hash
pass followed by a rehashed Serde pass that independently reapplies the
exact byte, depth, and node admission while decoding. Execution-index
read/modify/write holds one bounded cross-process transaction lock from the
fresh read through atomic publish and rejects an embedded execution
identity that disagrees with the path. Canonical raw tool-result
materialization holds an exact-key process lock and an adjacent bounded
cross-process file lock across manifest/locator publish; failed publication
removes any uncommitted locator, manifest, and payload before releasing it.

Provider-backed startup maintenance must not block the HTTP server bind:
LLM-dispatch orphan recovery, published-surface repair, stale synthesis
recovery, subscription reconciliation, feed/UI-thread scope materialization,
API-mining cleanup and agent runtime/scheduler hydration run after bind (or on
a delayed blocking worker); agent routes report
`agent_startup_hydration_pending` meanwhile. Capability-evolution restore uses
non-blocking shared locks and skips busy scopes; memory consolidation treats a
busy shared memory file as a skipped tick.

Provider path validation caches the resolved provider root and canonical
root candidates per `ArtifactV2Workspace`.

Task multi-write journals use a version-two metadata format: each
destination payload is staged as a separate provider-owned file; the
journal records destination path, staged path, byte size, and BLAKE3 hash.
Recovery validates the complete staged set before changing a destination,
then re-reads and revalidates one bounded payload at a time while applying
the write set. The journal is removed as the commit acknowledgement. A
failed validation leaves both records intact and lands no target writes.
Version-one journals with embedded byte arrays remain readable under
explicit operation, journal-byte, and aggregate-payload limits.

Verified terminal-output projection is provider-owned and streaming: a
canonical regular file already inside the workspace is copied through a
bounded heap buffer to an atomic temporary destination and published only
after its SHA-256 still matches the accepted artifact decision. JSON media
is syntax/depth validated from a buffered file reader before its
digest-verified copy, so replacing an in-memory `Value` does not weaken
the output writer's media contract.

Feed template bootstrap files resolve through the provider. The boundary does
not hide APIs that need real OS handles or physical paths: DuckDB
databases/parquet scans, cross-process lock files, executable skill
materialization, local process/dispatch ledgers, service work dirs/caches and
local-provider fallback internals remain filesystem-backed.

## Regression Guarantees

Tests pin path parity (task, internal-task, chat outputs, scoped execution
dirs), that helpers route through an injected provider, wrapper behaviour
including outside-root and parent-escape rejection, and that the retired
`silverbullet_space` selector resolves to `local_file` over the same root.

## Non-Goals

No migration of canonical task/chat data into the notes folder; publishing
user-visible Markdown copies is the separate notes layer.
