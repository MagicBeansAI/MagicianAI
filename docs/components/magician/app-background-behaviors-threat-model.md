# App Background Behaviors Threat Model

Normative security contract for the additive `app_behaviors_v1` increment:
manifest, reviewed grant, durable scheduler, the deterministic
zero-LLM-operation `Recipe` path and the reviewed operation-step recipe path,
behind an off-by-default boot switch. A behavior with model operations is
executable only when `behavior_execution_ready` (`apps/behavior_recipe.rs`)
accepts its reviewed steps (`runner: auto` with steps covering the
allow-set, or `runner: recipe` binding exactly one unguarded step to a
contextual round, as Town Square's `ambient_turn` does); otherwise it stays
inert (see [Behavior operation recipes](app-behavior-recipes.md)).

This specializes the [app-platform threat model](app-platform-threat-model.md)
for owner-reviewed, interval-scheduled app workflows and weakens none of its
scope, current-authority, data-handling, effect, resource or lifecycle
boundaries. Event-triggered workflows and one-way owner attention have their own
[threat model](app-events-owner-notifications-threat-model.md); they reuse this
scheduler's resource and pause spine but not its declaration or grant identity.

## Security objective

A background tick may repeat work without a user gesture but must never create
authority by repetition. A missing, ambiguous, stale, corrupt, over-budget or
unavailable dependency yields no launch and a bounded health reason.

1. A timer is a wake signal, not execution authority.
2. Every accepted fire binds one exact scope, installation generation, package
   revision, grant revision, reviewed behavior request, cadence bucket, source
   revision and resource period.
3. Stored content is untrusted data: it cannot become an instruction, select an
   operation, widen a destination or mint a contribution.
4. Only the intersection of compiled support, immutable package lock, reviewed
   behavior operations, workflow requirements, current grant, handling policy
   and remaining resource authority may execute.
5. Retry reuses the same accepted fire and idempotency identity. The durable
   head keeps only payload-free source evidence; retry rematerializes the exact
   record revision and proceeds only if content, provenance and policy still
   match. It never rebinds to newer input, resets spend or depth, or creates
   catch-up backlog.
6. Scope, installation, grant, pause and kill-switch checks reopen at final
   dispatch and at every physical effect.
7. Content, spend, active time, storage, starts, contributions and scheduler
   history are all bounded.

## Trust zones and data flow

Trusted server owners authenticate scope, resolve installation and grant, load
the immutable package, select an exact app-store revision, accept a durable
fire, admit resources, dispatch the governed workflow and settle effects. The
package, manifest prose, workflow instructions, model output, stored entity
fields, external tool results and previously generated app data are untrusted.

```text
UTC interval wake
  -> current scope/install/grant/behavior resolution
  -> typed own-store source selection at one exact revision
  -> durable fire acceptance and lease
  -> resource/start admission
  -> governed workflow (deterministic Recipe, or reviewed operation-step recipe)
  -> compiled effect/contribution owners
  -> durable settlement and bounded health projection
```

No API body, header, manifest field, model response, task row, health row or
serialized lease substitutes for any of these server-owned resolutions.

## Authority stack and kill switches

Each layer may only narrow the previous:

1. The boot-level `app_platform.background_behaviors.enabled` switch is on
   (code default off; the repo seed `magician-config.yaml` sets it on; turning it off
   stops new acceptances and authority reopening for accepted tasks). The
   process-local workflow gate is clone-shared, non-serialized, closed by default,
   and opened only after the enabled boot path builds the sole scheduler owner.
2. The authenticated scope is live and not background-paused.
3. The installation is enabled at the exact accepted generation.
4. The package still requires `app_behaviors_v1` and its manifest still has the
   exact behavior/action/operation/schema/resource request.
5. The live grant permits background execution, meets minimum interval and
   concurrency, and holds an exact behavior grant whose request digest matches.
6. Package lock, workflow, compiled effect owner, data-handling, destination and
   resource authority all admit the act.
7. A model step runs only through the operation-step owner:
   `AppWorkflowService::admit_recipe_step` re-derives the live recipe against
   the grant's `steps_digest` and the remaining per-run output tokens before
   each model call, and dispatch takes the permit-gated `app:` route. A
   zero-LLM recipe has no model edge.

The reserved `app:*` router namespace needs a runtime-only move-only dispatch
permit bound to operation, physical profile, provider kind and output-token
ceiling, minted only by the behavior LLM dispatcher after manifest, budget and
live-operation checks. An ordinary app disclosure guard is insufficient, and
every generic router entry (including logical chunking) rejects the reserved
arm. The operation-step aggregate must bind behavior declaration, admitted
operation, model context, installation, package revision, grant and execution
under one digest that the dispatcher verifies before minting, so a same-named
operation from another manifest cannot substitute a token hint, purpose or
schema.

`behavior.operations` is an unordered allow-set, never a recipe or default
choice. Without an explicit immutable operation-step binding, a scheduled
behavior is refused at the model-input boundary before the generic
`agentic_decision` lane; the runtime never picks the first operation, infers
YAML order or uses the purpose as selector. A zero-LLM behavior cannot declare
`output_schema` (that describes model output only).

Disable, quarantine, retained uninstall, update-begin, grant revocation, scope
pause and the boot switch are kill switches: an accepted or leased fire cannot
cross the next consequential boundary. Already-dispatched effects follow
existing uncertainty rules. If a current-authority read fails, the fire stays
blocked or retryable under bounded policy — never a cached grant, the manifest
request, an older generation, or success.

## Exact own-store input boundary

V1 behaviors source input only from the same scope and the same installation's
entity store. Never: another scope or installation (even the same user's);
ambient conversation, caller envelopes, task prompts, filesystem, host stores,
memory/retrieval indexes or external providers; arbitrary queries,
model-generated selectors, free-form field paths or undeclared entities; or a
record changed after the fire's source fence.

Selection is typed and compiled from the reviewed contract; the store validates
entity, record, fields, schema and bounded result. Unknown fields, open JSON
traversal, unbounded scans and selector text in interpreter position are
refused. The durable input carries the canonical app data envelope and source
identity: `AppSourceRefKind::EntityField`,
`reference = record:<blake3(canonical {entity, record_id})>`, exact positive
revision and non-empty selected field paths. The reference is provenance only;
acceptance also binds scope-binding, installation, package, schema, grant,
value-schema, content, policy and provenance digests. Input is joined with
workflow policy; model input is at least `sensitive` and at most `local_only`
unless stricter policy applies.

Selection and acceptance observe one consistent snapshot or fail closed. The
retry head stores a redacted invocation identity (revision, field list,
content/provenance/policy digests, admission timestamps), never field values.
Retries rematerialize and compare exactly; a changed, reclassified, deleted or
tombstoned record fences and clears the pending fire.

## Stored-content prompt injection

Entity text may contain forged system messages, tool calls, closing tags,
encoded instructions or prior-behavior output; any model or effect step keeps it
tainted through selection, rendering, output, contributions and receipts. Fixed
host prompt hierarchy for a reviewed operation step:

1. host system, safety, policy and effect instructions;
2. a fixed host instruction that package text and tagged content cannot override
   that authority or select tools, destinations, policy or spend;
3. the reviewed workflow instruction and output contract (semantic task, not
   host authority);
4. behavior purpose and scheduling metadata (descriptive only);
5. the canonical `<app_input>` boundary holding the typed value as data.

The lane reuses the app processing boundary: metadata and values go through its
bounded JSON writer (escaping `<`, `>`, `&`), and separately rendered text inside
an established tag passes the shared boundary-tag neutralizer (a new tag must be
added there before shipping). These are defense in depth; the runtime also keeps
source refs, labels, digests and an explicit stored-content taint bit in the
trace. A behavior never gets a generic tool catalog: only the admitted
operation; invented tools, alias disagreement, undeclared destinations or
schema-invalid output are rejected, and output mutates or contributes only
through the typed action/effect owner.

## Compiled effect ports and ceilings

No new effect primitive; the [app tool bind and contain](app-tool-bind.md)
classes apply:

| Effect class | Background rule |
|---|---|
| Pure transform / trusted local clock | Only exact compiled actions supported by the current physical owner; clock facts are host-minted. |
| Bound HTTP | Only an already-wired exact method and reviewed destination; redirects, DNS/connect identity, Host and SNI remain bound. |
| Bound file / write / table | Only the existing capability-directory or structured-table owner and its exact reviewed root, path, operation, and byte ceilings. |
| Bound host-read | Refused unless a named host-read binder independently supports the exact operation. Own-store entity input does not widen this class. |
| Bound side-effect | Only an existing governed owner with its own current authorization, spend, destination, and uncertainty contract. |
| Device | Refused in V1 unattended execution. |
| Unbound | Refused. |

Classification is not permission: execution needs `app_effect_owner_supported`,
exact argument proof, immutable implementation identity, contain profile,
package-lock and reviewed-behavior membership, and final reclassification.

The effective ceiling is the minimum of platform, manifest, requested grant,
owner-narrowed grant, package, behavior, operation, scope and period limits,
covering at least: tokens, micro-USD and active seconds per run; tokens and cost
per monthly period; starts per period and minimum interval;
foreground/background concurrency and lifetime; paid-tool invocations,
browser/network actions, payload and attachment bytes; record count and storage
bytes; causation depth, spend depth and proposals per run.

- **Reserve → I/O → settle.** Queued/paused/parked time is not active time;
  nested/parallel time uses interval union. Unknown post-dispatch outcomes stay
  reserved; only trusted no-dispatch proof releases. Retry and child work keep
  the original root and period.
- **One absolute I/O window.** The final fence samples the remaining window and
  every LLM, store, compiled, governed, OS-jail, browser, macOS and Android owner
  is bounded by it. Before physical start, release needs no-dispatch proof;
  after start, the owner is cancelled where possible and settles uncertain —
  never a proven-unspent refund. Abort-before-I/O readback rebuilds the
  admitted-active interval union from the journal and refreshes the root cache
  with the journal fence.
- **Two monthly totals.** The installation-period projection enforces the
  app-wide grant across foreground, event and behavior work; a second indexed
  total over roots with the same scheduler-sealed behavior ledger digest
  enforces the behavior's owner-narrowed token/cost ceilings. The ledger digest
  binds behavior id + reviewed request (narrowing cannot reset spend); a
  separate authority-binding digest freezes the granted behavior and limits into
  the run binding and resource-tree identity. Ordinary roots omit the
  discriminator; a missing, mismatched or mutable one is corrupt authority.
  Historical V22 behavior roots whose run binding and resource tree both lack it
  may only finish accounting cleanup — never dispatch, reserve or resume.

### Unattended spend

A non-zero behavior cost ceiling is consent only for that reviewed behavior,
operation set, interval, destination policy and period — not an app wallet,
reusable foreground approval or model-selected purchase. A zero remaining balance
stays zero even for "free" operations. If an effect owner needs live
confirmation, external auth or cart cross-check, the fire parks or fails with the
existing typed disposition; consent is never inferred from timeout, prior
approval, stored text or purpose. Remote success followed by response or
persistence loss is charged and uncertain, never replayed.

**Period ceilings park, not retry.** Root admission checks monthly ceilings with
a zero-size candidate, so a period just under `max_monthly_tokens` admits a
launch whose first model attempt is refused; rescheduling on the interval would
repeat that forever. A refusal on `monthly_tokens` or `monthly_cost` marks the
run (`period_ceiling_breach`); the recurring observer reads it off the terminal
run (status `completed`) and moves the head's next fire to the resource period
end (first of the next UTC month, `resource_period_end`) with `last_error`
`period_resource_ceiling:<ceiling>` and one warning. Per-run ceilings
(`no_progress`, `active_time`) fail that run and reschedule normally.

## Causation, spend depth, and self-amplification

The grant records causation- and spend-depth ceilings. Every behavior model
turn is a root fire: the dispatch budget is built with a constant depth of 0
(`BEHAVIOR_ROOT_CAUSATION_DEPTH`, `artifact_v2/service.rs`), steps of one
recipe do not add hops, and a background-origin terminal cannot fire another
behavior, so the reviewed ceiling bounds nothing yet. Multi-hop causation must
seal root fire, origin
behavior, parent cause and both depths, increment every expanding edge, keep
generated-source provenance, and refuse before dispatch at the lowest ceiling;
content, output, contributions and mutations can never supply or reset them.

The V1 scheduler collapses missed intervals to at most one due candidate, never
catches up per bucket, self-schedules or turns output into cadence. At most the
granted overlap runs. Root admission advances the fixed-period start counter
under the live scheduler fence; crash recovery does so only from an exact
Artifact TaskState root-acceptance proof plus the correlated task binding (a
binding alone precedes root start and is not proof).

## Storage and contribution volume

Recipe store mutations (including a contextual round's validated drafts) commit
only through the App mutation owner. Mutations must stay within the package schema and exact
action, charge record/byte ceilings before commit, and reuse one mutation
identity across retries. Contributions are a destination-owned port: a step may
propose only to a reviewed port, and the contribution owner reopens authority,
validates handling and retention, and applies the behavior's per-run proposal
ceiling plus the port's ceilings. The proposal ceiling is checked against the
exact planned terminal projection (a create without summary may yield none; an
update without summary yields one invalidation per matching port) before any
commit intent. A legacy committed mutation whose recovered projection exceeds
the ceiling terminalizes with the entire contribution set suppressed.

**Scheduler bounds.** The registry caps behaviors per package, one live head per
behavior, numeric failure counters, closed error codes and retained events.
Reconciliation and due work use bounded indexed pages. A reconciliation pass
persists start, forward cursor and provisional debt before each installation
(separate from the last completed-pass time), so cancellation resumes after that
installation; leftover debt at inventory end starts another bounded sweep, so a
slow app cannot hide a newly enabled one. Inventory slices and per-installation
work have separate ceilings; due heads keep a separate fairness cursor. Retry is
capped exponential backoff (1 h), not a dead letter. Input and secrets never
enter heads or events.

**Historical acceptance first.** Recovery of accepted fires runs before
package/schema/grant/behavior reconciliation, including for disabled
installations: expired pending heads are probed for their exact Artifact root and
active task binding, proven starts settle, and only then are unaccepted heads
retired. Live leases are kept until settlement or expiry. Each candidate takes
Artifact's per-task start-admission lock before observing TaskState; the guard
moves into the blocking registry transition and survives waiter cancellation. An
opaque durable cursor advances before parsing or lock waits, so one malformed row
or contended task cannot block others.

Fresh root admission takes the same lock; its final registry snapshot (after
recipe reservations) requires an unpaused scope, the installation enabled at the
exact generation and package, the exact active grant and schema, the behavior
digest and fire identity, and a live pending lease; the lock is released only
after the Artifact root commit and worker spawn. Source removal, contract block,
update, disable and other destructive transitions use the same fenced
settlement; ordinary reconciliation cannot clear `pending`.

**Scope discovery** stops at the physical scan ceiling (excess reported as
capacity debt), batches principal pages with a fixed directory-page budget per
refill, and isolates an unreadable principal or metadata entry for the pass
(an unreadable scope root fails closed). Scope work has fixed cross-scope
concurrency and a per-scope wall-clock budget; timeout stops new local work, an
admitted blocking transition keeps its guard until commit, and a pre-admission
cancel leaves the durable lease to recover.

## Durable fire, lease, retry, and idempotency

```text
fire ref: installation + generation + package + grant + behavior
          + reviewed request digest + scheduled UTC boundary
task id:  scope + installation + action + fire idempotency + launch ref
head:     digest-bound source envelope + source policy
```

Acceptance compare-and-appends a pending fire (authority/source/resource
identities, content-free source evidence and digest, monotonic revision) before
dispatch. A short TTL lease names the worker and a monotonic fence token; lease
acquire/renew/transition/settle are revision CAS, so an expired worker cannot act.
A package or grant revision cannot replace a head while its lease is live. Once
expired, the head is checked for exact root acceptance before any reset,
abandonment or retirement, under one Artifact start-admission guard (if the
workflow publishes the root first, recovery settles acceptance).

Scheduling is interval-only UTC: no cron, timezone, DST or unbounded catch-up.
Restart computes the latest eligible bucket, collapses misses to one, and
deduplicates by deterministic identity.

- Before acceptance: no fire, no effect.
- After acceptance, before admission: reconciliation re-leases the same fire.
- After admission, before settlement: recovery reads the deterministic
  task-binding control and requires a durable latest root in Artifact TaskState
  before rematerializing; a binding without a root stays the same retryable
  fire.
- After a future physical effect starts, before response: the effect receipt
  and uncertainty state decide; no blind refire.
- After terminal settlement: replay returns the same result.
- After generation, grant, pause or kill-switch change: the fire is
  blocked/stale.

A future operation aggregate must derive every mutation, proposal, LLM attempt
and effect idempotency identity from the fire and a stable step ordinal.
Provider exactly-once is not claimed without provider idempotency; such outcomes
stay uncertain and consume their reservation.

## Scope isolation

Scope is part of every durable key, lookup, lease, source fence, resource root,
idempotency key and health query; bare behavior/action/task/installation/record
ids are never globally unique. The worker runs from a server-authenticated scope
carrier and never takes principal/workspace from a package or payload. Registry
and entity stores are SQLCipher/scoped; mismatch, missing key, decryption failure
or inconsistent fence is a hard refusal, not an empty result. Cross-install
composition, shared host reads, Town Square feeds and owner-wide
memory/retrieval are outside the own-store boundary.

A maintenance worker cannot resolve execution authority; only the workflow
owner's validated background launch token mints the short-lived
`ReviewedBackgroundLaunch` scope for one installation (no generic worker
execution, interactive approval or meeting-capture intent). Grants, scope pause,
exact scheduler fire and resource leases stay mandatory at launch.

## Health and operator visibility

The owner/operator projection is bounded and metadata-only. Head state is
`idle`, `pending` or `blocked`; scope pause is separate. Items expose only
installation/behavior identity, generation and package revision, revision and
fence, effective interval, next-due/available/lease times, pending fire ref,
accepted workflow-root count, bounded attempt/failure counters, period start
counters, update time and a closed sanitized failure code. `accepted_count`
means Artifact durably published the root, not that work or an effect completed.

It also returns the scope-pause revision, process-local `worker_running`, a fixed
recovery posture, corruption counts and ≤ 512 sanitized events.
`worker_running` comes from the scheduler that produced the snapshot (marked by
the supervisor for each worker attempt) and is never patched by callers. A worker
failure stops new claims but keeps health and pause control, since accepted
workflows still reopen the durable pause row. Events are a bounded recent feed
repeated per item page. Item pages are live best-effort reads (a fresh poll is
needed to see concurrently moved heads); the physical cursor can skip a corrupt
row.

Never exposed: record values, prompts, model output, tool payloads, secrets,
credentials, raw error bodies, capability tokens, SQLCipher keys, HMAC material.
Health is a projection; editing or deleting it authorizes nothing. Recovery is
report-only without proven exclusive authority; operators may pause, disable,
revoke or inspect, but the API mints no retry and repair never synthesizes a
grant, source revision, refund or success.

## Threat-to-control matrix

| Threat | Required control | Fail-closed result |
|---|---|---|
| Package requests a fast or expensive loop | Closed interval vocabulary; requested and granted per-behavior ceilings; owner may only narrow | Install/review refusal or no admission |
| Timer runs a revoked or updated app | Current generation/grant/behavior digest recheck at acceptance and final I/O | Fire becomes stale/blocked |
| Stored record says to ignore policy or call a tool | Typed selection, taint/provenance, boundary neutralization, fixed instruction hierarchy, exact projected operations | Text remains data; invented call refused |
| Generated output triggers recursive spend | Behavior model turns are root fires (depth 0) and a background-origin terminal cannot fire another behavior; start/monthly caps remain independent | No executable expansion edge |
| Restart creates a catch-up storm | UTC interval buckets, one collapsed miss, deterministic acceptance id | At most one due fire |
| Two workers accept the same bucket | Scope-qualified CAS acceptance and fenced TTL lease | One live fence; stale worker refused |
| Crash follows workflow admission | Exact historical task-id reconstruction plus Artifact-published root and matching active immutable task binding, all before source rematerialization | Counters settle; a binding file alone cannot consume the fire |
| Future effect loses an external response | Effect receipt and conservative uncertainty settlement are a reopen gate | No effect lane until recovery is exact |
| Behavior reads another app/scope | Same-install typed source resolver and scope in every key | Hard scope/source refusal |
| Model widens effect or destination | Compiled owner predicate, exact argument proof, package lock, reclassification, one-shot effect permit | Effect not dispatched |
| Behavior floods entity/contribution/scheduler storage | Record/byte/proposal/fire/retry/history ceilings and bounded indexed scans | Mutation/proposal/fire refused |
| Kill switch races a lease | Current pause/lifecycle/grant/resource fence at final consequential boundary | Not-yet-dispatched work stops |
| Health or logs leak sensitive input | Metadata-only bounded projection and closed error codes | Content omitted |

## Explicit deferrals

Outside `app_behaviors_v1`:

- cron/calendar/timezone expressions, DST policy, per-missed-bucket catch-up;
- manifest indicators and Town Square feed consumption;
- cross-app, cross-installation, cross-scope, host-store, memory-index or
  arbitrary external input;
- unattended device/UI automation and generic raw shell, SQL, filesystem,
  network or MCP lanes;
- multi-host scheduling, remote lease consensus, sharding, and transactional
  outbox delivery across independent stores;
- provider exactly-once effects without provider idempotency;
- multi-hop behavior causation (carrying the fire row's recorded depth into
  dispatch), generated-source lineage and child idempotency identities;
- recovery that invents authority or treats a completed child as proof of safe
  parent retirement;
- mobile OS background-execution integration.

Related contracts: [app entity store](app-entity-store.md),
[app-platform contract kernel](app-platform-contract-kernel.md),
[resource authority](resource-authority-api.md), and
[flat-loop execution](execution/FLAT_LOOP.md).
