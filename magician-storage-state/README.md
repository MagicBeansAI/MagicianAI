# magician-storage-state

Dormant SQLite/PostgreSQL repository foundations and SQL-backed `LeaseStore`.

Default Magician startup does **not** depend on this crate. Local product
leases remain the Task 3 filesystem adapter plus Task 4 `ScopeLeaseManager`.

- SQLite pool: WAL, busy timeout, bounded slots, schema ledger
- PostgreSQL pool: `sslmode=require` only; connects with rustls (webpki roots);
  DSN redacted in `Debug`
- Copyable `ExampleRepository`: one Immediate transaction owns item+outbox,
  CAS `UPDATE … RETURNING`, and full-scope import
- SQL leases: monotonic generation, server-clock expiry, two-process contention,
  stale renew

```
make test-storage-state
```
