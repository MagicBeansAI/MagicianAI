# App Events And Owner Notifications Threat Model

Schedule/event execution is behind the off-by-default
`app_platform.background_behaviors.enabled` boot master; foreground
owner-notification debt always retains a delivery owner.

## Scope

Two independent manifest/review/runtime axes:

- `app_event_behaviors_v1`: one same-installation canonical root execution
  terminal can start an exact reviewed event behavior;
- `app_owner_notifications_v1`: an app workflow may emit one-way,
  workflow-scoped briefing/escalation content into the owner's existing
  attention funnel.

Neither is a generic event bus, push channel, approval protocol,
question/response primitive, or authority transfer.

Out of scope for V1: started/cancelled/delegated-child/transport-only/cross-app
event kinds; app-authored events, arbitrary filters, cross-installation fanout;
multi-hop causation/spend propagation; notification questions, approvals,
critical severity, phone push and app response authority; distributed workers or
provider-level exactly-once claims.

## Trust boundaries

Trusted: the canonical Artifact journal/projector, workflow task and run
sidecars, authenticated scope service, immutable package lock, registry
transactions, reviewed grants, resource authority, and the shared
`UserRequestService`. Untrusted data: app manifests, stored values, workflow
instructions, model/tool output, notification text, UI-thread strings,
serialized leases, transport events.

```text
Event flow:
persisted canonical execution-completed receipt
  -> private observer proves task + canonical root run + installation
  -> foreground-origin and succeeded/failed allow-list
  -> immutable ingress receipt + deterministic fire debt
  -> bounded lease + live binding/rate authorization
  -> exact event-bound governed workflow
  -> durable settlement

Notification flow:
authenticated app workflow notify_owner effect
  -> exact task/manifest/port/grant revalidation
  -> deterministic durable notification outbox row
  -> bounded owner-delivery lease
  -> durable deterministic UserRequest
  -> existing attention UI; response detached from app routing
```

## Event invariants

1. The observer accepts only persisted `agentic.execution_completed` facts,
   never the live presentation broadcaster.
2. Installation identity comes from the sealed app task binding; the event
   execution must be the canonical root in the execution sidecar. UI-thread text
   is only a consistency check.
3. V1 projects only bounded `execution_ref`, `succeeded|failed` and canonical
   recorded time in a host-constructed, non-deserializable DTO with
   Sensitive/LocalOnly floors.
4. Scheduled- and event-origin terminals are suppressed, so V1 is one-hop and
   cannot form schedule/event recursion (full causation/spend lineage is out of
   scope).
5. One immutable receipt records each enabled-installation/event-ref projection
   and exact fanout (including zero). Replay must match bytes, digest, kind and
   fire count; a purge/disable race acknowledges without recreating bytes.
6. Fanout ≤ 32. Event-ref replay uses a `(installation_id,event_ref)` index;
   claim pages, lease duration, retry delay and attempts are bounded.
7. Rate identity is stable per installation + event-behavior id: active period
   and consumed starts survive package/grant digest changes, and the new ceiling
   applies to already consumed work. `last_started_at` is kept independently of
   the period, so rollover cannot reset `min_interval_seconds`; invalid
   timestamps fail closed.
8. Rate admission, launch identity sealing and attempt charging share the final
   pre-launch transaction; preclaimed, deferred, paused, timed-out or unstarted
   work consumes no attempt.
9. Every dispatch and workflow root reopens scope, pause, enabled generation,
   package, schema, grant/review digest, source policy, resource authority,
   lease, fence and expiry. Unsupported profiles are dead-lettered with closed
   reason codes.
10. Corrupt rows are quarantined individually; one bad row cannot roll back a
    claim page or strand later leases.
11. After the 7-day terminal retention window, compaction may remove projection
    and fire payloads only when stored fanout is exact and every fire is
    terminal with the same kind and projection digest. An immutable
    digest/fanout tombstone stays the replay authority until installation purge.
12. Each compaction candidate owns a savepoint; a failed integrity proof rolls
    back that candidate, keeps all payload evidence, and writes a content-free
    quarantine marker that later scans recognize.
13. Candidates are discovered via durable raw identity cursors, not an
    eligibility-filtered oldest-first query: forward epochs with a fixed identity
    end, plus an independent cyclic revisit rail served after ≤ 64 forward
    pages, so skipped live/young rows are revisited without stalling progress.
    ≤ 32 raw receipts per sweep; malformed timestamps go to quarantine; deleted
    cursor keys remain valid seek positions.

## Notification invariants

1. A notification command exists only after the workflow owner reopens the
   exact task, workflow, compiled `notify_owner` port, manifest, generation and
   current notification grant.
2. V1 permits only `briefing|escalation`, `info|warning`, bounded
   title/message, reviewed purpose, stable effect identity and content-free app
   acceptance. `question`, `critical`, response routing and free-text responses
   are refused.
