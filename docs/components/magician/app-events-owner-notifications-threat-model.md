# App Events And Owner Notifications Threat Model

Schedule and event execution sit behind the
`app_platform.background_behaviors.enabled` boot master. It defaults to
`false` in code (`AppBackgroundBehaviorSettings::default`), but the repo-root
`magician-config.yaml` seed sets it `true`. Foreground owner-notification debt
always keeps a delivery owner.

## Scope

Two independent manifest/review/runtime axes:

- `app_event_behaviors_v1`: one same-installation canonical root execution
  terminal can start an exact reviewed event behavior;
- `app_owner_notifications_v1`: an app workflow may send one-way,
  workflow-scoped briefing or escalation content into the owner's attention
  funnel.

Neither is a generic event bus, push channel, approval protocol,
question/response primitive or authority transfer.

Out of scope for V1: started, cancelled, delegated-child, transport-only and
cross-app event kinds; app-authored events, arbitrary filters and
cross-installation fanout; multi-hop causation or spend propagation;
notification questions, approvals, critical severity, phone push and app
response authority; distributed workers and provider-level exactly-once claims.

## Trust boundaries

Trusted: the canonical Artifact journal and projector, workflow task and run
sidecars, the authenticated scope service, the package lock, registry
transactions, reviewed grants, resource authority and the shared
`UserRequestService`. Untrusted: app manifests, stored values, workflow
instructions, model and tool output, notification text, UI-thread strings,
serialized leases and transport events.

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
   never the live broadcaster.
2. Installation identity comes from the sealed app task binding, and the
   execution must be the canonical root. UI-thread text is only a consistency
   check.
3. V1 projects only `execution_ref`, `succeeded|failed` and recorded time, in a
   host-built, non-deserializable DTO with Sensitive/LocalOnly floors.
4. Scheduled- and event-origin terminals are suppressed, so V1 is one hop and
   cannot recurse.
5. One immutable receipt records each installation/event-ref projection and its
   exact fanout (including zero). Replay must match bytes, digest, kind and fire
   count; a purge or disable race acknowledges without recreating bytes.
6. Fanout ≤ 32. Replay uses a `(installation_id,event_ref)` index. Claims are
   ≤ 32 per scope tick, the lease 30-600 s (default 120 s), the retry delay
   5 s-1 h (default 30 s) and attempts ≤ 64 (`AppEventBehaviorRuntimeLimits`).
7. Rate identity is per installation and event-behavior id, so consumed starts
   survive package or grant changes and a new ceiling applies to work already
   consumed. `last_started_at` is kept apart from the period, so rollover cannot
   reset `min_interval_seconds`; invalid timestamps fail closed.
8. Rate admission, launch identity and attempt charging share the final
   pre-launch transaction; deferred, paused, timed-out or unstarted work
   consumes no attempt.
9. Every dispatch and workflow root reopens scope, pause, enabled generation,
   package, schema, grant/review digest, source policy, resource authority, lease, fence and expiry. Unsupported
   profiles are dead-lettered with closed reason codes.
10. Corrupt rows are quarantined one by one; a bad row cannot roll back a claim
    page or strand later leases.
11. After the 7-day terminal retention window, compaction may remove projection
    and fire payloads only when fanout is exact and every fire is terminal with
    the same kind and digest. A digest/fanout tombstone stays the replay
    authority until installation purge.
12. Each compaction candidate has its own savepoint; a failed integrity proof
    rolls back that candidate, keeps its evidence and writes a content-free
    quarantine marker.
13. Candidates are found through durable raw identity cursors: forward epochs
    plus a cyclic revisit rail served after ≤ 64 forward pages, so skipped young
    rows are revisited without stalling. ≤ 32 receipts per sweep; malformed
    timestamps go to quarantine; deleted cursor keys remain valid seek positions.

## Notification invariants

1. A notification command exists only after the workflow owner reopens the
   exact task, workflow, compiled `notify_owner` port, manifest, generation and
   current grant.
2. V1 allows only `briefing|escalation`, `info|warning`, a title ≤ 160 bytes
   and message ≤ 4 KiB, a reviewed purpose, a stable effect identity and content-free app acceptance.
   `question`,
   `critical`, response routing and free-text replies are refused.
