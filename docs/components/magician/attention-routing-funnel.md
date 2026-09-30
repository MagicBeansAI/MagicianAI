# Attention Routing Funnel

The attention routing funnel is the backend contract for deciding where candidate items should
surface after ingest, distillation, extraction, filtering, and dedupe.

The canonical Rust vocabulary and deterministic router live in `magician_v2::attention_funnel`:
stable source, source-family, candidate, lane, route outcome, drop reason, and route-event types
(snake_case wire strings). The router takes an `AttentionCandidate` plus `AttentionRouteContext` and
returns one `RouteOutcome`; it has no persistence and calls no LLMs. Design (archived):
`docs/archive/plans/2026-07-08-attention-routing-funnel-architecture-design-implementation.md`.

## Routing boundary

- actionable comms obligations and promise-like proposed actions → Follow-ups;
- owner approval/intervention → Needs you;
- terminal work failure reports → Failed, not Needs you;
- explicit non-actionable but useful context → Worth a look;
- weak, duplicate, sensitive or recency-only candidates are dropped or withheld.

Comms resurfacing reads only locally distilled summaries, never raw bodies. Active follow-up
overlap is checked through router context so the funnel records the canonical
`active_follow_up_exists` drop rather than filtering silently. Comms source watermarks use
provider-native message cursors; salience scoring uses unix-second timestamps. Resurfacing's
non-recency signal checks only derive router context for `recency_only`; the shared router owns
final Worth-a-look eligibility.

Lane is distinct from source family (a Follow-ups card may have source family `promise` or
`comms_ingest`). `attention_source_family` route metadata is authoritative for new Follow-up rows;
label/proposed-action inference remains only for older rows. The channel-assist stats endpoint
falls back to row parsing if its optimized source-family SQL fails, rather than returning 500.

## Producers and lane facade

- **Channel classifier.** The body-blind classifier runs through the router before creating
  channel annotations (the Follow-ups and quiet-classification store). Routed Follow-ups become
  `needs_approval`; dropped/low-confidence/no-action/FYI outcomes still write a quiet `classified`
  row so the message is not reclassified. Annotations carry `attention_source_family` plus either
  `attention_lane` / `attention_route_reason` or `attention_drop_reason` in `proposed_action`.
  Writes append idempotent per-stage traces and final route/drop events; failures record the failed
  stage without faking a final route. A high-confidence card that is `repeat_of_recently_handled`
  against recent acknowledged/dismissed cards on the same thread drops as `cooldown_active`
  (recent handling is model context, not an unconditional suppressor).
- **Resurfacing** runs through the same router before a candidate can be `surfaced`. The
  resurfacing store backs Worth a look; the LLM curator only phrases/selects among eligible
  candidates, and eligibility needs explicit non-actionable-useful context. Distilled comm
  candidates rehydrate `needs_reply_hint` / `follow_up_hint` at routing time (actionable hints →
  Follow-ups). Recency-only, weak, cooldown, missing-summary, overlap and curator-deferred
  decisions are recorded as drops. Positive Worth route events are recorded only after the guarded
  store transition actually marks the candidate surfaced. Legacy refs with raw slashes fall back to
  provider-scoped evidence-message lookup for overlap.
- **Today / attention inbox** items (Needs you, Follow-ups, Active work, Delivered, Changed, HITL,
  failure, approval, escalation, running-work) are converted to candidates, routed and recorded
  best-effort with deterministic event ids and stage traces, without changing payloads or stores.
  They append via `AttentionFunnelStore::append_events` (one transaction and one retention run per
  refresh).

`magician_v2::attention_lane_facade` returns normalized lane pages with stable totals, limits,
`has_more` and cursor tokens. Backing stores: channel annotations → Follow-ups; resurfacing store →
Worth a look (after router eligibility); feed/V3 task stores → Needs you, Active work, Delivered,
Changed, Failed. Today sections, message follow-ups, Worth a look and the inbox use cursor-backed
server pages.

**A lane's cursor is the key that lane is sorted on.** Most Today sections sort by `updated_at`
(cursor `{updated_at}:{item_id}`). Follow-ups sorts `priority DESC, updated_at ASC` across nine
bands (780/770/760/750/730/725/690/640/610), so its cursor is `{priority}:{updated_at}:{item_id}`;
otherwise a vanished row (completing a follow-up) would resume by the wrong key and re-serve rows.
Older band-less cursors still resolve by exact row match, then the previous key. The
`/channel-assist/follow-ups` lane is a separate producer using `NeedsApprovalCursor` over
`created_at DESC, id DESC`.

