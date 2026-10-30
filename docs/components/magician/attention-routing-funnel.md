# Attention Routing Funnel

The attention routing funnel decides where candidate items surface after ingest, distillation,
extraction, filtering and dedupe.

`magician_v2::attention_funnel` holds the vocabulary (source, source-family, candidate, lane, route
outcome, drop reason, route-event types; snake_case wire strings) and the deterministic router: an
`AttentionCandidate` plus `AttentionRouteContext` in, one `RouteOutcome` out, with no persistence and
no LLM calls. Design (archived):
`docs/archive/plans/2026-07-08-attention-routing-funnel-architecture-design-implementation.md`.

## Routing boundary

- actionable comms obligations and promise-like proposed actions → Follow-ups;
- owner approval/intervention → Needs you;
- terminal work failure reports → Failed, not Needs you;
- explicit non-actionable but useful context → Worth a look;
- weak, duplicate, sensitive or recency-only candidates are dropped or withheld.

Comms resurfacing reads only locally distilled summaries, never raw bodies. Active follow-up overlap
is checked through router context and recorded as an `active_follow_up_exists` drop, never filtered
silently. Comms source watermarks use provider-native message cursors; salience scoring uses
unix-second timestamps. Resurfacing derives only the `recency_only` context; the router owns final
Worth-a-look eligibility.

Lane is distinct from source family (a Follow-ups card may have family `promise` or
`comms_ingest`); `attention_source_family` route metadata is authoritative for new Follow-up rows,
older rows fall back to label/proposed-action inference. Channel-assist stats fall back to row
parsing, not a 500, if the source-family SQL fails.

## Producers and lane facade

- **Channel classifier** (body-blind) → channel annotations → Follow-ups. Routed items become
  `needs_approval`; dropped/low-confidence/no-action/FYI outcomes still write a quiet `classified`
  row so the message is not reclassified. `proposed_action` carries `attention_source_family` plus
  `attention_lane` / `attention_route_reason` or `attention_drop_reason`. Writes append idempotent
  per-stage traces and final route/drop events; failures record the failed stage, never a fake final
  route. A high-confidence card that is `repeat_of_recently_handled` against recently
  acknowledged/dismissed cards on the same thread drops as `cooldown_active` (recent handling is
  model context, not an unconditional suppressor).
- **Resurfacing** → Worth a look. The router runs before a candidate can be `surfaced`; the LLM
  curator only phrases/selects among eligible candidates, and eligibility needs explicit
  non-actionable-useful context. Distilled comms rehydrate `needs_reply_hint` / `follow_up_hint` at
  routing time (actionable → Follow-ups). Recency-only, weak, cooldown, missing-summary, overlap and
  curator-deferred decisions are recorded as drops; positive Worth events only after the guarded
  store transition marks the candidate surfaced. A curator pass writes its events in one
  `append_events` batch with content-keyed ids, so an unchanged repeated decision is not
  re-recorded. Time-windowed funnel counts for resurfacing therefore count each decision's first
  occurrence per content revision, not every pass that repeated it. Legacy refs with raw slashes fall back to
  provider-scoped evidence-message lookup for overlap.
- **Today / attention inbox** items (Needs you, Follow-ups, Active work, Delivered, Changed, HITL,
  failure, approval, escalation, running-work) become candidates, routed and recorded with
  deterministic event ids and stage traces, without changing payloads or stores, via
  `AttentionFunnelStore::append_events` (one transaction and one retention run per refresh); an
  append failure is logged as a warning and the response is served unchanged.

`magician_v2::attention_lane_facade` serves normalized lane pages (stable totals, limits, `has_more`,
cursor tokens). Backing stores: channel annotations → Follow-ups; resurfacing store → Worth a look
(after router eligibility); feed/V3 task stores → Needs you, Active work, Delivered, Changed, Failed.
Today sections, message follow-ups, Worth a look and the inbox use cursor-backed server pages.

**A lane's cursor is the key that lane is sorted on.** Most Today sections sort by `updated_at`
(cursor `{updated_at}:{item_id}`). Follow-ups sorts `priority DESC, updated_at ASC` across nine bands
(780/770/760/750/730/725/690/640/610), so its cursor is `{priority}:{updated_at}:{item_id}`; a
vanished row (a completed follow-up) would otherwise resume by the wrong key and re-serve rows. Older
band-less cursors resolve by exact row match, then the previous key. `/channel-assist/follow-ups` is
a separate producer using `NeedsApprovalCursor` over `created_at DESC, id DESC`.

