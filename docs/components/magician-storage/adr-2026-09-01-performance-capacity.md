# ADR: Performance and capacity budgets (Decision Gate 3)

**Status:** Accepted  
**Date:** 2026-09-01  
**Task:** Storage plan Task 18A  
**Accepted by:** Magician runtime and storage owners

## Decision

Remote-durable performance and capacity budgets are the numbers below.
A Gate 3 drill **fails** if a measured value exceeds the matching line
(throughput is a floor: it fails if measured is below the accepted
number). Recording a measurement with no target does not close this gate.

These numbers were measured against:

- local filesystem adapters (`magician-storage`)
- hermetic S3 object adapter (`magician-storage-s3` `MemoryBlobStore`)
- SQLite example repository (`magician-storage-state`)
- Task 12/13 owner kits for chat append/page and task list

Offline cutover write-freeze of **900 s** is accepted for a representative
scope, so section 15.3 online migration stays **optional**.

| Line | Accepted | Unit |
| --- | --- | --- |
| Repository read p50 / p95 / p99 | 20 / 100 / 250 | ms |
| Repository mutation p50 / p95 / p99 | 50 / 200 / 500 | ms |
| Chat append / page p95 | 200 / 200 | ms |
| Task list/page p95 | 250 | ms |
| Object small (64 KiB) first-byte / complete p95 | 50 / 100 | ms |
| Object small (64 KiB) complete p95, **local adapter** | 250 | ms |
| Object medium (≤ 8 MiB) first-byte / complete p95 | 100 / 2_000 | ms |
| Object large complete p95 | 15_000 | ms |
| Max in-memory bytes per transfer | 64 MiB | bytes |
| Parquet flush / manifest commit / query p95 | 2_000 / 100 / 100 | ms |
| Outbox backlog | 10_000 | pending |
| Dataset backlog | 1_000 | uncommitted generations |
| Lease TTL / renewal / loss detection | 86_400 / 3_600 / 3_600 | s |
| Scratch quota | 64 MiB | bytes |
| Scratch cleanup | on open | flag |
| Index rebuild p95 / stale fallback | 30_000 ms / 60 s | |
| Migration throughput (floor) | 8 MiB/s | bytes/s |
| Write-freeze window | 900 | s |
| Backup RPO / restore RTO | 0 / 900 | s (Gate 2 canonical) |
| Monthly storage / operations / egress | 100 GiB / 10M / 50 GiB | caps |

Performance failure must not trigger unverified publication, raw prefix
listing, unfenced local fallback, or a silent drop.

## Amendment: local small-object line, and where latency is enforced

**A local durable put is not a remote one.** The 100 ms small-object line is a
single network round trip. `LocalStorage::put` publishes through a pending file
and two sidecar generations, and on macOS every `sync_all` is an `F_FULLFSYNC`
that flushes the whole device cache (~40 ms per 64 KiB put, mostly
bookkeeping). The local adapter therefore has its own line of **250 ms**
(`ObjectSmallCompleteLocalP95Ms`); the remote line stays 100 ms.

**Wall-clock lines cannot be asserted on a saturated runner.** Millisecond-scale
lines are enforced only when `MAGICIAN_GATE3_ENFORCE_LATENCY` is not `0`; the
workspace report lane sets it to `0` and records the numbers, and
`make test-storage-budgets` enforces them one test at a time. Declared lines
(quotas, TTLs, backlogs, flags) and second-scale lines with large headroom
(write-freeze, restore RTO) are always enforced.

Both gate constants are asserted at **compile time** in
`magician-storage/src/gate3.rs`, so reopening either stops the workspace
building:

```rust
const _: () = assert!(GATE3_CLOSED, "...");
const _: () = assert!(!ONLINE_MIGRATION_REQUIRED, "...");
```

The drill in `magician_v2/gate3/` still checks at runtime that the build's
numbers are the ones the closed gate accepted.

`fsync_parent` makes a completed rename durable with the parent-directory flush
alone, because every caller already flushed the file before renaming. The
crash-recovery path still uses `fsync_file_and_parent`, since it republishes
bytes written by a process that can no longer vouch for them.

## What this packet does not activate

Default startup still uses `local_embedded`. Cutover still requires a
recent backup, remote health, no competing lease, and confirmation.
This gate only accepts the numbers.

## Evidence

- `magician-storage/src/gate3.rs` accepted constants
- `magician-storage/tests/gate3.rs`, `magician-storage-s3/tests/gate3.rs`,
  `magician-storage-state/tests/gate3.rs`
- `magician/src/magician_v2/gate3/` chat, task, freeze, backup, and cost drills
- `make test-storage-budgets` — the lane that enforces the wall-clock lines