## Route-event observability

`magician_v2::attention_funnel_store::AttentionFunnelStore` writes a bounded SQLite
`attention_route_events` table under the storage base root. Retention runs inside every append on
`/today` and `/feed/attention`, index-ordered on `attention_route_events_scope_created_idx`
(`principal, workspace, created_at DESC, id DESC`) and bounded on composite `(created_at, id)` (one
millisecond can hold thousands of batch rows; `id` is `AUTOINCREMENT`, so the composite is exact):

- **Age prune** — rows older than `ROUTE_EVENT_RETENTION_DAYS` (30) for the scope.
- **Cap prune** — seek to the `ROUTE_EVENT_SCOPE_CAP`-th (50,000th) newest row and range-delete
  below it. Reaching the cap is the steady state for a busy scope.

`GET /api/magician/v2/attention-funnel/observability` returns scoped counters by stage, trace
status/final outcome, source kind, source family, lane, route reason and drop reason, plus recent
event metadata (shown on `/observe/stats`).

## Progressive attention learning (Slice 1)

`magician_v2::attention::learning` is an owner-scoped learning plane that never changes candidate
truth or deterministic safety routing. Follow-ups and Worth a look map actions to one canonical
outcome vocabulary and persist idempotent client event IDs (1..=200 chars), source revisions, label
quality and contract-qualified embeddings in `attention_learning.db`. Acknowledge is neutral;
absent feedback is never a negative label.

- **Ranking.** Explicit feedback triggers a bounded Bayesian embedding-kNN re-score of the active
  cohort, with separate kNN selection for usefulness and actionability; neutral/inapplicable labels
  are filtered before per-task `k`. Outcome capture and rank enforcement have separate flags. The
  API ranks the complete eligible universe before pagination and returns a `learned-v1` cursor
  bound to model generation and universe digest; drift fails the cursor with a refresh response.
  Learning changes ordering only.
- **Learned lane.** Slice-1 actionability kNN also picks a lane for future similar cards: demote For
  you → Worth a look ("shouldn't have been flagged"), promote Worth → For you ("this needs me"),
  never to Non-surfaced. These moves are valid in routing `baseline`; the web projection parser must
  not reject `route_applied` for lack of a canary.
- The canonical projection applies learned rank as within-lane order even in `baseline`. Useful,
  Dismiss and do-it persist without a client decision id: the rank-recompute worker reconstructs
  the served decision from `attention_decision_items` (raw and canonical aliases, preferring a
  selected row, retrying until the ledger is visible). `semantic_ranking_enabled: false` is the
  immediate ordering rollback (keeps outcomes, embeddings, checkpoints). Health reports **Learned
  order active** only when a succeeded, complete canonical projection (or the legacy learned path
  when canonical delivery is absent) represents it.
- **Historical bootstrap.** Before producers start, one immutable cutoff is persisted; a bounded
  worker imports older Follow-up feedback and Worth affinity/dismissal embedding snapshots with
  deterministic event IDs and monotonic checkpoints (capped Worth sources drain first).
  Post-cutoff repair tails consume only shared event IDs and private semantic snapshots written by
  live actions, repairing missing events exactly once. Historical exemplars update the estimator
  but enqueue no fake rank jobs; nothing manufactures a reward.
- **Embedding bind queue.** When local embedding is unavailable, a durable queue keeps ≤ 8,192
  chars of the locally derived brief (never raw bodies), with scope-fair reclaimable leases.
  Feedback spends `attention_learning.embedding_timeout_ms`; background repair and refresh spend
  `attention_learning.background_embedding_timeout_ms`. Temporary failures back off to an hourly
  cap; poison rows go `dead` after eight attempts. Malformed legacy surfaces are quarantined; dead
  snapshots cannot pin the historical cursor; scope deletion removes all bind rows.
