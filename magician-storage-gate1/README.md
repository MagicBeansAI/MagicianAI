# magician-storage-gate1

Disposable Decision Gate 1 spike. Synthetic Magician-shaped SQL workloads
(task CAS, chat pages, attention retention, outbox claim, leases) against
local SQLite and optional PostgreSQL.

This crate is **not** a production adapter:

- `magician-bin` must not depend on it
- it must not open MagicianNotes or cataloged stores
- it is not a source of truth

ADR: [docs/components/magician-storage/adr-2026-08-31-remote-transactional-backend.md](../docs/components/magician-storage/adr-2026-08-31-remote-transactional-backend.md)

```
make test-storage-gate1
MAGICIAN_GATE1_POSTGRES_URL=postgres://… make test-storage-gate1
```