## Route-event observability

`magician_v2::attention_funnel_store::AttentionFunnelStore` writes a bounded SQLite
`attention_route_events` table under the storage base root. Retention runs inside every append on
`/today` and `/feed/attention`, ordered by `attention_route_events_scope_created_idx`
(`principal, workspace, created_at DESC, id DESC`) and bounded on the exact composite
`(created_at, id)` (`id` is `AUTOINCREMENT`):

- **Age prune** — rows older than `ROUTE_EVENT_RETENTION_DAYS` (30) for the scope.
- **Cap prune** — seek to the `ROUTE_EVENT_SCOPE_CAP`-th (50,000th) newest row and range-delete
  below it. Reaching the cap is the steady state for a busy scope.

`GET /api/magician/v2/attention-funnel/observability` returns scoped counters by stage, trace
status/final outcome, source kind, source family, lane, route reason and drop reason, plus recent
event metadata (shown on `/observe/stats`).

## Progressive attention learning

`magician_v2::attention::learning` is an owner-scoped plane that never changes candidate truth or
deterministic safety routing; learning changes ordering only. Follow-ups and Worth a look map
actions to one canonical outcome vocabulary and persist idempotent client event IDs (1..=200
chars), source revisions, label quality and contract-qualified embeddings in
`attention_learning.db`. Acknowledge is neutral; absent feedback is never a negative label.

- **Ranking.** Explicit feedback triggers a bounded Bayesian embedding-kNN re-score of the active
  cohort (separate kNN for usefulness and actionability; neutral/inapplicable labels filtered before
  per-task `k`). Outcome capture and rank enforcement have separate flags. The API ranks the complete
  eligible universe before pagination and returns a `learned-v1` cursor bound to model generation
  and universe digest; drift fails it with a refresh response.
- **Learned lane.** Slice-1 actionability kNN also moves future similar cards between For you and
  Worth a look, never to Non-surfaced. These moves are valid in routing `baseline`; the web
  projection parser must not reject `route_applied` for lack of a canary.
- Learned rank orders within lanes even in `baseline`. Useful, Dismiss and do-it persist without a
  client decision id: the rank-recompute worker reconstructs the served decision from
  `attention_decision_items` (raw and canonical aliases, preferring a selected row, retrying until
  the ledger is visible). `semantic_ranking_enabled: false` is the immediate ordering rollback
  (keeps outcomes, embeddings, checkpoints). Health shows **Learned order active** only when a
  succeeded, complete canonical projection (or, absent canonical delivery, the legacy path)
  represents it.
- **Historical bootstrap.** Before producers start, one immutable cutoff is persisted; a bounded
  worker imports older Follow-up feedback and Worth affinity/dismissal embedding snapshots with
  deterministic event IDs and monotonic checkpoints (capped Worth sources drain first). Post-cutoff
  repair tails consume only shared event IDs and private semantic snapshots written by live actions,
  repairing missing events exactly once. Historical exemplars update the estimator but enqueue no
  rank jobs; nothing manufactures a reward.
- **Embedding bind queue.** Without local embedding, a durable queue keeps ≤ 8,192 chars of the
  locally derived brief (never raw bodies) with scope-fair reclaimable leases. Timeouts:
  `attention_learning.embedding_timeout_ms` (feedback),
  `attention_learning.background_embedding_timeout_ms` (repair/refresh). Temporary failures back
  off to an hourly cap; poison rows go `dead` after eight attempts. Malformed legacy surfaces are
  quarantined; dead snapshots cannot pin the historical cursor; scope deletion removes all bind rows.
- `GET /api/magician/v2/channel-assist/attention-learning/historical-bootstrap/status` reports
  progress, tail cursors and queue health; unchanged polls stop after bounded checks and rescore only
  scopes that imported or repaired. Score propagation commits one wave per writer ticket and
  transaction; a read-pool permit held ≥ 1 s logs a warning.

## Semantic features and actionability

Evidence-bearing, revision-bound LLM semantic features (from the existing body-blind classifier
call; never a routing authority) feed a calibrated actionability preview: mode, snapshot id,
per-card probability, deterministic explanation and coverage. Missing, stale, invalid or
incompatible rows fall back to Slice-1 Bayesian scoring and stay in the universe; cached scores need
an exact match on the semantic envelope and ordered feature vector.