- `GET /api/magician/v2/channel-assist/attention-learning/historical-bootstrap/status` reports
  progress, tail cursors and queue health; unchanged polls stop after bounded checks and rescore
  only scopes that actually imported or repaired. Score propagation commits one wave under one fair
  writer ticket and transaction; projection reads copy a revision under a pooled read permit and
  parse after releasing it (warns on ≥ 1 s holds).

## Semantic features and actionability

Evidence-bearing, revision-bound LLM semantic features (from the existing body-blind classifier
call; never a routing authority) feed a calibrated actionability preview. Serving exposes mode,
snapshot id, per-card probability, deterministic explanation and coverage. Missing, stale, invalid
or incompatible rows fall back to Slice-1 Bayesian scoring and stay in the universe; cached scores
require an exact match on the semantic envelope and ordered feature vector.

`attention_learning::training` fits L2-logistic + Platt over explicit outcomes joined to the served
feature vector and refuses (recorded) below label/AUC/calibration gates. CLI:
`magician attention-learning train-actionability`; in-process when
`attention_learning.actionability.training.enabled`. A first scope install is forced to shadow and
never edits YAML; a later passing install may become `enforced`. Serving uses the scope install when
YAML is `disabled` or a YAML pin cannot load.
`GET/POST /channel-assist/attention-learning/actionability-training/{status,run}`.

**Coverage backfill** is a separate non-serving control plane. Work rows hold scope, surface,
candidate identity, source revision, producer identity, status, lease/retry state and error code —
no brief or raw content. Scan progress is checkpointed per principal/workspace/surface with an
opaque cursor. Coverage counts only succeeded envelopes compatible with the current revision and
schema/extractor/prompt/model/profile contract. Workers lease a concurrency-sized wave just before
model calls, renew exact leases, and complete only under current ownership and revision; expired
leases are reclaimed first; health separates active vs expired in-flight work. The admin boundary
only schedules work. Actionability serving needs a separate rollout decision.

## Duplicate grouping

A separate calibrated pair task. Pair labels are canonical unordered, revision-bound records with
idempotent owner event IDs; owner `not_duplicate` labels are durable cannot-links checked across
whole components before every merge (exact source identity cannot override them). The merge
threshold lives in the immutable pair snapshot with its contracts. Both list APIs group the complete
universe before pagination: shadow returns diagnostics only; enforced returns representatives with
expansion endpoints; member lifecycle rows are never touched; missing/disabled snapshots yield
singletons. `attention_learning.grouping.max_pair_evaluations` (default 2,500,000) is an integrity/
latency breaker: if required pairs exceed it, nothing is evaluated or truncated, every item is a
singleton, and `budget_exceeded=true` is reported with reconciled totals.

## Decision ledger and canonical projection

Every Follow-up and Worth list projection atomically persists one `attention_decisions` row plus
`attention_decision_items` for the complete evaluated universe before returning any card; each card
carries its decision-item identity. `GET .../attention-learning/decisions/{decision_id}` is the
diagnostic. An API return is never an impression.

**Impressions.** `POST /api/magician/v2/channel-assist/attention-learning/impressions` reports
cumulative visibility; the store verifies scope, decision item, source revision and served surface.
Event IDs are idempotent (retries take the maximum dwell; a reused ID with different identity is
rejected). Verification needs the configured minimum dwell (`1..=60,000` ms; submitted dwell
`1..=86,400,000` ms) and never creates a negative label. Field limits: event/decision IDs 200,
candidate/revision 500, visibility-rule version 100, client type and viewport class 64, client
version 128; empty or control-bearing fields rejected.

**Routing policy.** Lane utility weights, calibration, uncertainty calibration, minimum confidence,
utility margin and surface utility live in one immutable routing-policy snapshot requiring exact
actionability and semantic contracts and pinning the optional grouping snapshot/model as an
all-or-none pair (candidate grouping identity must match, including `None`). Modes: `baseline`,
`shadow`, deterministic `canary`; shipped configs are `baseline` with no snapshot and
`canary_fraction: 0.0`. `information_brief.stated_action` is the instant baseline rollback, bypassed
only inside an assigned complete-union canary. Public `fallback_reason` is a bounded code.

