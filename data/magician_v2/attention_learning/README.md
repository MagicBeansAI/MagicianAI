# Slice-2 actionability offline evaluation

`semantic-coverage-frozen-v1.json` is the content-free Increment 1 control-plane
fixture. It freezes the contracts that must hold before actionability training
or ranking is useful: scope-and-surface checkpoints resume without skipping or
duplicating eligible candidates; coverage requires an exact source revision and
semantic producer identity; Worth-a-look revisions are opaque strings; repeated
administrative scheduling is idempotent and performs no extraction inline; and
all shipped learning/backfill modes remain inert by default.

The future coverage replay must report its denominator and candidate-total
reconciliation. `missing`, `invalid`, stale, incompatible, pending, leased,
retrying, and exhausted work are not counted as compatible coverage. The admin
operation is intentionally schedule-only: workers own leases, bounded model
calls, validation, and persistence after the request returns. This fixture is
authored evidence, not evidence that a replay was run during this increment.

The frozen fixture contains only structured, content-safe features and typed
outcomes. `action_completed` is positive; `irrelevant`, `not_actionable`,
`obsolete`, and `not_owner` are negative. `useful`, neutral, duplicate,
timing-only, and no-interaction rows are excluded.

Future validation command (do not run during phased implementation):

```bash
python3 scripts/eval_attention_actionability.py \
  --fixtures data/magician_v2/attention_learning/actionability-frozen-v1.json \
  --snapshot data/magician_v2/attention_learning/results/actionability-snapshot-v1.json \
  --report data/magician_v2/attention_learning/results/actionability-report-v1.json
```

The tool uses group-disjoint chronological fit, hyperparameter-selection,
Platt-calibration, and final-test partitions. It reports precision@20,
recall@20, Brier score, ECE, and named slices. A generated snapshot is not
activated automatically: it must be reviewed, installed immutably in the
scoped attention store, and referenced by explicit `snapshot_id` in shadow
mode before any enforcement can be considered.

The generated snapshot also pins the semantic extractor contract, prompt
version, model, and profile declared by the fixture. Runtime inference rejects
otherwise valid features produced by a different semantic identity.

The live trainer is now in-process Rust, not this fixture script. Magician and
`magician attention-learning train-actionability` both call
`attention_learning::training::train_actionability`, which reads scoped
outcomes from `attention_learning.db`, joins them to the captured serve-time
vector (raw Follow-up id or last-served binding), and emits a snapshot that
`validate()` accepts. The Python command remains the frozen-fixture eval; it
is not how production snapshots are produced.

After review, install a snapshot with the one-purpose admin command:

```bash
MAGICIAN_SKIP_KEYCHAIN=1 ./magician.bin attention-learning \
  install-actionability-snapshot \
  --snapshot data/magician_v2/attention_learning/results/actionability-snapshot-v1.json
```

The command accepts only a regular JSON file up to 2 MiB, parses and fully
validates the snapshot contract, and installs it immutably in the resolved
Attention Learning store. Replaying identical content is idempotent; reusing a
snapshot ID for different content is rejected. Installation never activates
the snapshot and never edits configuration. To prevent a preselected missing
artifact from becoming active as a side effect, the command rejects installing
an ID already selected by non-disabled runtime configuration. The actionability
mode therefore remains `disabled` until a separately reviewed rollout
explicitly selects the snapshot.

## Slice-3 pair grouping audit

Slice 3 treats duplicate/underlying-obligation identity as a calibrated
pairwise task. Runtime accepts only a pinned immutable snapshot carrying exact
feature, semantic-extractor, embedding, calibration, and audited merge-threshold
contracts. There is no independent runtime threshold knob.

Future frozen audit command (do not run during phased implementation):

```bash
python3 scripts/eval_attention_pair_grouping.py \
  --fixtures data/magician_v2/attention_learning/pair-frozen-v1.json \
  --report data/magician_v2/attention_learning/results/pair-report-v1.json
```

The audit is precision-first: it reports merge precision/recall,
false-merge count/rate, and named slices. The checked-in
`pair-report-v1.json` is a frozen expected report, not evidence that the command
was run during this implementation phase.

After review, install without activating:

```bash
MAGICIAN_SKIP_KEYCHAIN=1 ./magician.bin attention-learning \
  install-pair-model-snapshot --snapshot <pair-model-snapshot.json>
```

Identical replay is idempotent; same-ID/different-content reuse is rejected.
The command also rejects a snapshot ID already selected by non-disabled
grouping configuration, preventing installation from activating a preselected
missing artifact.

## Slice-4 lane-routing and verified-impression replay