3. Acceptance, idempotency, period volume, attention-pending ceiling, TTL and
   outbox insert happen in one immediate transaction; changed bytes under one
   effect id fail closed.
4. Period usage is keyed by installation/workflow/port, not review digest; a
   shorter replacement period inherits the longest active window, so re-review
   cannot mint fresh allowance.
5. `max_pending` counts pending, leased and delivered-unexpired attention, so
   moving a row to delivered cannot evade the ceiling.
6. **Capacity refunds.** Claiming charges nothing; the live lease is
   revalidated and charged just before `UserRequest` submission; pause preserves
   debt. A UserRequest scope-capacity refusal publishes a unique content-free
   refund receipt before applying the debit. Pending receipts fence claim and
   attempt-ceiling selection and are repaired in fixed pages; an active
   successor is never revoked (its begin/settle CAS consumes the debit). Applied
   receipt identity prevents double decrement and cascades away with outbox
   compaction. Retry backoff starts at 5 s, caps at 1 h and never crosses
   absolute expiry, bounding a maximum 31-day TTL to 753 serialized receipts per
   correlation. The charged owner atomically returns to pending, advances its
   fence and clears lease identity (no ghost lease). Process loss or auth
   expiry before the first receipt commit may consume the charged attempt; after
   commit, debt stays durable and fenced.
7. The outbox settles to delivered only after `UserRequestService` has durably
   snapshotted the deterministic pending request; otherwise the lease is
   recovered.
8. The absolute outbox expiry travels in host context and drives live and
   restored UserRequest timers, so delayed delivery cannot extend the TTL.
   Marker-bearing rows require exact host source and sealed expiry; malformed
   restored shapes are redacted into cleanup debt.
9. Resolved notification content is replaced at once by a compact
   identity/scope/digest/expiry tombstone kept only until the TTL.
10. Owner responses never route to an app task or execution; the app gets
    neither content nor a resume signal.
11. A terminal outbox row keeps its payload through both its TTL and reviewed
    period; only then may the compactor replace it with an immutable
    effect/payload-digest tombstone (idempotent replay; changed payload or
    authority fails closed until purge).
12. Payload digest, typed payload, severity and host correlation are re-proved
    before compaction; failure follows the per-row rollback + content-free
    quarantine path.
13. Notification compaction uses the same forward/revisit discovery over raw
    `correlation_id` order, classifies eligibility only inside the transaction
    over a fixed 32-row page, re-samples host time and authenticated scope after
    acquiring the transaction, and revalidates authority before every commit, so
    an expiring credential cannot publish tombstones, delete payload or advance a
    cursor.

### Body custody (invariant 14)

- The app outbox is the source-payload authority through its retention window.
  `UserRequestService` is the sole durable owner of the delivered attention
  projection; no generic sink mirrors the body.
- The private keyed HITL lifecycle store (V3 SQLite, private `0600` files,
  DELETE journal) keeps only an expiry-bound content-free pending/resolved proof
  plus an opaque random UUID-v4 owner generation, stored beside the UserRequest
  body to prevent same-key substitution. It keeps neither the body nor a body
  digest (a resolved proof's fixed `HitlResolved` envelope fingerprint is not a
  body digest). Restore never turns a pending proof back into content until the
  live UserRequest owner republishes the exact generation and deadline.
- Reconciliation returns a short-lived opaque ticket holding the publication
  lock, bound to its broadcaster and each accepted event's length + SHA-256. The
  UserRequest owner does a non-awaiting exact pending revalidation and
  lifecycle-locked broadcast through it; contention drops authority and retries
  rather than suspending with the file lock, and the ticket is released before
  any async yield. Exact reads refuse content at absolute expiry.
- Ready startup never scans the aggregate table; exact reads/writes use the
  scoped primary key. Cleanup deletes ≤ 512 expired rows per pass and vacuums a
  fixed page slice, re-armed from indexed `MIN(expiry)`.
- The one-time legacy JSONL import streams bounded records, commits, archives
  the JSONL and replaces its path with a private `0700` directory before DB
  readiness — permanently fencing older binaries from appending split authority.
  Pre-ready crashes reimport the archive; post-ready startup refuses a missing or
  substituted fence.

### Sealed marker in generic sinks (invariant 15)

The workspace transport log, generic progress EventLog, ChatStore escalation
projection, per-chat-turn JSONL fanout, mobile push dispatcher and process debug
log recognize the host-sealed notification marker and refuse the body. The
workspace-log writer streams a bounded-memory startup rewrite of legacy copies;
only single malformed/over-ceiling rows are dropped, never ordinary rows for
size. `/events` backfill and `internal_data` event-log readers withhold marked
and unclassifiable rows even before that rewrite; the live `/events` tail refuses
this surface. Execution-index JSONL is outside the filter. Append, scrub and
compaction share an advisory cross-process lock, and scrub CASes source length,
digest and file identity before replacement.

### Resolution and restart (invariant 16)