**Union projection.** One scoped projection loads the complete active Follow-up and Worth snapshots,
proves each page has the rows its `total` promised, origin-qualifies identities (raw IDs are not
unique across stores), then groups/routes the union once.
`GET .../attention-learning/canonical-projection` returns it. Each source loads in **one** query:
Worth bounded by `CANONICAL_UNION_WORTH_PAGE_LIMIT` (checks `min(total, limit)` rows); Follow-up is
the whole lane via `CANONICAL_UNION_FOLLOW_UP_PAGE_HINT` (checks `rows == total`, re-read once at
the exact total if larger). A source-generation token is read before and after. Legacy list
responses return only a compact `canonical_attention_projection_ref`. Stale generation, short page,
count mismatch or an unavailable projector cannot publish a partial result; the legacy handler
serves its atomic baseline with a typed fail-closed state.

The schema-v1 projection has typed status, policy and integrity blocks plus ordered
`lanes.follow_up`, `lanes.worth_a_look`, `lanes.non_surfaced`. Items carry origin, source revision
and origin payload, `origin_lane`/`served_lane`/`learned_lane`, grouping metadata and origin-owned
action descriptors (method, href, confirmation), so either origin renders in either lane without
reconstruction. Totals reconcile with zero duplicates and drops. Follow-up actions: `open_source`,
`approve`, `acknowledge`, `useful`, `dismiss`, `snooze`; Worth: `open_source`, `useful`,
`acknowledge`, `dismiss`. The projection also persists compact diagnostics (learning coverage, rank
generation, grouping, routing, bandit health) consumed by both legacy health panels; policy identity
includes the diagnostics contract version.

Rank/posterior and pair/cannot-link evidence stay owned by the origin surface; the projector remaps
within-origin pairs to origin-qualified IDs before union grouping. Unlabeled cross-origin pairs may
use calibrated inference but never borrow owner evidence. In baseline, final lane order stays the
atomic origin order unless an independent ordering policy applies.

### What each owner act teaches

**For you** is owner work (reply, handle, schedule, a deadline owed). **Worth a look** is
serendipity (news, FYI, digests, audience invitations). Useful never chooses a lane; lane
corrections do, applied to future similar cards once kNN evidence clears the Slice-1 minimum. Taps
never hide a card into Non-surfaced.

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
a verb; cards promoted only from such a brief are retracted on the next reconcile.

### Hard Follow-up/Worth communication identity reconciliation

Before learned ranking, grouping, routing, delivery or pagination, the union reconciles
communication identity. The only positive join is exact equality of the validated tuple
`(provider, account_alias, thread_id)`; text, subjects, senders, embeddings, similarity and LLM
output cannot establish or override it. Follow-up owns an exact active match; each matching Worth
candidate becomes a durable Worth-origin alias (revision and feedback preserved) excluded from all
downstream stages and cursor order. If the Follow-up goes inactive, the Worth source reappears.
Alias and revision changes are in the universe digest. Malformed refs are a safety failure: the
projection fails closed with a bounded diagnostic and the server-side legacy Worth guard withholds
unresolved rows. Public alias summaries are capped at 100 opaque records. Integrity:
`raw = materialized + duplicate_hidden` and `raw = grouped_members + duplicate_hidden`. Frozen
contract: `cross-lane-identity-frozen-v1.json`.

## Bandit serving and canonical delivery

A personal Bayesian linear head is scoped by principal, workspace, surface and immutable bandit
snapshot, which pins the upstream and semantic contracts, ordered features, Gaussian prior
precision, observation variance (`1e-6..=1e6`), reward mapping, finite posterior draw count,
exploration floor, slate bounds, attribution window and the `blake3-counter-box-muller-v1` replay
contract — no hidden weights. Neutral-seen, duplicate-identity, timing-negative, unknown-quality,
missing-interaction and attribution failures never update the posterior. Serving builds the finite
probability-matching distribution and records each position's conditional probability. Shadow
serves baseline order (propensity 1); canary may only reorder hard-eligible representatives inside
each lane's slate — never change lanes or act.

Shipped YAML is `bandit.mode: shadow`, `snapshot_id: null`: Magician auto-trains a prior once
actionability and routing snapshots exist, updates the posterior from attributed outcomes, and
promotes that snapshot to canary after 40 posterior updates (a YAML pin would block this; a new
upstream snapshot forces shadow again). The prior file is written via the shared durable writer.