`attention_learning::training` fits L2-logistic + Platt over explicit outcomes joined to the served
feature vector and refuses (recorded) below label/AUC/calibration gates. Run it with
`magician attention-learning train-actionability` or in-process via
`attention_learning.actionability.training.enabled`. A first scope install is forced to shadow and
never edits YAML; a later passing install may become `enforced` (used when YAML is `disabled` or its
pin cannot load).
`GET/POST /channel-assist/attention-learning/actionability-training/{status,run}`.

**Coverage backfill** is a separate non-serving control plane. Work rows hold identity, source
revision, producer, status, lease/retry state and error code — never brief or raw content. Scans
checkpoint per principal/workspace/surface with an opaque cursor. Coverage counts only succeeded envelopes compatible with the current revision and
schema/extractor/prompt/model/profile contract. Workers lease a concurrency-sized wave just before
model calls, renew exact leases and complete only under current ownership and revision; expired
leases are reclaimed first; health separates active vs expired in-flight work. The admin boundary
only schedules work; actionability serving needs a separate rollout decision.

## Duplicate grouping

A separately calibrated pair task. Pair labels are canonical unordered, revision-bound records with
idempotent owner event IDs; owner `not_duplicate` labels are durable cannot-links checked across
whole components before every merge (exact source identity cannot override them). The merge
threshold lives in the immutable pair snapshot with its contracts. Both list APIs group the complete
universe before pagination: shadow returns diagnostics only, enforced returns representatives with
expansion endpoints; member lifecycle rows are never touched; missing/disabled snapshots yield
singletons. `attention_learning.grouping.max_pair_evaluations` (default 2,500,000) is an
integrity/latency breaker: if required pairs exceed it, nothing is evaluated or truncated, every item
is a singleton and `budget_exceeded=true` is reported with reconciled totals.

## Decision ledger and canonical projection

Every Follow-up and Worth list projection atomically persists one `attention_decisions` row plus
`attention_decision_items` for the whole evaluated universe before returning any card; each card
carries its decision-item identity. Diagnostic: `GET .../attention-learning/decisions/{decision_id}`.
An API return is never an impression.

**Impressions.** `POST /api/magician/v2/channel-assist/attention-learning/impressions` reports
cumulative visibility, verified against scope, decision item, source revision and served surface.
Event IDs are idempotent (retries take the max dwell; reuse with different identity is rejected).
Verification needs the configured minimum dwell (`1..=60,000` ms; submitted dwell `1..=86,400,000`
ms) and never creates a negative label. Field limits: event/decision IDs 200, candidate/revision 500,
visibility-rule version 100, client type and viewport class 64, client version 128; empty or
control-bearing fields rejected.

**Routing policy.** Lane utility weights, calibration, uncertainty calibration, minimum confidence,
utility margin and surface utility live in one immutable routing-policy snapshot requiring exact
actionability and semantic contracts and pinning the optional grouping snapshot/model as an
all-or-none pair (candidate grouping identity must match, including `None`). Modes: `baseline`,
`shadow`, deterministic `canary`; shipped configs are `baseline`, no snapshot,
`canary_fraction: 0.0`. `information_brief.stated_action` is the instant baseline rollback, bypassed
only inside an assigned complete-union canary. Public `fallback_reason` is a bounded code.

**Union projection.** `GET .../attention-learning/canonical-projection` loads the complete active
Follow-up and Worth snapshots in **one** query each and proves each page has the rows its `total`
promised (Worth: `min(total, limit)` under `CANONICAL_UNION_WORTH_PAGE_LIMIT`; Follow-up: the whole
lane via `CANONICAL_UNION_FOLLOW_UP_PAGE_HINT`, `rows == total`, re-read once at the exact total if
larger). It origin-qualifies identities (raw IDs are not unique across stores), then groups/routes
the union once; a source-generation token is read before and after. Legacy list responses carry
only a compact `canonical_attention_projection_ref`. Stale generation, short page, count mismatch
or an unavailable projector publishes nothing partial; the legacy handler serves its atomic baseline
with a typed fail-closed state.

The schema-v1 projection has typed status, policy and integrity blocks and ordered
`lanes.follow_up`, `lanes.worth_a_look`, `lanes.non_surfaced`. Items carry origin, source revision,
origin payload, `origin_lane`/`served_lane`/`learned_lane`, grouping metadata and origin-owned action
descriptors (method, href, confirmation), so either origin renders in either lane without
reconstruction. Totals reconcile with zero duplicates and drops. Follow-up actions: `open_source`,
`approve`, `acknowledge`, `useful`, `dismiss`, `snooze`; Worth: `open_source`, `useful`,
`acknowledge`, `dismiss`. The projection also persists compact diagnostics (learning coverage, rank
generation, grouping, routing, bandit health) read by both legacy health panels; policy identity
includes the diagnostics contract version.

