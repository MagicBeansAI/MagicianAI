# ADR: Recovery objectives (Decision Gate 2)

**Status:** Accepted  
**Date:** 2026-09-01  
**Task:** Storage plan Task 17  
**Accepted by:** Magician runtime and storage owners

## Decision

Track A `local_embedded` recovery objectives are the numbers below. A
restore drill **fails** if measured RPO or RTO exceeds the matching class.
Recording a measurement with no target does not close this gate.

| Class | RPO | RTO | Notes |
| --- | --- | --- | --- |
| Canonical state | 0 s | 900 s | Durable write is the commit point; 15 minutes to restore a representative scope snapshot |
| Required objects | 0 s | 900 s | Immutable object committed before a database reference; 7-day delayed physical GC |
| Observability datasets | 86_400 s | 14_400 s | 24 h RPO (regenerable from journals/Parquet); 4 h rebuild |
| Secrets / configuration | 0 s | 1_800 s | Bootstrap files are durable; secret *bytes* stay out of backups and follow the external vault/KMS procedure |

Object tombstones retain bytes for **7 days** (`DEFAULT_TOMBSTONE_RETAIN`) so a
database recovery point can still resolve referenced generations. GC never
deletes a live object, a referenced version, or a live replacement.

## What this packet does not activate

Default startup does not snapshot, export, or collect garbage. Remote PITR
(`pg_dump` / object-lifecycle) remains the remote-profile operator path after
Track B activation. These targets apply to the local_embedded profile that
Track A ships.

## Evidence

- `magician-storage` delayed object GC and dataset generation GC
  (`magician-storage/tests/gc.rs`)
- `magician/src/magician_v2/recovery/` representative scope snapshot/restore
  spanning repositories, objects, datasets, indexes, devices, and secret
  refs; integrity walker (key/version/digest); signed snapshot; Gate 2
  pass/fail comparison
- Section 16.3 drills: local_embedded backup/restore, remote logical
  SQLite PITR with referenced object generations, fresh-host empty
  scratch/index rebuild, deleted-object retain until unreferenced GC,
  credential rotation without rewriting product bytes
- storage-backup-restore runbook