- Resolution removes the live prompt even if lifecycle publication fails after
  the response commit; startup retries the content-free resolution once the
  UserRequest shards are clean, first re-establishing the pending proof if the
  original append never committed (ensure + resolution under one lock). A
  generic or different-app same-key lifecycle is preserved and refuses the
  transition.
- Hot acceptance and restore establish durable UserRequest ownership and arm the
  original timeout before releasing pending/history. Durable reconciliation never
  broadcasts a saved body itself; the worker revalidates the still-live request
  under a short pending guard and calls a lifecycle-locked broadcaster, so a
  response or expiry that wins during IO suppresses late publication.
- Request and resolution retries run through coalesced 32-entry queues (each pop
  examines ≤ 64 nodes), grouped per scope with one blocking-lane shared-lock
  reduction per page. Resolution stays blocked behind requested debt; requested
  and resolved facts must match every reconstructed field and the stable
  timestamp/digest; same-family mismatches keep first authority. Orphan
  responses normalize against the sealed deadline, so a late raw response can
  only become the timeout outcome.
- Other scoped UserRequests keep ordered content-free requested/resolved
  publication-pending bits with their history row, cleared only after exact
  lifecycle acceptance; both debts live outside the display-history limit.
  Startup rewrites unreadable pending shards from the parsed map and removes a
  legacy source only after every target shard is durable. Response APIs reject
  incomplete principal/workspace assertions. One shared bounded sweeper handles
  lifecycle expiry; keyed reads enforce expiry before deletion.

### Structural resource limits

- **UserRequest admission:** one request ≤ 512 KiB, 32K context nodes, 128
  options; each scope's pending/history shard ≤ 512 rows and 16 MiB. An
  over-ceiling or unreadable shard stays on disk and quarantines only its scope.
  Exact-id, logical-scope and physical-scope indexes keep hot operations
  shard-bounded. Recovery quarantines a shard whose rows resolve elsewhere or
  whose directory is aliased by distinct logical scopes; conflicting duplicate
  IDs quarantine every supplying scope; a failed startup reconciliation write
  quarantines the scope and aborts its timeouts. Remaining aggregate risk: scope
  discovery uses the provider's eager filesystem listing, not a paged cursor.
- **Writer lease:** one non-waiting process-lifetime lease covers every
  UserRequest shard and both legacy files, acquired before either owner loads;
  contention or configuration misuse skips recovery and fences all durable
  mutations. It is not a downgrade fence: drain pre-lease binaries before
  upgrade and do not overlap them on rollback.
- **Generic HITL authority** is intentionally aggregate-unbounded (never
  discarded) but keyed; startup does no full replay; recovery admits ≤ 32 keys
  per page. Database size is the remaining capacity risk.
- **Refund receipts** hold only correlation and lease/CAS identity; pending
  debt drains in the caller's 32-row page; correlation-scoped pending-count and
  claim/compaction fences have their own partial index. Claim pages parse all
  rows before sampling one lease clock and roll back if auth or any lease is
  stale at commit.
- **Reducer/staging work** lives under a private dedicated root; each UUID
  operation holds a cross-process advisory lease. A teardown failure leaves an
  unlocked sentinel for a restartable background sweeper (128-entry slices,
  hourly rescan, never delays startup, never reclaims a leased item, preserves
  unknown names). Legacy flat artifacts are validated and age-gated 24 h.
- **Broadcast ring:** sealed app lifecycle events never enter the shared
  in-process ring; owner surfaces read the durable `UserRequestService` record.
  Downstream filters remain defense in depth.

## Worker and kill-switch behavior

One scope-discovery worker with bounded scope concurrency; schedule, event and
owner-notification work run in independent lanes so one stalled debt class
cannot starve another. Owner delivery and terminal-payload maintenance stay
active when the boot master is off; schedule/event claims and canonical event
admission stay closed. Scope pause narrows all lanes; revoke, disable, update,
purge, stale grants, expired auth and lease loss fail closed.

The process task supervises the worker: panic or unexpected return clears the
health bit, keeps the ownership latch, and restarts with exponential backoff
capped at 30 s (reset after a stable minute). Explicit cancellation closes
workflow runtime and canonical-event admission before releasing ownership.

Whole-installation purge deletes registry-owned schedule, event, outbox,
tombstone and quarantine rows, but reports `SchedulesAndOutbox` as
`RetainedShared` because `UserRequestService` may keep live-TTL proofs.
Compaction cursors are content-free worker progress, not installation data, and
are left in place.

`/apps/background-behaviors` is schedule health and pause control, not an
event/notification payload view. Durable rows hold bounded closed state and
reason codes; notification content never enters errors or health.

Related documents: [scheduled behaviors](app-background-behaviors-threat-model.md),
[app platform threat model](app-platform-threat-model.md),
[HITL/attention](hitl-attention.md), and
[resource authority](resource-authority-api.md).