3. Acceptance, idempotency, period volume, the pending ceiling, TTL and outbox
   insert happen in one transaction; changed bytes under one effect id fail
   closed.
4. Period usage is keyed by installation/workflow/port, not review digest, and
   a shorter replacement period inherits the longest active window, so
   re-review cannot mint fresh allowance.
5. `max_pending` counts pending, leased and delivered-unexpired attention, so
   delivery cannot evade the ceiling.
6. **Capacity refunds.** Claiming charges nothing; the lease is revalidated and
   charged just before `UserRequest` submission; pause preserves debt. A
   scope-capacity refusal publishes a unique content-free refund receipt before
   the debit. Pending receipts fence claim and attempt-ceiling selection and are
   repaired in fixed pages; an active successor is never revoked. Receipt
   identity prevents double decrement. Retry backoff runs 5 s to 1 h and never
   crosses absolute expiry, so a 31-day TTL bounds a correlation to 753
   receipts. The charged row returns to pending, advances its fence and clears
   its lease. Process loss or auth expiry before the first receipt commit may
   consume the attempt; after commit, debt stays durable and fenced.
7. The outbox settles to delivered only after `UserRequestService` has durably
   stored the pending request; otherwise the lease is recovered.
8. The absolute outbox expiry drives live and restored UserRequest timers, so
   late delivery cannot extend the TTL. Marker-bearing rows require exact host
   source and sealed expiry; malformed restored rows are redacted
   into cleanup debt.
9. Resolved content is replaced at once by an identity/scope/digest/expiry
   tombstone kept until the TTL.
10. Owner responses never route to an app task; the app gets neither content
    nor a resume signal.
11. A terminal outbox row keeps its payload through its TTL and reviewed
    period; only then may the compactor swap it for a payload-digest tombstone.
    A changed payload or authority fails closed until purge.
12. Digest, payload, severity and correlation are re-proved before compaction;
    failure takes the per-row rollback and quarantine path.
13. Notification compaction uses the same forward/revisit discovery over raw
    `correlation_id` order, decides eligibility inside the transaction over a
    fixed 32-row page and revalidates authority before every commit, so an
    expiring credential cannot publish tombstones, delete payload or advance a
    cursor.

### Body custody (invariant 14)

- The app outbox owns the source payload through its retention window.
  `UserRequestService` is the only durable owner of the delivered attention
  projection; no generic sink mirrors the body.
- The private keyed HITL lifecycle store (V3 SQLite, `0600` files, DELETE
  journal) keeps only an expiry-bound content-free pending/resolved proof and a
  random owner generation stored beside the UserRequest body to prevent
  substitution. It keeps no body and no body digest (the fixed `HitlResolved`
  envelope fingerprint is not one). Restore never turns a pending proof back
  into content until the live owner republishes the exact generation and
  deadline.
- Reconciliation hands out a short-lived ticket holding the publication lock,
  bound to its broadcaster and each event's length and SHA-256. Contention
  drops authority and retries instead of holding the file lock across an await.
  Exact reads refuse content at absolute expiry.
- Startup never scans the whole table. Cleanup deletes ≤ 512 expired rows per
  pass and vacuums a fixed page slice, re-armed from indexed `MIN(expiry)`.
- The one-time legacy JSONL import commits, archives the JSONL and replaces its
  path with a private `0700` directory before readiness, which fences older
  binaries from appending. Pre-ready crashes reimport the archive; post-ready
  startup refuses a missing or substituted
  fence.

### Sealed marker in generic sinks (invariant 15)

The workspace transport log, generic progress EventLog, ChatStore escalation
projection, per-chat-turn JSONL fanout, mobile push dispatcher and process
debug log recognize the host-sealed notification marker and refuse the body.
The workspace-log writer rewrites legacy copies at startup in bounded memory,
dropping only malformed or oversized rows. `/events` backfill and
`internal_data` event-log readers withhold marked and unclassifiable rows; the
live `/events` tail refuses this surface. Execution-index JSONL is outside the
filter. Append, scrub and compaction share a cross-process lock, and scrub
checks source length, digest and file identity before replacing.

### Resolution and restart (invariant 16)

