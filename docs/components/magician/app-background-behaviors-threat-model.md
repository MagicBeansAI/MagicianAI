# App Background Behaviors Threat Model

Normative security contract for `app_behaviors_v1`: manifest, reviewed grant,
durable scheduler, the deterministic zero-LLM-operation `Recipe` path and the
reviewed operation-step recipe path, behind an off-by-default boot switch. A
behavior with model operations runs only when `behavior_execution_ready`
(`apps/behavior_recipe.rs`) accepts its steps (`runner: auto` covering the
allow-set, or `runner: recipe` binding one unguarded step to a contextual round,
as Town Square's `ambient_turn` does); otherwise it is inert
([Behavior operation recipes](app-behavior-recipes.md)).

It narrows the [app-platform threat model](app-platform-threat-model.md);
event-triggered workflows have their own
[threat model](app-events-owner-notifications-threat-model.md) and reuse this
scheduler's resource and pause spine, not its declaration or grant identity.

## Security objective

A timer is a wake signal, never authority; repetition never creates authority,
and any missing, ambiguous, stale, corrupt, over-budget or unavailable
dependency yields no launch and a bounded health reason. Every accepted fire
binds one exact scope, installation generation, package revision, grant
revision, reviewed behavior request, cadence bucket, source revision and
resource period. Stored content is data and cannot instruct, select an
operation, widen a destination or mint a contribution. Only the intersection of
compiled support, package lock, reviewed operations, workflow requirements,
current grant, handling policy and remaining resources may execute. Retries
reuse the same fire and idempotency identity and never rebind to newer input,
reset spend or depth, or create catch-up backlog. Authority reopens at final
dispatch and every physical effect, and every resource and history is bounded.

## Trust zones and data flow

Package, manifest prose, workflow instructions, model output, stored fields,
tool results and prior app output are untrusted; server owners resolve each
step, and no API body, header, manifest field, model response, task row, health
row or serialized lease substitutes for them:

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

## Authority stack and kill switches

Each layer only narrows:

1. Boot switch `app_platform.background_behaviors.enabled` (code default off;
   the repo seed `magician-config.yaml` sets it on; off stops acceptances and
   authority reopening). The process-local workflow gate is clone-shared,
   non-serialized, closed by default, and opened only once the enabled boot path
   builds the sole scheduler owner.
2. Scope live and not background-paused.
3. Installation enabled at the accepted generation.
4. Package still requires `app_behaviors_v1` with the exact
   behavior/action/operation/schema/resource request.
5. Live grant permits background execution, interval and concurrency, with a
   matching behavior request digest.
6. Lock, workflow, effect, handling, destination and resource owners admit.
7. Model steps run only via `AppWorkflowService::admit_recipe_step`, which
   re-derives the recipe against the grant's `steps_digest` and remaining
   per-run output tokens before each call, on the permit-gated `app:` route.
   Zero-LLM recipes have no model edge.

The reserved `app:*` router namespace needs a runtime-only, move-only permit
bound to operation, physical profile, provider kind and output-token ceiling,
minted only by the behavior LLM dispatcher after manifest, budget and
live-operation checks; an app disclosure guard is insufficient and generic
router entries (including chunking) reject it. The operation-step aggregate's
digest, verified before minting, binds declaration, operation, model context,
installation, package revision, grant and execution, so another manifest's
same-named operation cannot substitute a token hint, purpose or schema.

`behavior.operations` is an unordered allow-set. Without an immutable
operation-step binding a behavior is refused before the generic
`agentic_decision` lane; the runtime never picks by order or purpose. Zero-LLM
behaviors cannot declare `output_schema`.

Kill switches (disable, quarantine, retained uninstall, update-begin, grant
revocation, scope pause, boot switch) stop an accepted or leased fire at the
next consequential boundary; already-dispatched effects follow uncertainty
rules. A failed authority read blocks or retries, never falling back to a
cached grant, the manifest request, an older generation or success.

## Exact own-store input boundary

Input comes only from the same scope and installation's entity store, by
reviewed typed selection: never another scope or installation (even the user's
own), conversation, caller envelopes, task prompts, filesystem, host stores,
memory/retrieval or external providers; never arbitrary queries, model
selectors, free-form paths, undeclared entities or a record changed after the
source fence.

The store validates entity, record, fields, schema and bounded result, and
refuses unknown fields, open JSON traversal, unbounded scans and selector text
in interpreter position. Input carries the canonical app data envelope and
`AppSourceRefKind::EntityField` with `reference = record:<blake3(canonical
{entity, record_id})>`, an exact positive revision and non-empty field paths.
The reference is provenance only; acceptance also binds scope-binding,
installation, package, schema, grant, value-schema, content, policy and
provenance digests. Model input is at least `sensitive` and at most `local_only`
unless stricter policy applies.

Selection and acceptance share one snapshot or fail closed. The retry head keeps
revision, fields, digests and admission timestamps, never values; retry
rematerializes that exact revision, and a changed, reclassified, deleted or
tombstoned record clears the pending fire.

## Stored-content prompt injection

Entity text stays tainted through rendering, output, contributions and
receipts. Prompt hierarchy for a reviewed step:

1. host system, safety, policy and effect instructions;
2. a fixed instruction that package text and tagged content cannot override
   that authority or select tools, destinations, policy or spend;
3. the reviewed workflow instruction and output contract (semantic task, not
   authority);
4. behavior purpose and scheduling metadata (descriptive only);
5. the canonical `<app_input>` boundary holding the typed value as data.

Values pass the app boundary's JSON writer (escaping `<`, `>`, `&`); text inside
an established tag passes the shared boundary-tag neutralizer (a new tag must be
added there before shipping). The trace keeps source refs, labels, digests and a
stored-content taint bit. A behavior never gets a generic tool catalog, only its
admitted operation; invented tools, alias disagreement, undeclared destinations
and schema-invalid output are rejected, and output acts only through the typed
action/effect owner.

## Compiled effect ports and ceilings

No new effect primitive; [app tool bind and contain](app-tool-bind.md) classes
apply:

| Effect class | Background rule |
|---|---|
| Pure transform / trusted local clock | Exact compiled actions of the current owner; clock facts host-minted. |
| Bound HTTP | Wired exact method and reviewed destination; redirects, DNS/connect, Host and SNI stay bound. |
| Bound file / write / table | Existing directory or table owner with exact root, path, operation and byte ceilings. |
| Bound host-read | Refused unless a named binder supports the exact operation; own-store input does not widen it. |
| Bound side-effect | Existing governed owner with its own authorization, spend, destination and uncertainty contract. |
| Device | Refused in V1 unattended execution. |
| Unbound | Refused. |

Classification is not permission: execution also needs
`app_effect_owner_supported`, exact argument proof, immutable implementation
identity, contain profile, package-lock and reviewed-behavior membership and
final reclassification.

The effective ceiling is the minimum across platform, manifest, grant (requested
and narrowed), package, behavior, operation, scope and period, covering per-run
tokens, micro-USD and active seconds; monthly tokens and cost; starts and
minimum interval; concurrency and lifetime; paid tools, browser/network actions,
payload and attachment bytes; records and storage bytes; causation depth, spend
depth and proposals per run.

- **Reserve → I/O → settle.** Queued/paused/parked time is not active;
  nested/parallel time uses interval union. Unknown outcomes stay reserved
  unless no-dispatch is proven; retries and child work keep the root and period.
- **One absolute I/O window.** The final fence's remaining window bounds every
  LLM, store, compiled, governed, OS-jail, browser, macOS and Android owner;
  cancellation after physical start settles uncertain, never a refund.
  Abort-before-I/O readback rebuilds the active interval union from the journal.
- **Two monthly totals.** The installation-period projection enforces the
  app-wide grant across foreground, event and behavior work; an indexed total
  over roots sharing the scheduler-sealed behavior ledger digest (behavior id +
  reviewed request, so narrowing cannot reset spend) enforces the behavior's
  narrowed ceilings. A separate authority-binding digest freezes the granted
  behavior and limits into the run binding and resource-tree identity. Ordinary
  roots omit the discriminator; a missing, mismatched or mutable one on a
  behavior root is corrupt authority; historical V22 roots lacking it in both
  may only finish accounting cleanup, never dispatch, reserve or resume.

### Unattended spend

A non-zero cost ceiling consents only to that behavior, operation set,
interval, destination policy and period, never an app wallet, reusable
foreground approval or model-selected purchase. Zero remaining stays zero, even
for "free" operations. Effects needing live confirmation, external auth or cart
cross-check park or fail with the typed disposition; consent is never inferred
from timeout, prior approval, stored text or purpose. Remote success with a lost
response or persistence is charged and uncertain, never replayed.

**Period ceilings park, not retry.** Admission checks monthly ceilings with a
zero-size candidate, so near `max_monthly_tokens` a launch can pass whose first
model call is refused. A `monthly_tokens` or `monthly_cost` refusal marks the
run `period_ceiling_breach`; the observer reads it off the terminal run (status
`completed`) and moves the next fire to `resource_period_end` (first of the next
UTC month) with `last_error` `period_resource_ceiling:<ceiling>` and one
warning. Per-run ceilings (`no_progress`, `active_time`) fail the run and
reschedule normally.

## Causation, spend depth, and self-amplification

The grant records causation- and spend-depth ceilings, but every behavior model
turn is a root fire at constant depth 0 (`BEHAVIOR_ROOT_CAUSATION_DEPTH`,
`artifact_v2/service.rs`): recipe steps add no hops and a background-origin
terminal cannot fire another behavior, so the ceiling bounds nothing yet.
Multi-hop causation must seal root fire, origin behavior, parent cause and both
depths, increment them on every expanding edge, keep generated-source
provenance and refuse before dispatch at the lowest ceiling; content, output,
contributions and mutations can never supply or reset them.

Missed intervals collapse to one due candidate (no per-bucket catch-up or
self-scheduling), at most the granted overlap runs and output never sets
cadence. The period
start counter advances under the live fence, or in crash recovery
only from an exact Artifact TaskState root-acceptance proof (a task binding
alone is not proof).

## Storage and contribution volume

Recipe store mutations (including a contextual round's drafts) commit only
through the App mutation owner, within the package schema and exact action,
charging record/byte ceilings first and reusing one mutation identity across
retries. Contributions go only to a reviewed port, whose owner reopens
authority, validates handling and retention, and applies the per-run proposal
ceiling plus its own ceilings against the exact planned projection (a create
without summary may yield none; an update without summary yields one
invalidation per matching port) before any commit intent. A legacy mutation
whose recovered projection exceeds it terminalizes with all contributions
suppressed.

**Scheduler bounds.** Behaviors per package, one live head per behavior,
numeric counters, closed error codes and retained events are capped; scans are
bounded indexed pages. A reconciliation pass persists start, cursor and
provisional debt before each installation, so cancellation resumes there and
leftover debt starts another bounded sweep; a slow app cannot hide a newly
enabled one. Inventory slices and per-installation work have separate ceilings;
due heads keep a separate fairness cursor. Retry
is exponential backoff doubling per failure and capped at 1 h
(`retry_delay_seconds`), not a dead letter. Input and secrets never
enter heads or events.

**Historical acceptance first.** Accepted-fire recovery runs before
reconciliation, even for disabled installations: expired heads are probed for
their Artifact root, proven starts settle, then unaccepted heads retire; live
leases are kept until settlement or expiry. Recovery and fresh root admission
both hold Artifact's per-task start-admission lock (admission until the root
commits and the worker spawns, with a final check of scope, generation, package,
grant, schema, digest, fire and live lease); the recovery guard survives waiter
cancellation. An opaque durable cursor advances before parsing or lock waits, so
a malformed row or contended task blocks no other. Destructive transitions use
the same fenced settlement; ordinary reconciliation cannot clear `pending`.

**Scope discovery** stops at the scan ceiling (reporting capacity debt), pages
principals under a fixed directory-page budget, isolates an unreadable
principal or metadata entry for the pass and fails closed on an unreadable
scope root; scope work has fixed concurrency and a per-scope time budget.
Timeout stops new local work, an admitted transition keeps its guard until
commit, and a pre-admission cancel leaves the durable lease to recover.

## Durable fire, lease, retry, and idempotency

```text
fire ref: installation + generation + package + grant + behavior
          + reviewed request digest + scheduled UTC boundary
task id:  scope + installation + action + fire idempotency + launch ref
head:     digest-bound source envelope + source policy
```

Acceptance compare-and-appends a pending fire (content-free source evidence,
monotonic revision) before dispatch. A short TTL lease names the worker and a
monotonic fence token; acquire/renew/transition/settle are revision CAS, so an
expired worker cannot act; a package or grant revision cannot replace a leased
head. An expired head is checked for exact root acceptance, under the
start-admission guard, before any reset, abandonment or retirement.

Scheduling is interval-only UTC (no cron, timezone or DST); restart collapses
misses to the latest eligible bucket and deduplicates by deterministic identity.

| Crash point | Outcome |
|---|---|
| Before acceptance | No fire, no effect. |
| After acceptance, before admission | The same fire is re-leased. |
| After admission, before settlement | Rematerializes only with the task binding plus a durable root in Artifact TaskState; a binding alone stays retryable. |
| After a future physical effect starts, before response | Effect receipt and uncertainty decide; no blind refire. |
| After terminal settlement | Replay returns the same result. |
| After generation, grant, pause or kill-switch change | Fire is blocked/stale. |

A future operation aggregate must derive every mutation, proposal, LLM attempt
and effect idempotency identity from the fire and a stable step ordinal. Without
provider idempotency, outcomes stay uncertain and consume their reservation.

## Scope isolation

Scope is in every durable key, lookup, lease, source fence, resource root,
idempotency key and health query; bare ids are never globally unique. The worker
takes scope from a server-authenticated carrier, never a package or payload.
Registry and entity stores are SQLCipher/scoped; a mismatch, missing key,
decryption failure or inconsistent fence is a hard refusal, not an empty result.
Cross-install composition, shared host reads, Town Square feeds and owner-wide
memory/retrieval are outside the own-store boundary.

A maintenance worker cannot resolve execution authority; only the workflow
owner's validated launch token mints the short-lived `ReviewedBackgroundLaunch`
scope for one installation (no generic execution, interactive approval or
meeting-capture intent); grants, scope pause, exact fire and resource leases
stay mandatory at launch.

## Health and operator visibility

The projection is bounded and metadata-only. Head state is `idle`, `pending`
or `blocked`; scope pause is separate. Items carry installation/behavior identity,
generation, package revision, revision and fence, effective interval,
next-due/available/lease times, pending fire ref, accepted root count, bounded
attempt/failure and period-start counters, update time and a closed sanitized
failure code. `accepted_count` means Artifact durably
published the root, not that work completed. The response adds the pause
revision, process-local `worker_running` (scheduler-set, never by callers),
a fixed recovery posture, corruption counts and ≤ 512 sanitized events (a
recent feed repeated per item page). A worker failure stops new claims but
keeps health and pause control, since accepted workflows still reopen the pause
row. Item pages are live best-effort reads; the cursor can skip a corrupt row.

Never exposed: record values, prompts, model output, tool payloads, secrets,
credentials, raw error bodies, capability tokens, SQLCipher keys, HMAC material.
Health authorizes nothing; recovery is report-only without proven exclusive
authority. Operators may pause, disable, revoke or inspect; the
API mints no retry, and repair never synthesizes a grant, source revision,
refund or success.

## Threat-to-control matrix

| Threat | Control | Fail-closed result |
|---|---|---|
| Fast or expensive loop | Closed interval vocabulary; ceilings the owner may only narrow | Refused at review or admission |
| Timer runs a revoked or updated app | Digest recheck at acceptance and final I/O | Fire stale/blocked |
| Stored record injects instructions | Typed selection, taint, neutralization, fixed hierarchy | Text stays data; invented call refused |
| Output triggers recursive spend | Depth-0 root fires; start/monthly caps | No expansion edge |
| Restart catch-up storm | UTC buckets, one collapsed miss, deterministic id | At most one due fire |
| Two workers accept one bucket | Scoped CAS acceptance, fenced TTL lease | Stale worker refused |
| Crash after workflow admission | Exact task-id reconstruction, published root and matching binding before rematerialization | Counters settle; a binding alone cannot consume the fire |
| Future effect loses its response | Receipt and uncertainty settlement | No effect lane until recovery is exact |
| Behavior reads another app/scope | Same-install resolver; scope in every key | Hard refusal |
| Model widens effect or destination | Owner predicate, argument proof, package lock, reclassification, one-shot permit | Not dispatched |
| Storage or scheduler flood | Record/byte/proposal/fire/retry/history ceilings | Refused |
| Kill switch races a lease | Pause/lifecycle/grant/resource fence at the final consequential boundary | Undispatched work stops |
| Health or logs leak input | Metadata-only projection, closed codes | Content omitted |

## Explicit deferrals

Outside `app_behaviors_v1`:

- cron/calendar/timezone expressions, DST policy, per-missed-bucket catch-up;
- manifest indicators and Town Square feed consumption;
- cross-app, cross-installation, cross-scope, host-store, memory-index or
  arbitrary external input;
- unattended device/UI automation and raw shell, SQL, filesystem, network or MCP
  lanes;
- multi-host scheduling, lease consensus, sharding, cross-store outbox delivery;
- provider exactly-once effects without provider idempotency;
- multi-hop causation (carrying the fire row's recorded depth into dispatch),
  generated-source lineage and child idempotency identities;
- recovery that invents authority or treats a completed child as proof of safe
  parent retirement;
- mobile OS background-execution integration.

Related contracts: [app entity store](app-entity-store.md),
[app-platform contract kernel](app-platform-contract-kernel.md),
[resource authority](resource-authority-api.md), and
[flat-loop execution](execution/FLAT_LOOP.md).