Rank/posterior and pair/cannot-link evidence stay owned by the origin surface; the projector remaps
within-origin pairs to origin-qualified IDs before union grouping. Unlabeled cross-origin pairs may
use calibrated inference but never borrow owner evidence. In baseline, lane order stays the atomic
origin order unless an independent ordering policy applies.

### What each owner act teaches

**For you** is owner work (reply, handle, schedule, a deadline owed). **Worth a look** is serendipity
(news, FYI, digests, audience invitations). Useful never chooses a lane; lane corrections move future
similar cards once kNN evidence clears the Slice-1 minimum. Taps never hide a card into
Non-surfaced.

| Owner act | Where it lives | Canonical outcome | Ranking (usefulness) | Ranking (actionability) | Lane |
|---|---|---|---|---|---|
| Approve / do it / reply sent | For you | `action_completed` | up | up | unchanged |
| Useful (no task) | For you | `useful` | up | none | unchanged |
| Shouldn't have been flagged | For you | `not_actionable` | none | down | future similar → Worth a look |
| Mark useful / Open | Worth a look | `useful` | up | none | unchanged |
| This needs me | Worth a look | `action_completed` | up | up | future similar → For you |
| Acknowledge | both | `neutral_seen` | none | none | unchanged |
| Dismiss, no reason / not relevant / spam | both | `irrelevant` | down | down | unchanged |
| Dismiss, wrong classification | For you | `not_actionable` | none | down | future similar → Worth a look |
| Dismiss, already handled | both | `obsolete` | none | none | unchanged |
| Dismiss, duplicate / delegated | both | identity / not-owner | none | none or down | unchanged |
| Snooze / open source / silence | — | *not recorded* | — | — | — |

For you admission is brief logic, not preference: only a reply hint, explicit owner-owes /
waiting-on / follow-up hint, request, deadline, scheduling, or a transaction/change notice stating
owner work. General information, promotions, events and untyped "read this" briefs stay out even with
a verb, and cards promoted only from such a brief are retracted on the next reconcile.

### Hard Follow-up/Worth communication identity reconciliation

Before learned ranking, grouping, routing, delivery or pagination, the only positive identity join is
exact equality of the validated tuple `(provider, account_alias, thread_id)`; text, subjects,
senders, embeddings, similarity and LLM output cannot establish or override it. Follow-up owns an
exact active match; each matching Worth candidate becomes a durable Worth-origin alias (revision and
feedback preserved) excluded from all downstream stages and cursor order, and reappears if the
Follow-up goes inactive. Alias and revision changes are in the universe digest. Malformed refs are a
safety failure: the projection fails closed with a bounded diagnostic and the server-side legacy
Worth guard withholds unresolved rows. Public alias summaries are capped at 100 opaque records.
Integrity: `raw = materialized + duplicate_hidden` and `raw = grouped_members + duplicate_hidden`.
Frozen contract: `cross-lane-identity-frozen-v1.json`.

## Bandit serving and canonical delivery

A personal Bayesian linear head is scoped by principal, workspace, surface and an immutable bandit
snapshot pinning upstream and semantic contracts, ordered features, Gaussian prior precision,
observation variance (`1e-6..=1e6`), reward mapping, finite posterior draw count, exploration floor,
slate bounds, attribution window and the `blake3-counter-box-muller-v1` replay contract (no hidden
weights). Neutral-seen, duplicate-identity, timing-negative, unknown-quality, missing-interaction and
attribution failures never update the posterior. Serving builds the finite probability-matching
distribution and records each position's conditional probability. Shadow serves baseline order
(propensity 1); canary may only reorder hard-eligible representatives inside a lane's slate, never
change lanes or act.

Shipped YAML is `bandit.mode: shadow`, `snapshot_id: null`: Magician auto-trains a prior once
actionability and routing snapshots exist, updates the posterior from attributed outcomes and
promotes that snapshot to canary after 40 posterior updates (a YAML pin blocks this; a new upstream
snapshot forces shadow again). The prior file goes through the shared durable writer.

