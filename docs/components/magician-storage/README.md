# magician-storage

Neutral crate for Magician storage identifiers, errors, bootstrap profiles,
and capability traits (`ObjectStore`, `DatasetStore`, `IndexStore`,
`LeaseStore`, `ScratchStore`, `SecretStore`).

It does not depend on `magician`, AWS, or PostgreSQL. Domain repository
traits stay with their owner crates.

Program contract: [Storage Abstraction](../magician/storage-abstraction.md).
Catalog: [storage-catalog.yaml](../magician/storage-catalog.yaml).

Default `magician-bin` startup stays `local_embedded`: it does not select
remote adapters, start migration, or run activation/cutover.

## Bootstrap

`--storage-bootstrap <path>` and `MAGICIAN_STORAGE_BOOTSTRAP_CONFIG` select a
YAML document parsed by this crate **before** stores open. When both are
absent, the profile is `local_embedded` and existing `workspace_storage`
behavior is unchanged.

Remote documents fail closed on missing/unknown/newer schema, inline secrets,
weak TLS, SQLite/DuckDB as the remote state driver, overlapping object/dataset
prefixes, scratch inside the object root, and leases that are not
`state_database`. `credentials_ref` is a `SecretRef`: slash-separated
`LogicalObjectId` segments such as `object-store/production`, never an
inline secret. Identical dataset-part restage of the same digest is
idempotent CAS; a different digest at the same part identity is `Conflict`.

Examples: `magician-storage/examples/local_embedded.yaml`,
`magician-storage/examples/remote_durable.yaml`.

## Local adapters

`magician_storage::fs::LocalStorage::open(root)` builds filesystem adapters
under that root (`objects/`, `datasets/`, `indexes/`, `leases/`, `scratch/`,
`secrets/`). They do not relocate or bulk-rewrite existing bytes. Existing
files are readable without a migration.

**Object metadata:** additive per-object sidecars owned by the local adapter,
not a second catalog and not public locators. Beside `objects/<encoded-key>`
the adapter may write:

- `<key>.objmeta.json` — version, digest, length, live/publishing/tombstone
- `<key>.objlock` — exclusive per-key flock
- `<key>.objpub.<version>` — unpublished generation (crash recovery)
- `<key>.objquarantine` — fail-closed evidence
- `<key>.objretain.<version>` — tombstoned bytes kept through the restore window

`list_diagnostic` skips those siblings. On first version-aware access to
legacy bytes, the adapter locks the key, computes length/digest, and
registers an opaque UUID version without mutating the object bytes. Corrupt
sidecars return `Corrupt`; digest/length mismatch returns `Integrity`.
Tombstone metadata wins over leftover bytes. Interrupted publish recovers
to exactly one old or new valid generation. `CreateOnly`,
`ExpectedVersion`, and conditional delete are implemented; overwrite
assigns a version.

Leases are an OS exclusive lock plus a monotonic generation file. Scratch
enforces purpose/path confinement and a byte high-water mark. Secrets are
created mode `0600` (not chmod-after-write) and values never appear in `Debug`.
The local index adapter records a rebuildable watermark only; it is not
canonical memory.

`StorageRuntime::open_local` builds the typed adapter set plus a
`ScopeLeaseManager`. S3 adapters live in `magician-storage-s3` and stay
unselected here. Classified live writes go through cataloged owner kits via
`typed_io`. `ArtifactV2Workspace` remains the local compatibility adapter
for unclassified, scratch, notes, and JSONL commit-marker paths.

## Process-wide typed runtime (Task 22)

`magician-bin` installs one `StorageRuntime` at the resolved workspace root
under `.magician-storage/` and publishes it process-wide before CLI /
`--reindex` return. The exclusive default `anonymous`/`default` scope lease
is acquired only on the long-running server path. Profile and held
generations report on `GET /health/storage`. A second **server** process
targeting the same local scope is refused. Distinct scopes remain independent.
Lease generation is the fencing token; lease-loss trips a shutdown callback and canonical writers fail closed.

