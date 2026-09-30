# magician-storage-migration

Dormant owner-closure and migration qualification framework (Task 8).

Default Magician startup does **not** depend on this crate. There is no
operator CLI, UI, or provider selector here; Task 19 exposes that surface
after owner handlers and recovery are qualified.

- Catalog readiness/evidence schema lives in `magician-storage` and
  `scripts/storage_catalog_guard.py`
- Owner handler registry, fenced coordinator, crash/restart ledger, and
  owner-by-owner source guards live here
- Rollback invalidates forward-phase evidence; `find_open` does not treat
  `RolledBack` / `RollbackPending` as a forward-open migration. Cutover after
  rollback cannot skip the fence
- Synthetic owner proves every migration phase and readiness state

```
make test-storage-migration
```