**Delivery roots.** `GET .../attention-learning/canonical-deliveries/{lane}` (`follow_up` or
`worth_a_look`) creates one immutable root before page zero, binding projection/digest, candidate
revisions, policy/model snapshot, posterior version, seed, scope, expiry and a
`source_generation_token` (row-ordered streaming digests of both source lanes, the versioned
temporal bucket, and projection/cache identity). The policy draw happens once; cursor pages read the
persisted root and never resample, so retries return identical pages. Lanes never share a delivery
decision ID or candidate. Cursor requests validate the token with narrow scans; pre-token rows use
projection-ID/universe-digest validation until TTL. `root_policy_propensity` is the OPE weight on
every page; `conditional_delivery_propensity` is `1.0`. Expiry, scope/lane/root mismatch, token or
projection drift, revision drift or missing cursor state → HTTP 409
`attention_delivery_refresh_required` (never spliced). Disabled/degraded policy creates a stable
HTTP 200 `baseline_fallback` root.

Each item carries a persisted exposure token (scope, decision, delivery, page, position, item,
revision, digest, propensities, expiry); the response carries the root-frozen `impression_policy`.
Only a delivered, token-verified item meeting dwell becomes a verified impression; impression rows
copy both propensities. Feedback attributes through the delivery root and verified impression;
outcome replay updates the posterior at most once; a replay may only monotonically enrich
attribution.

Operator routes (metadata only): `GET .../attention-learning/delivery-health`,
`GET .../semantic-extraction/status`, `POST .../semantic-extraction/enqueue`. Delivery roots, items,
pages, page items and cursors have scoped tables; retention cascades expired roots past the horizon;
scoped deletion touches only the selected scope. Defaults: page size 50, max 200, TTL 900 s,
retention 30 days, bandit serving disabled/null/zero.

## Rank recompute

Feedback commits the canonical outcome first, then verifies decision item, source revision,
attribution window and preferably a verified impression before an idempotent posterior update. List
reads never call the model or wait on a job. One scoped `attention_rank_recompute_jobs` row per
outcome runs `pending -> in_flight -> retry` (exhausted → `dead`, no double learning); enqueue or
worker failure never rolls back the outcome, and scheduling reconciles missing jobs. The receipt
returns served-before rank, a pending job and polling URL.

Jobs resolve against the projection the decision was **served from** (acting removes the candidate
from the live projection). `candidate_set_digest` is content-only. A served projection compares
recompute generation, posterior version, candidate and source revision; only a live read can yield
`universe_changed_during_commit`. Results are `served_universe_diagnostic` or
`current_universe_diagnostic`; only a succeeded current one may expose an after rank (bound to
revision, union digest, generation, posterior, policy, time); CAS drift → `stale`. Canonical delivery
outcomes and both legacy adapters share this path. `make check-rank-recompute-semantics` (in
`check-all`) keeps backend constants, the frozen fixture and the web parser in agreement.

`POST /attention-learning/rank-recompute/requeue` returns `universe_changed_during_commit` jobs to
`pending` (`apply: false` default, bounded, idempotent), backfilling `affected_rank_before` from the
served rank. Served-projection lookup is exact identity first, universe-digest/time as fallback.
`GET .../rank-recompute/jobs/{job_id}` polls; `status` / `schedule` / `process` separate health,
reconciliation and processing. `rank_recompute.enabled=false` pauses leasing. Jobs renew at one third
of lease duration. Retention preserves active jobs, outcomes and idempotency anchors and checkpoints
posterior state before compacting raw updates.

`attention_outcomes` stores claimed `decision_id`, `delivery_id`, optional `impression_id`, with
only monotonic enrichment on replay. Crash reconciliation schedules a missing job only from
attribution already validated into `attention_bandit_updates`. Without client attribution, enqueue
and worker reconstruct the most recent decision containing the candidate; only a truly unlinkable
outcome reports `no_served_decision`, and an unparseable stored projection reports
`served_projection_unreadable` (both terminal for the web poller).

**Feature capture** writes each vector once in content-addressed `attention_feature_vectors` with
bindings in `attention_candidate_feature_bindings`; the decision item carries the digest. Content
identity includes the full semantic producer contract plus actionability/temporal contracts.
Legacy snapshot rows remain readable (schema V1) and migrate in bounded batches. Age uses
`attention_temporal_buckets_v1`; the actionability contract is
`attention_actionability_features_v2` (V1 snapshots must be retrained). Training and serving share
one extraction function; capture is forward-only.

