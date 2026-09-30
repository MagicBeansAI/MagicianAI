# Recurring App tasks

A scheduled App behavior owns one internal Artifact task per authenticated
scope, installation and behavior. Each occurrence owns a separate root execution.
Direct actions and event deliveries retain their existing invocation identities.
No package version change is needed to use this scheduler behavior.

The task is a history container. It does not share an occurrence's authority or
budget with a later occurrence. Each fire retains immutable, sealed bindings to
its input revision, idempotency key, package, grant, agent definition, resource
ledger and root execution. Runtime and child-execution reads resolve that root's
binding. Results live under the execution directory. Public run references carry
both the task and occurrence identity; reading or cancelling an old run cannot
select a later root.

Historical recipe control derives the exact run directory from its canonical
root execution ID and validates the retained lifecycle/binding pair. It does not
scan or cap the task's total run history. The pending-reservation ceiling counts
only unaccepted reservations; retired history does not consume that capacity.
Older roots without canonical recipe IDs retain their bounded compatibility
lookup. Startup enumeration and legacy control resolve lifecycle files beneath
the workspace task directory; provider-relative directory entries must never
be passed directly to file-lock or workspace I/O operations.

Registry schema 37 adds immutable occurrence locators, a current-occurrence
pointer and per-behavior execution health. Existing TaskBinding control records
remain immutable. Replaying an old occurrence does not move the current pointer
backwards. The original per-fire identity remains the delivery locator used to
recover acceptance after a process interruption.

Archived execution bindings use the same bounded task-metadata admission as the
current binding sidecar. They do not use the smaller limit for control tokens:
sealed agent, prompt and recipe material routinely exceeds that token limit.
Decoding and seal authentication run on the bounded blocking lane, returning a
boxed binding. Recipe/store calls must not deserialize retained agent definitions
on their already occupied execution-worker stack.

## Scheduling and health

The scheduler observes the recurring task before claiming another fire. A live
execution or unsettled terminal resource tree blocks the next occurrence without
creating another task or execution. The next due time is the terminal completion
time plus the reviewed interval. Missed ticks coalesce; they are not a catch-up
queue. The start-admission lock and a separate occurrence-publication lock guard
cross-process races.

Reopening the container checks its previous execution under the already-held
task write guard after journal recovery. It must not call the public execution
reader there, because that reader takes the same lock.

Startup terminal outcome writes and resource settlement run as awaited execution
jobs. Boxing a future alone does not give its poll function a fresh native stack,
and nesting it beneath the large startup recovery state machine overflows the
default stack. The execution-job boundary keeps the normal worker stack
size and propagates cancellation when the per-task recovery budget expires.

Observation follows the currently sealed occurrence's root, not the task's
previous root pointer. If a restart occurs after occurrence publication but
before Artifact acceptance, the missing new root remains retryable under the
same occurrence. Observing the previous completed root would otherwise block
that recovery indefinitely.

An indexed observation flag keeps running or unsettled executions eligible for
the scheduler's next scan, including runs that finish before their original
next-due time. Settled outcomes are counted once, after resource recovery, so
late settlement cannot lose committed-record counts.

Health retains launch acceptance counts separately from completed, partial and
failed execution counts and actual committed records. An accepted launch is not
a completed round or a published post. A successful social round may choose to
remain quiet. Cleanup failures retain a recovering state and block a new root.

Scheduling, history reads and outcome bookkeeping perform no model calls. App
semantic processing continues through the governed execution runtime and normal
LLM dispatcher, with a separate ledger for each root.

## History and maintenance

`GET /api/magician/v3/tasks/{task_id}/details` returns at most 25 enriched executions
by default (`limit` is clamped to 1–100), newest first. Supply the returned
`next_execution_cursor` to load older records. `execution_total` reports the
number of lightweight index entries; only the requested page hydrates execution
payloads. The index is still read in memory, so this bounds payload I/O rather
than claiming constant-cost index access.

Recurring details also include `recurring_schedule`: the reviewed interval,
latest execution status and next eligible time. While a run is active or awaiting
settlement, the next eligible time is absent and the UI explains that it waits
for the current run.

The owner-only maintenance route
`DELETE /api/magician/v2/apps/maintenance/action-runs/{run_ref}` removes an obsolete
per-fire scheduled task only after all executions are terminal and resource
settlement succeeds. It rejects the persistent recurring task, direct actions
and event deliveries. Canonical deletion fences late writers and removes index
rows; App entities and resource/control audit journals remain intact. This route
is a first-party owner maintenance operation, not an App capability. Generic task
lifecycle APIs continue to reject App-owned tasks.

The scoped verification helper is `scripts/verify-recurring-app-tasks.py`.
Its `login` command creates a normal private session; read commands accept
`--session`. `apps` reads the installed-app directory for activation verification.
`dispatch --task <id>` reads scoped, content-free LLM facts and
groups call and dispatcher-job counts by root execution, provider and model.
`queue --task <id>` captures content-free live dispatcher job evidence for that
exact scoped task, including execution identity, provider, model and outcome.
Use it to verify routing when the historical facts projection has missing
correlation rows; an empty facts query alone does not establish a queue bypass.
After correcting a refused launch, `retry-blocked` uses the existing owner retry
route with the observed installation generation and scheduler revision. It only
requeues a `workflow_launch_blocked` ambient fire, or a blocked
`workflow_execution_failed` occurrence after its exact current root is terminal
and fully settled. Stale observations and pending cleanup cannot authorize a
retry. Budgets, counters and the completion-based due time remain intact; this
is not a run-now command. Execution outcomes remain in `recurring.latest` and
do not overwrite a separate scheduler block reason. Resource admission denial
logs include installation and scheduler occupancy counts for diagnosis.
After live verification, `cleanup --verified-recurring-task <id>
--session <file> --out <evidence.json>` records incremental deletion receipts
and verifies all pre-existing unrelated tasks and post IDs remain. It refuses
cleanup until two recurring occurrences completed and at least one post was
committed. The helper is specifically for the authorized anonymous/default
ambient-task migration; the maintenance API itself supports scoped scheduled
App tasks generally.

## Verification

Run `make check-app-recurring`, `make test-app-recurring`,
`make test-app-recurring-ui` and `make check-ui`, using the SSD1 build and compiler
temporary directories. The Rust target passes both the recurring and owner-retry
filters to one test executable in one Cargo invocation. Completion record:
archived plan.