Libraries must not open a second backend from `$HOME` / `MAGICIAN_ROOT_DIR`.
Use `StorageRuntime::current()` / `magician_v2::process_storage`
(`current`, `require`, `runtime_root`, `workspace`) or an injected workspace.
`default_storage_base_path()` returns the installed root once set. Live I/O
goes through `typed_io`; Parquet republishes through `publish_written_parquet`;
SQLite/DuckDB stay on `database_file_path` / `host_database_path`.
Composition-root bootstrap (`magician-bin`, `config.rs` before stores open),
the desktop engine-root mapping, and magicutor stay allowlisted in
`scripts/typed_storage_boundary_allowlist.yaml`. `magician-bin` does not
depend directly on the s3/state/migration crates (the magician lib depends
on `magician-storage-migration`, so the binary links it transitively).

## Gates

- **Gate 1.** PostgreSQL is the first remote transactional state backend;
  local embedded remains SQLite. Hosted SQLite is not selected.
  [adr-2026-08-31-remote-transactional-backend.md](adr-2026-08-31-remote-transactional-backend.md).
  Spike crate `magician-storage-gate1` (`make test-storage-gate1`) is not a
  `magician-bin` dependency.
- **Gate 2.** Accepted local_embedded RPO/RTO:
  [adr-2026-09-01-recovery-objectives.md](adr-2026-09-01-recovery-objectives.md).
  A drill **fails** if measured RPO/RTO exceeds the matching class. Object GC
  retains tombstone bytes for seven days and will not collect a referenced
  version or the live replacement. Dataset GC drops unreferenced generations.
  Snapshot/restore: `magician/src/magician_v2/recovery/`. Archives are
  integrity-signed; secret bytes stay out of backups.
  Runbook: storage-backup-restore.
- **Gate 3.** Accepted performance and capacity budgets:
  [adr-2026-09-01-performance-capacity.md](adr-2026-09-01-performance-capacity.md).
  A drill fails if a measured line misses its number. Offline cutover
  write-freeze of 900 s is accepted, so online migration stays optional.
  The lines are remote-durable numbers except where a name says `Local`; a
  local put pays a chain of full-device flushes rather than one round trip.
  Wall-clock lines are enforced by `make test-storage-budgets`, which runs
  them one at a time — the workspace report lane saturates the machine, so it
  sets `MAGICIAN_GATE3_ENFORCE_LATENCY=0` and records those numbers instead.

## Sibling crates

- `magician-storage-s3` (`make test-storage-s3`) — S3-compatible object and
  dataset stores. `open_from_profile` requires `remote_durable`.
- `magician-storage-state` (`make test-storage-state`) — SQLite pool, schema
  ledger, example repository, PostgreSQL/SQL lease adapter.
  `IdempotencyKey` / `Revision` live in `magician-storage`. No production
  owner is routed through the state crate.
- `magician-storage-migration` (`make test-storage-migration`) — owner
  handler registry, fenced coordinator, crash/restart and closure-packet
  ledgers, owner source guards. Rollback enters `RollbackPending` while
  forward-phase evidence is still complete, then invalidates
  export/import/verify/cutover so resume cannot skip those phases.
  Catalog readiness keys: `scripts/storage_catalog_guard.py`. Copy
  [owner-closure-reference.md](owner-closure-reference.md).

## Track A / B

Catalog owners are `remote_ready` while `local_active`. Tier 2
contracts: [capability-support-matrix.yaml](capability-support-matrix.yaml).
Scratch and rebuildable indexes may be deleted without losing Tier 1 bytes.
`magician storage …` and `GET /storage/activation` expose inventory, status,
dry-run plan, and fenced cutover (recent backup + remote health +
confirmation). [task-19-activation.md](task-19-activation.md).
Two engine identities can share one durable store without copying a scope
directory. `open_local` still rejects `remote_durable`. Device-bound
automation is `device-bridge-required`. Closed reference owners are
`remote_ready` with local layouts still canonical.

## Typed storage boundaries

`make check-typed-storage-boundaries` rejects new implicit-path storage
(`scripts/typed_storage_boundary_allowlist.yaml`).
`WorkspaceFileProvider` is not a remote boundary. Local sources stay until
`remote_active` plus a separate operator cleanup decision.
[task-21-typed-storage-boundaries.md](task-21-typed-storage-boundaries.md).
