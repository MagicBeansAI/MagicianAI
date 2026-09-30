# Task 21 — Typed storage boundaries and documentation closure

Prevent regression to implicit path storage. Compute placement and storage
placement stay independent only through the explicit Track B workflow.
Default startup remains `local_embedded` and `StorageRuntime::open_local`.

## Ratchets

| Guard | What it rejects |
| --- | --- |
| `scripts/check_typed_storage_boundaries.py` | New runtime-root I/O, `read_parquet(` globs, object SDKs, ambient `StorageRuntime` / `LocalStorage` / `S3ObjectStore` / `SqlitePool` construction, and `StorageRuntime::install` outside the reviewed allowlist. Scan roots include `magician-storage-gate1`, `document-to-markdown-cli`, and `kindle`. |
| `scripts/storage_catalog_guard.py` | Unlisted on-disk `Connection::open` (Task 0; still the database-open ratchet) |
| `scripts/check_store_durability_adoption.py` | New hand-rolled durable writes (not a semantic "is this fs write durable?" scan) |
| `scripts/skillshub_runtime_root_linter.py` | Skillshub `MAGICIAN_ROOT_DIR` resolution outside the Task 16A shim |

Allowlist: `scripts/typed_storage_boundary_allowlist.yaml`. Exact files are
two-way. Prefix trees are reviewed adapters, owner kits, backup/export, and
the `magician-bin` composition root. `magician-bin` must depend on
`magician-storage` and must not depend on `magician-storage-s3`,
`magician-storage-state`, or `magician-storage-migration`.

## Retirement

- `WorkspaceFileProvider` is documented as a local compatibility adapter, not
  a remote-storage boundary. The Settings Runtime provider control remains.
- Copying a scope directory is not a supported migration or acceptance path.
- Reaching `remote_ready` does not delete the current local source. Cleanup
  requires `remote_active`, the documented rollback-retention window, a
  successful restore drill, and a separate operator decision.
- Remaining `default_storage_base_path` call sites are local-compatibility
  and fail the ratchet if new library sites appear.

## What this packet does not do

It does not change default startup, wire S3/Postgres into `magician-bin`,
delete local adapters, or remove the desktop Runtime provider control.