- Resolution removes the live prompt even if lifecycle publication fails after
  the response commit; startup retries the content-free resolution, first
  re-establishing the pending proof if needed. A generic or other-app lifecycle
  under the same key is preserved and refuses the transition.
- Acceptance and restore establish durable UserRequest ownership and arm the
  original timeout first. Reconciliation never broadcasts a saved body itself;
  the worker revalidates the live request and calls a lifecycle-locked
  broadcaster, so a response or expiry that wins during IO suppresses late
  publication.
- Request and resolution retries run through coalesced 32-entry queues per
  scope. Resolution stays blocked behind requested debt. Requested and resolved
  facts must match every reconstructed field and the stable timestamp/digest;
  same-family mismatches keep first authority. Orphan responses normalize against the
  sealed deadline, so a late raw response can only become the timeout outcome.
- Other scoped UserRequests keep content-free publication-pending bits with
  their history row, cleared only after exact lifecycle acceptance and kept
  outside the display-history limit. Startup rewrites
  unreadable pending shards and removes a legacy source only after every target
  shard is durable. Response APIs reject incomplete principal/workspace
  assertions. One shared sweeper handles lifecycle expiry; keyed reads enforce
  expiry before deletion.

### Structural resource limits

- **UserRequest admission:** one request ≤ 512 KiB, 32K context nodes, 128
  options; each scope's pending/history shard ≤ 512 rows and 16 MiB. An
  oversized or unreadable shard stays on disk and quarantines only its scope.
  Exact-id, logical-scope and physical-scope indexes keep hot operations
  shard-bounded. A shard whose rows resolve elsewhere or whose directory is
  aliased is quarantined; conflicting duplicate IDs quarantine every supplying
  scope; a failed startup reconciliation write quarantines the scope and aborts
  its timeouts. Remaining risk: scope discovery uses an eager filesystem listing, not
  a paged cursor.
- **Writer lease:** one non-waiting process-lifetime lease covers every
  UserRequest shard and both legacy files, acquired before either owner loads;
  contention or configuration misuse skips recovery and fences
  all durable writes. It is not a downgrade fence: drain pre-lease binaries
  before upgrade and do not overlap them on rollback.
- **Generic HITL authority** is deliberately unbounded in aggregate (never
  discarded) but keyed; startup does no full replay; recovery admits ≤ 32 keys per page. Database size is the remaining capacity
  risk.
- **Refund receipts** hold only correlation and lease/CAS identity; pending debt
  drains in 32-row pages, and correlation-scoped pending-count and
  claim/compaction fences have their own partial index. Claim pages roll back if auth or
  any lease is stale at commit.
- **Reducer/staging work** lives under a private root with one cross-process
  lease per operation. A failed teardown leaves a sentinel for a background
  sweeper (128-entry slices, hourly, never delays startup, never reclaims a
  leased item, preserves unknown names). Legacy flat artifacts are validated
  and age-gated 24 h.
- **Broadcast ring:** sealed app lifecycle events never enter the shared
  in-process ring; owner surfaces read the durable `UserRequestService` record.
  Downstream filters remain defense in depth.

## Worker and kill-switch behavior

One scope-discovery worker with bounded concurrency runs schedule, event and
owner-notification work in separate lanes, so one stalled class cannot starve
another. With the boot master off, owner delivery and terminal-payload
maintenance keep running while schedule/event claims and event admission stay
closed. Scope pause narrows all lanes; revoke, disable, update, purge, stale
grants, expired auth and lease loss fail closed.

A panic or unexpected return clears the health bit, keeps the ownership latch
and restarts the worker with exponential backoff capped at 30 s (reset after a
stable minute). Explicit cancellation closes workflow
runtime and event admission before releasing ownership.

Whole-installation purge deletes registry-owned schedule, event, outbox,
tombstone and quarantine rows, but reports `SchedulesAndOutbox` as
`RetainedShared` because `UserRequestService` may keep live-TTL proofs.
Compaction cursors are content-free progress and stay.

`/apps/background-behaviors` is schedule health and pause control, not a
payload view. Notification content never enters errors or health.

Related documents: [scheduled behaviors](app-background-behaviors-threat-model.md),
[app platform threat model](app-platform-threat-model.md),
[HITL/attention](hitl-attention.md), and
[resource authority](resource-authority-api.md).