**Projection storage.** Normalized schema V3 (V2 readable), content-addressing item revisions and
diagnostics once; responses materialize at the API edge; delivery pages reconstruct only their
window. Retention removes unreferenced projections, then bodies. Outcomes, impressions and jobs copy
`projection_id` before compaction.

**SQLite access.** One ticket-ordered FIFO writer and a four-connection query-only read pool. The
three serving-path writes (decision record, normalized projection, delivery creation) wait at most
`REQUEST_PATH_WRITER_WAIT` (2 s); an abandoned ticket is skipped, the projection falls back to the
atomic baseline with `attention_writer_busy`, and delivery creation returns 503 with `Retry-After`,
so page reads never queue behind maintenance. Large JSON is prepared before the writer transaction;
no guard crosses an await; a saturated read pool fails closed after one deadline.

Eval scripts: `scripts/eval_attention_bandit_ope.py`, `scripts/eval_attention_rank_recompute.py`,
`scripts/eval_attention_historical_bootstrap.py`, `scripts/eval_attention_cross_lane_identity.py`.

### Contextual actions are lane-neutral

`ResurfacingActionService` acts on a `ResurfacingActionTarget` — a candidate or a channel-assist
follow-up — through one claim, replay-on-retry and error taxonomy; new lanes are new variants.
`resurfacing_action_claims.target_kind` disambiguates ids. Only source-independent kinds
(`ResurfacingActionKind::is_source_independent`, e.g. a reminder built from submitted title, note and
time) are reachable from a follow-up. A resurfacing candidate is re-read inside the claim
transaction; a follow-up lives in another database and is validated just before claiming.
Idempotency and replay are identical. `POST /channel-assist/follow-ups/{annotation_id}/actions`
shares the Worth route's contract and error codes.

### Reading the surface metrics

- `verified_impression_coverage` is per decision and near-zero by construction; do not read it as
  "is the reward signal flowing". `verified_impression_total` (scope-level) is that indicator.
- Embedding coverage matches both canonical surface-qualified ids and raw surface-local ids
  (embeddings are stored under the raw id).
- Semantic health uses the active source/producer cohort; historical dead letters are shown
  separately and cannot degrade a healthy cohort; pending recovery outranks an older invalid
  envelope. An unavailable ranked projection is not evidence of observe mode.
- Candidate embeddings are memoised per scope and surface, refreshed incrementally and validated by
  a `(row count, max updated_at)` probe.

### Incremental history maintenance and worker recovery

Canonical page delivery schedules history maintenance without awaiting the writer. Each coalesced
database/scope pass freezes its keyset horizon and releases the writer between transactions (four
history rows, or ≤ 32 body rows / 4 MiB; one oversized body can progress). Reference checks use
reverse indexes and preserve serving decisions, feedback, impressions, rank jobs, recent history and
cross-scope bodies; new projections cannot extend a pass forever. This is logical pruning, not
VACUUM.

The rank worker continues immediately after a full batch (yielding between passes) and uses its
interval when idle or failing. Missing serving evidence settles `no_served_decision`; exhaustion
keeps the real reason. Legacy `max_retries_exhausted` dead jobs get one bounded versioned recovery
pass through normal leases, audited in `attention_queue_recoveries` so retry allowance cannot reset
repeatedly.

Semantic discovery scans ≥ 500 active candidates per page independent of model-call budgets. The
dispatcher receives the existing semantic JSON schema (allowed field refs, 1–12 evidence count);
local validation still decides usability. Discovery may requeue a schema/evidence-invalid result once
per source/extractor revision (recorded in the recovery ledger), and may reopen a successful receipt
only when active-source discovery observed incompatible features, once per revision and producer;
the worker rechecks coverage after leasing and completes covered work without a model call.
Same-producer, same-revision invalid refreshes keep valid features. Source-revision CAS and call
budgets stay in force. The communication extractor applies to communication and web briefs; other
families are `not_applicable` (coverage computed over applicable count) and keep deterministic
features. Development backfill allowance: 60 calls/minute and 60 per pass, two concurrent requests
(repo config, live config and defaults agree); every wave rechecks foreground pressure.
`make test-attention-recovery` covers this area.