**Delivery roots.** `GET .../attention-learning/canonical-deliveries/{lane}` (`follow_up` or
`worth_a_look`) creates one immutable root before page zero, binding projection/digest, candidate
revisions, policy/model snapshot, posterior version, seed, scope, expiry and a
`source_generation_token` (row-ordered streaming digests of both source lanes, the versioned
temporal bucket and projection/cache identity). The policy draws once and cursor pages read the
persisted root and never resample, so retries return identical pages. Lanes never share a delivery
decision ID or candidate. Cursor requests validate the token with narrow scans; pre-token rows use
projection-ID/universe-digest validation until TTL. `root_policy_propensity` is the OPE weight on
every page; `conditional_delivery_propensity` is `1.0`. Expiry, scope/lane/root mismatch, token or
projection drift, revision drift or missing cursor state → HTTP 409
`attention_delivery_refresh_required` (never spliced); a disabled or degraded policy yields a stable
HTTP 200 `baseline_fallback` root.

Each item carries a persisted exposure token (scope, decision, delivery, page, position, item,
revision, digest, propensities, expiry); the response carries the root-frozen `impression_policy`.
Only a delivered, token-verified item meeting dwell becomes a verified impression; impression rows
copy both propensities. Feedback attributes through the delivery root and verified impression;
outcome replay updates the posterior at most once and may only monotonically enrich attribution.