`routing-frozen-v1.json` is a content-free complete-universe replay fixture.
It audits candidate-total reconciliation, actionable-recall preservation,
irrelevant Follow-up reduction, high-confidence cross-lane gates, verified
impression coverage, and event-id deduplication. API return and missing
interaction are never labels.
The policy fixture pins an exact optional Slice-3 grouping snapshot/model pair,
and every candidate row must match it before duplicate-cost evaluation.

Future frozen replay command (do not run during phased implementation):

```bash
python3 scripts/eval_attention_routing.py \
  --fixtures data/magician_v2/attention_learning/routing-frozen-v1.json \
  --report data/magician_v2/attention_learning/results/routing-report-v1.json
```

`bandit-ope-frozen-v1.json` is the content-free Slice-5 exact-propensity OPE
fixture. `scripts/eval_attention_bandit_ope.py` implements IPS, SNIPS, doubly
robust, SWITCH, grouped bootstrap confidence intervals, ESS, overlap/support,
and importance-weight diagnostics. Its `--self-test` is synthetic and is also
authored but not run during phased implementation.

Install a reviewed immutable policy without activating it:

```bash
MAGICIAN_SKIP_KEYCHAIN=1 ./magician.bin attention-learning \
  install-routing-policy-snapshot --snapshot <routing-policy-snapshot.json>
```

Routing defaults to `baseline`. The first list integration persists complete
decisions and shadow proposals, but it marks its lane-specific input as an
incomplete cross-lane universe. Canary changes therefore fail closed until one
authoritative projector can load, route, group, rank, and paginate the union of
baseline Follow-up and Worth-a-look candidates without making moves disappear.

### Slice-4 Increment 2 canonical-union contract

`canonical-union-frozen-v1.json` freezes the content-safe integration contract
for the authoritative cross-lane projector. It intentionally gives one
Follow-up and one Worth-a-look source the same raw `candidate_id`: canonical
identity is origin-qualified, so concatenating the two stores cannot alias,
drop, or overwrite either source. The complete case also moves one origin of
each kind into the opposite learned lane, preserves its exact origin payload
and origin-owned action descriptors, and keeps every grouped member available
with reconciled representative/member totals.
Follow-up descriptors freeze `open_source`, `approve`, `acknowledge`, `useful`,
`dismiss`, and `snooze`; Worth descriptors freeze `open_source`, `useful`,
`acknowledge`, and `dismiss`. Methods and exact origin-owned hrefs travel with
the descriptor, so the receiving lane never invents an action.

Learning evidence remains origin-scoped inside the union. Follow-up candidates
consume Follow-up rank/posterior, pair-label, and cannot-link state; Worth
candidates consume the corresponding Worth state. Existing within-origin pair
records are remapped from raw lane-local IDs to origin-qualified canonical IDs
before grouping. An unlabeled cross-origin pair may receive calibrated model
inference, but it cannot borrow an owner pair label or cannot-link from either
surface merely because one raw ID matches.

Baseline semantics include order, not only route labels. Learned ranks may be
recorded as diagnostics, but shipped `baseline` plus dormant
`canary_fraction: 0.0` preserves each origin's atomic order and applies no
cross-lane or non-surfaced move.

Fail-closed `fallback_reason` values are bounded codes; raw store, path, and
error-chain detail stays in server logs and never becomes response identity.

Both legacy list responses expose the same additive top-level
`canonical_attention_projection`; the standalone read is
`GET /api/magician/v2/channel-assist/attention-learning/canonical-projection`.
All three consumers must observe one projection ID and one complete-union
digest for the same complete source snapshot. Replaying the same union is
idempotent. Schema-v1 projection JSON frozen before exact cross-lane
reconciliation remains readable: missing `cross_lane_reconciliation` becomes
bounded `unavailable/canonical_projection_unavailable` health and missing
`duplicate_aliases` becomes an empty list. Current responses still serialize
both additive fields, so their absence is never mistaken for fresh successful
reconciliation. A changed generation, short/incomplete load, source-count
mismatch, or unavailable projector cannot publish a partial learned result:
the affected request serves its original atomic legacy baseline and reports a
typed fail-closed projection status. Baseline and zero-fraction canary defaults
are complete but non-applying: shipped mode is `baseline` and its dormant
`canary_fraction` is `0.0`.

The static integration contract is
`magician/tests/attention_canonical_projection_contract.rs`. The fixture and
tests were authored for the phased validation pass and were not executed while
the implementation phases remained open.

### Slice-4 Increment 3 frozen multi-page delivery

