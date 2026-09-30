# ADR: Remote transactional state backend (Decision Gate 1)

**Status:** Accepted  
**Date:** 2026-08-31  
**Task:** Storage plan Task 5  
**Spike:** `magician-storage-gate1` (disposable; not a production adapter)

## Decision

PostgreSQL is the first remote production backend for Magician transactional
state (tasks/executions, chat, attention/resurfacing, outboxes, and
database-backed leases).

Local embedded mode remains SQLite behind the **same domain repository
traits**. Compute placement and storage placement stay independent: a laptop
may select `remote_durable`, and a server may run `local_embedded`.

Hosted SQLite / libSQL is **not** selected. A remote SQLite file is forbidden
by the storage contract. A hosted SQLite protocol would still have to pass this
same matrix, including PITR and multi-writer lease fencing, before it could
replace PostgreSQL.

No remote database adapter is wired into `magician-bin` or default startup by
this ADR. Owner packets implement repositories after this gate.

## Options

| Option | Local | Remote | Verdict |
| --- | --- | --- | --- |
| A. PostgreSQL remote + SQLite local (same traits) | SQLite | PostgreSQL | **Accepted** |
| B. PostgreSQL for local-container and remote | PostgreSQL | PostgreSQL | Rejected: extra local-install burden; SQLite already proved the single-writer local shapes |
| C. Hosted SQLite/libSQL + local SQLite | SQLite | libSQL/Turso or a network SQLite file | Rejected: network files are contract-illegal; hosted SQLite does not own PITR/outbox/lease isolation as well as PostgreSQL |

## Spike isolation

`magician-storage-gate1` uses synthetic fixtures only. It is not a dependency of
`magician`, `magician-bin`, or any production crate. It does not open
MagicianNotes or cataloged stores. It cannot become a parallel source of truth.

Run:

```
make test-storage-gate1
# optional live Postgres:
MAGICIAN_GATE1_POSTGRES_URL=postgres://... make test-storage-gate1
```

## Scenario matrix (section 14)

Shapes exercised: task CAS + idempotency, chat session + paginated transcript,
attention retention query, outbox claim, lease fencing, scope isolation, cursor
order, schema migration, uncommitted rollback, committed durability, backup/
restore.

| Scenario | SQLite | PostgreSQL adapter | Hosted SQLite |
| --- | --- | --- | --- |
| task/execution CAS + idempotency | pass | implemented; run with `MAGICIAN_GATE1_POSTGRES_URL` | not selected |
| chat session + paginated append/read | pass | implemented | not selected |
| attention/resurfacing retention query | pass | implemented | not selected |
| outbox claiming | pass (one claimant) | implemented (`UPDATE … RETURNING`) | weaker over HTTP write-forwarding |
| lease fencing / stale generation | pass | implemented | generation fencing possible; isolation weaker than PG |
| scope isolation + cursor order | pass | implemented | not selected |
| schema migration | pass (idempotent `ALTER`) | implemented (transactional DDL) | not selected |
| connection-loss / commit ambiguity | pass: uncommitted drop rolls back; committed row survives | implemented; `CommitLikelihood::Maybe` still required after network timeout | not selected |
| backup / restore | pass (`rusqlite` backup API, fresh file) | **not** file-copy; requires `pg_dump` / PITR | point-in-time restore is a hosted extra, not a file |
| two-process writer | **BUSY/LOCKED** (measured) | MVCC writers | serialized through one primary |
| Rust driver | rusqlite 0.32 bundled | tokio-postgres 0.7 compiled in the spike | libsql crate not adopted |
| TLS / pooling | N/A (process-local file) | required by remote profile (`tls: require`) | TLS to HTTP endpoint; pooling ≠ PG sessions |
| Local-install burden | already shipped | optional for remote profile only | extra hosted account |

The PostgreSQL dialect is skipped unless `MAGICIAN_GATE1_POSTGRES_URL` is set; a
live-PG run fills that column without changing the decision.

## Why not PostgreSQL everywhere

Local Magician is already a single canonical writer per scope (Task 4
`ScopeLeaseManager`). SQLite passed every Magician-shaped scenario under that
assumption, including backup-by-file. Forcing PostgreSQL on every laptop
install adds a server, migrations, and credentials with no local correctness
gain.

## Why not hosted SQLite

1. Object storage must not run a live SQLite/DuckDB file (plan §5.3). A
   copied-directory remote model is already rejected.
2. Two processes on one SQLite file get `SQLITE_BUSY`. That is acceptable
   locally because the scope lease refuses the second process. It is not a
   remote multi-tenant coordination story.
3. Outboxes and leases need compare-and-set plus fencing generations.
   PostgreSQL `UPDATE … WHERE … RETURNING` under a real transaction is the
   production shape. libSQL HTTP write forwarding can emulate rows but does
   not give Magician PITR, pooling, or cancellation equal to PostgreSQL.
4. Default recommendation in the plan is PostgreSQL; the spike did not find
   evidence to overturn it.

## Consequences

- Domain repositories keep SQLite local implementations until an owner packet
  adds a PostgreSQL implementation of the **same trait**.
- `remote_durable` continues to require `state.driver` other than sqlite/duckdb,
  `leases.driver: state_database`, and `tls: require`.
- Task 6 (remote object/dataset) stays independent of this state decision.
- Reconsider hosted SQLite only with a passing run of this matrix against a
  disposable hosted instance, including PITR and two-writer fencing.