Operator routes (metadata only): `GET .../attention-learning/delivery-health`,
`GET .../semantic-extraction/status`, `POST .../semantic-extraction/enqueue`. Delivery roots, items,
pages, page items and cursors have scoped tables; retention cascades expired roots past the horizon
and scoped deletion touches only the selected scope. Roots, items and the served canonical decision
keep `delivery_retention_days`; an expired root nothing attributes to (no impression, outcome,
recompute job or bandit update) stops pinning only its canonical projection after
`bandit.expired_delivery_retention_hours`; delivery items whose attribution item equals the canonical
decision item store `attribution_ref` instead of a copy (see
[storage governance](storage-governance.md#attention-learning-maintenance)). Defaults: page size 50,
max 200, TTL 900 s, retention 30 days, expired-unattributed retention 24 h, bandit serving
disabled/null/zero.

## Rank recompute

Feedback commits the canonical outcome first, then verifies decision item, source revision,
attribution window and preferably a verified impression before an idempotent posterior update. List
reads never call the model or wait on a job. One scoped `attention_rank_recompute_jobs` row per
outcome runs `pending -> in_flight -> retry` (exhausted → `dead`, no double learning); enqueue or
worker failure never rolls back the outcome, and scheduling reconciles missing jobs. The receipt
returns the served-before rank, a pending job and a polling URL.

Jobs resolve against the projection the decision was **served from** (acting removes the candidate
from the live projection); `candidate_set_digest` is content-only. A served projection compares
recompute generation, posterior version, candidate and source revision; only a live read can yield
`universe_changed_during_commit`. Results are `served_universe_diagnostic` or
`current_universe_diagnostic`; only a succeeded current one exposes an after rank (bound to revision,
union digest, generation, posterior, policy, time); CAS drift → `stale`. Canonical delivery outcomes
and both legacy adapters share this path. `make check-rank-recompute-semantics` (in `check-all`)
keeps backend constants, the frozen fixture and the web parser in agreement.

`POST /attention-learning/rank-recompute/requeue` returns `universe_changed_during_commit` jobs to
`pending` (`apply: false` default, bounded, idempotent), backfilling `affected_rank_before` from the
served rank. Served-projection lookup is exact identity first, universe-digest/time as fallback.
`GET .../rank-recompute/jobs/{job_id}` polls; `status` / `schedule` / `process` separate health,
reconciliation and processing. `rank_recompute.enabled=false` pauses leasing. Jobs renew at one third
of the lease duration. Retention preserves active jobs, outcomes and idempotency anchors and
checkpoints posterior state before compacting raw updates.

`attention_outcomes` stores claimed `decision_id`, `delivery_id` and optional `impression_id`,
enriched only monotonically on replay; crash reconciliation schedules a missing job only from
attribution already validated into `attention_bandit_updates`. Without client attribution, enqueue
and worker reconstruct the most recent decision containing the candidate; only a truly unlinkable
outcome reports `no_served_decision`, and an unparseable stored projection
`served_projection_unreadable` (both terminal for the web poller).

**Feature capture** writes each vector once in content-addressed `attention_feature_vectors`, bound
via `attention_candidate_feature_bindings`; the decision item carries the digest. Content identity
includes the full semantic producer contract plus actionability/temporal contracts. Legacy snapshot
rows stay readable (schema V1) and migrate in bounded batches. Age uses
`attention_temporal_buckets_v1`; the actionability contract is `attention_actionability_features_v2`
(V1 snapshots must be retrained). Training and serving share one extraction function; capture is
forward-only.

**Projection storage** is normalized schema V3 (V2 readable), content-addressing item revisions and
diagnostics once; responses materialize at the API edge and delivery pages reconstruct only their
window. Retention removes unreferenced projections, then bodies; outcomes, impressions and jobs copy
`projection_id` before compaction.

**SQLite access.** One ticket-ordered FIFO writer and a four-connection query-only read pool. The
three serving-path writes (decision record, normalized projection, delivery creation) wait at most
`REQUEST_PATH_WRITER_WAIT` (2 s); an abandoned ticket is skipped, the projection falls back to the
atomic baseline with `attention_writer_busy` and delivery creation returns 503 with `Retry-After`,
so page reads never queue behind maintenance. A saturated read pool fails closed after one
deadline.

Eval scripts: `scripts/eval_attention_bandit_ope.py`, `scripts/eval_attention_rank_recompute.py`,
`scripts/eval_attention_historical_bootstrap.py`, `scripts/eval_attention_cross_lane_identity.py`.

### Contextual actions are lane-neutral

`ResurfacingActionService` acts on a `ResurfacingActionTarget` (a candidate or a channel-assist
follow-up) with one claim, replay-on-retry and error taxonomy;
`resurfacing_action_claims.target_kind` disambiguates ids. Only source-independent kinds
(`ResurfacingActionKind::is_source_independent`, e.g. a reminder from submitted title, note and time)
are reachable from a follow-up. A resurfacing candidate is re-read inside the claim transaction; a
follow-up lives in another database and is validated just before claiming; idempotency and replay
are identical. `POST /channel-assist/follow-ups/{annotation_id}/actions` shares the Worth route's
contract and error codes.

### Reading the surface metrics

- `verified_impression_coverage` is per decision and near-zero by construction; the scope-level
  `verified_impression_total` shows whether reward signal flows.
- Embedding coverage matches canonical surface-qualified and raw surface-local ids (embeddings are
  stored under the raw id).
- Semantic health uses the active source/producer cohort; historical dead letters show separately and
  cannot degrade a healthy cohort; pending recovery outranks an older invalid envelope. An
  unavailable ranked projection is not evidence of observe mode.
- Candidate embeddings are memoised per scope and surface, refreshed incrementally and validated by a
  `(row count, max updated_at)` probe.

### Incremental history maintenance and worker recovery

Canonical page delivery schedules history maintenance without awaiting the writer. Each coalesced
database/scope pass freezes its keyset horizon and releases the writer between transactions (four
history rows, or ≤ 32 body rows / 4 MiB; one oversized body can progress) and preserves serving
decisions, feedback, impressions, rank jobs, recent history and
cross-scope bodies; new projections cannot extend a pass forever. This is logical pruning, not
VACUUM; see [storage governance](storage-governance.md#attention-learning-maintenance).

The rank worker loops immediately after a full batch, else waits its interval. Missing serving evidence settles `no_served_decision`; exhaustion
keeps the real reason. Legacy `max_retries_exhausted` dead jobs get one bounded versioned recovery
pass through normal leases, audited in `attention_queue_recoveries` so the retry allowance cannot
reset repeatedly.

Semantic discovery scans ≥ 500 active candidates per page regardless of model-call budgets. The
dispatcher receives the existing semantic JSON schema (allowed field refs, 1–12 evidence count);
local validation still decides usability. Discovery may requeue a schema/evidence-invalid result
once per source/extractor revision (recorded in the recovery ledger), and reopen a successful
receipt only when active-source discovery saw incompatible features, once per revision and producer;
the worker rechecks coverage after leasing and completes covered work without a model call.
Same-producer, same-revision invalid refreshes keep valid features; source-revision CAS and call
budgets stay in force. The communication extractor applies to communication and web briefs; other
families are `not_applicable` (coverage over the applicable count) and keep deterministic features.
Development backfill: 60 calls/minute and 60 per pass, two concurrent requests (repo config, live
config and defaults agree); every wave rechecks foreground pressure.
`make test-attention-recovery` covers this area.