`multi-page-delivery-frozen-v1.json` freezes pagination as delivery from one
immutable lane root, not a new policy decision per request. The first request
loads the complete canonical lane, binds the exact candidate revisions,
projection ID, complete-union digest, policy/model snapshot, posterior version,
seed, scope, and expiry, then samples the complete root order exactly once.
Every page has contiguous root positions and a persisted page identity. A
retry of the same opaque cursor returns the same page, delivery ID, items,
revisions, exposure tokens, and propensities without another policy draw.

The first slate deliberately contains distinct propensities strictly between
zero and one. `root_policy_propensity` is the exact sequential probability
used by OPE for that item in the frozen root; it remains attached when the item
is delivered on a later page. `conditional_delivery_propensity` is `1.0`
because selecting a page from an already persisted root is deterministic.
Replacing the root propensity with one on later pages would erase the causal
logging contract, while multiplying by a second exploration probability would
invent exploration that never occurred.

Cursors and exposure tokens are opaque persisted capabilities. They bind the
principal/workspace, lane, root decision, projection/digest, exact revision,
page, position, item, and expiry. An expired or unknown cursor, scope mismatch,
projection/digest drift, source-revision drift, or lane/root binding mismatch
returns typed HTTP 409 `refresh_required`; serving a partial or newly sampled
continuation is forbidden. Follow-up and Worth-a-look roots may share one
canonical projection/digest but always have different decision IDs, and their
delivered candidate sets never overlap.

The root-frozen, response-owned `impression_policy` supplies the dwell threshold
and visibility-rule version, including on cursor replay after mutable config
changes. A delivery impression is accepted only when the
event's delivery ID, page, position, candidate, exact revision, scope, and
exposure token verify against a delivered item and the configured dwell is
met. Idempotent retry of the same client event preserves one impression and
one exposure contribution. The impression row copies both propensities and
the complete delivery binding, so later delivery-ledger compaction cannot
erase OPE facts. API return, undelivered candidates, guessed positions,
expired tokens, and below-threshold visibility are not verified impressions.
Outcome attribution keeps the delivery-root decision ID from that receipt,
resolves its bound canonical projection only to recover frozen features, and
verifies personal policy/model/posterior/position/propensity from the delivery
ledger before one idempotent posterior update. It must not require the older
projection item to contain the later delivery policy draw.

Disabled, shadow-only, non-canary, missing, or degraded bandit state produces a
persisted deterministic `baseline_fallback` root: baseline order, null policy
identity when disabled, posterior version zero, seed `baseline`, and both
propensities equal to one. Bounded fallback codes expose no raw error detail.
Delivery defaults are page size 50, maximum 200, TTL 900 seconds, retention 30
days. Shipped YAML is `bandit.mode: shadow` with no snapshot pin; Magician
auto-installs a store snapshot and may later promote it to canary.

Delivery retention owns five scoped tables:
`attention_delivery_decisions`, `attention_delivery_decision_items`,
`attention_delivery_pages`, `attention_delivery_page_items`, and
`attention_delivery_cursors`. One scoped transaction manually cascades only
expired roots older than the retention horizon; unexpired roots, other owner
scopes, copied impression facts, and global immutable policy snapshots remain.
Scoped deletion removes all five delivery tables only for the requested
principal/workspace. The existing impression ledger retains its independent
retention ownership.

The static integration contract is
`magician/tests/attention_multi_page_delivery_contract.rs`. It and the frozen
fixture are authored for the final validation pass and were not executed while
implementation phases remained open.

### Slice-4 Increment 4 asynchronous rank recompute

`rank-recompute-frozen-v1.json` freezes the feedback-to-current-rank boundary
as durable asynchronous work. Every accepted canonical outcome has at most one
scoped `attention_rank_recompute_jobs` row under
`UNIQUE(principal, workspace, outcome_id)`. A successful outcome response is
immediate: `rank_recompute` is `enqueued`/`pending`, its current-universe
`affected_rank_after` and delta are null, and the client receives the scoped
status URL. If enqueue itself fails, feedback remains accepted and reports the
bounded `enqueue_failed` reason; the reconciliation scheduler may later create
the one missing job without duplicating the outcome.

Workers, not list/page handlers, own current-universe projection, posterior
read, model work, and persistence. The frozen lifecycle is
`pending -> in_flight -> retry -> in_flight -> succeeded`, with bounded leases,
backoff, dead-letter exhaustion, attempt-bound completion, and idempotent
terminal writes. A completed result is explicitly a
`current_universe_diagnostic`: it binds exact current source revision, complete
union digest, recompute generation, posterior version, immutable policy
snapshot, completion time, after rank, and delta. The before rank remains the
served decision/delivery position and is never rewritten as a current rank.

Completion uses source-revision, complete-universe digest, origin generations,
posterior, and snapshot compare-and-set guards. Inactive, revised, or
incompatible candidates terminate stale, as do universe/generation changes
during commit; none publish an after rank. The recompute universe reconciles
every Follow-up and Worth-a-look origin exactly once and reports per-origin
feature/posterior evidence. Delivery-native outcomes and both legacy feedback
adapters enter the same job semantics. Reads and pagination never claim jobs,
wait for them, or call the model.

The scoped polling endpoint is
`GET /api/magician/v2/channel-assist/attention-learning/rank-recompute/jobs/{job_id}`;
queue health is exposed separately at `rank-recompute/status`. Scheduling and
bounded processing are explicit `POST .../schedule` and `POST .../process`
admin boundaries with `{ "apply": boolean }`. The code default is disabled
(the shipped `magician-config.yaml` seed sets `rank_recompute.enabled: true`),
and disabled processing reports paused health without leasing work.
Terminal-job retention preserves pending/in-flight/retry rows, accepted
outcomes, and posterior idempotency anchors; scoped deletion removes only the
selected owner's jobs/outcomes/posterior while preserving other scopes and
global immutable snapshots.

Future content-free contract evaluation (do not run during phased
implementation):

```bash
python3 scripts/eval_attention_rank_recompute.py \
  --fixtures data/magician_v2/attention_learning/rank-recompute-frozen-v1.json \
  --report data/magician_v2/attention_learning/results/rank-recompute-report-v1.json
```

The frozen expected report is `rank-recompute-report-v1.json`; it is not
evidence that the evaluator or static contract tests were executed.

### Slice-4 Increment 5 exact cross-lane identity reconciliation

`cross-lane-identity-frozen-v1.json` freezes the hard identity boundary between
Follow-ups and Worth a look. A Worth communication candidate aliases an active
Follow-up only when both sides parse to the exact typed tuple
`(provider, account_alias, thread_id)`. Percent-decoding is validation, not
fuzzy normalization. Titles, summaries, subjects, senders, participants,
message text, embeddings, semantic similarity, and LLM judgment are forbidden
alias signals.

The Follow-up owns the one visible card and its actions. Each matching Worth
row becomes a durable, origin-preserving alias, but never enters ranking,
grouping, routing, materialization, or a delivery cursor. Its source lifecycle,
revision, and feedback history remain intact, so it reappears automatically
when the exact Follow-up owner is no longer active. Multiple Worth messages are
suppressed only when each independently resolves to that exact active thread;
similar content on a different typed thread remains visible.

Malformed communication references are never treated as independent verified
identities and never rescued by a text or embedding heuristic. Reconciliation
becomes typed `unavailable`, canonical publication fails closed, and the
server-side legacy Worth guard withholds unverified communication candidates.
Non-communication Worth sources remain eligible. The diagnostic is visible but
content-free. Public alias summaries contain scoped opaque identities, expose
no provider/account/thread/message/source-ref values, and are bounded to 100.

For valid source snapshots the frozen equations are:

```text
raw = follow_up_raw + worth_raw
duplicate_hidden = worth_aliases
reconciled = raw
raw = materialized + duplicate_hidden
raw = grouped_members + duplicate_hidden
grouped_representatives + grouped_duplicate_members = grouped_members
lane_total = grouped_representatives
```

The complete-universe digest is deterministic lowercase 64-character BLAKE3
hex and binds the alias set plus both owner and origin revisions. Reconciliation
happens before ranking, grouping, routing, and pagination; an alias can
therefore never reappear on a later page. The static contract is
`magician/tests/attention_cross_lane_identity_contract.rs`.

Future content-free audit command (do not run during phased implementation):

```bash
python3 scripts/eval_attention_cross_lane_identity.py \
  --fixtures data/magician_v2/attention_learning/cross-lane-identity-frozen-v1.json \
  --report data/magician_v2/attention_learning/results/cross-lane-identity-report-v1.json
```

The fixture, static contract, and evaluator are authored for the deferred
all-phases validation pass; their presence is not evidence that they ran.

## Slice-5 off-policy replay

`bandit-ope-frozen-v1.json` is content-free synthetic estimator evidence, not
promotion evidence. Its estimand is an explicit position-level reward, so each
weight uses the sequential conditional propensity for that position rather
than a joint-slate product. `target_supported_mass` must be computed from the
full candidate/action set at each decision-position; a selected logged action
alone cannot establish logging-policy support or overlap.
