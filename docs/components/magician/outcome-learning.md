# Outcome learning — what actually worked

Module: `magician-learning/src/outcome_learning/`. Plan (archived):
`docs/archive/plans/2026-08-07-opc-outcome-learning.md`.

The module has five parts: recording, the maturity policy that turns silence
into a fact, the cohort comparison that turns observations into candidates,
cross-audience aggregation, and retirement and contradiction. Four feeders
(`feeders.rs`) turn real data into their inputs: proposal candidates into what
`learning/` consumes (withholding any below the maturity policy's N floor,
with the count), data-room access events into a market read (visits from
`sequence`, ghosts kept), the outward-assertions reverse index into
retirement's `ClaimUse`, and recorded acts into maturity's `AwaitingOutcome`.
Nothing is applied and nothing is scored. Recording is purely additive, because
the data cannot be reconstructed later.

**The only writer is the maturity sweep** (`outcome_learning::sweep`, cadence
`outcome_learning::worker`, book `outcome_learning::book`, config key
`outcome_maturity`, boot entry `book::spawn_configured_maturity_sweep`). It
records `silent` for acts that went quiet. Nothing records `replied`,
`accepted`, `rejected`, `opened` or `progressed`; those wait on each channel's
reply side. Bias runs both ways: a replies-only store says everyone answers, a
silences-only store says nothing came back.

**The sweep ships off.** It records a judgement (no answer in the window means
the counterparty decided) into an append-only store, so `enabled` defaults to
`false`, `scopes` to empty, and an enabled sweep with no scope declines to start
and says so in its health snapshot.

**Settled is checked before ripe.** `mature_silences` reads what is already
recorded before deciding whether an act has ripened. The other order would make
an act already recorded silent read as "still open" after someone widens the
window. Cost: one store read per act per sweep.

## Generic, not a fundraising feature

An observation is "this variant of this act produced this result", whether the
act was a pitch, a support reply, a proposal or a scheduling message. Nothing in
the module names a domain.

`Confounder` is open (`kind`/`value`), not an enum: the confounders that matter
are domain-specific and found late, and a closed set would record them as
nothing.

## The guardrail comes before the loop

Three of the plan's five "must nots" are enforced by what the types cannot
express, and two by refusals.

### Silence is refused before its window closes

Before maturity, silence cannot be told from "not yet". `OutcomeLabel::Silent`
requires `matured_at`, and the store refuses it while `now < matured_at`. Every
other label is an event, true the moment it happens.

### The cohort key is not optional

An empty `variant_version` is refused: without it there is nothing to compare
against. Self-confirmation is prevented by cohort separation, not by amnesia,
so accepted proposals stay in the sample; their post-change observations show
whether the change helped.

### Delivery is tracked apart from response

A bounce recorded as `silent` would count as being ignored when nothing
arrived. `DeliveryState::reached_someone()` gates it, and `usable_cohort`
filters on it. `cohort` (what an owner sees) and `usable_cohort` (what a
comparison may run on) stay separate, so "observed thirty" and "thirty are
comparable" are different numbers.

### The owner is not observed

There is no field for the operator. An observation's subject is always an act
and a counterparty.

### No tracking is added

Only outcomes already recorded plus what outward adapters report are used.
`Opened` exists because visiting a room is an act on our own surface; no pixel
or beacon is ever added to an email.

## Maturity

Refusing premature silence is the guardrail; something must still decide when a
window has closed and turn quiet acts into observations, or the sample holds
only those who replied.

`MaturityPolicy` says how long to wait for a kind of act; `mature_silences`
applies it. The window is a domain judgement (an accelerator silent for two
weeks has decided; a support ticket silent for two hours has not), so it stays
configuration. A non-positive window is refused.

### Anything that came back stops the maturation

The sweep reads the act's existing observations rather than trusting a caller's
"still awaiting" list. Any recorded outcome stops it, including a rejection,
which is not engagement but is an answer. Silence is stamped with the moment
its window closed, not when the sweep ran, so a late sweep does not make the
counterparty look late.

`confounder_kind` names four constants (introduction, introducer, stage, org
size) so one idea is not recorded as `warm`, `warm_intro` and `introduced`,
splitting one cohort into three.

### The sweep that runs it

Three layers, each testable alone:

- `collect_acts` loads named acts from the outward-assertions store and joins
  each to the cohort key it was performed under. The key is supplied, since an
  act never records which variant version was live. An act ref the store does
  not hold is reported as `NoSuchAct`, never swept.
- `sweep_matured_silences` runs `awaiting_outcomes` and `mature_silences` and
  records what matured. It is the only function that writes.
- `run_maturity_sweep` does both, and refuses two scopes naming different
  tenants.

Acts are found through the assertion store's reverse index with
`act_refs_for_work(store, scope, &WorkContextKind)`, which reads the axis from
the kind's own wire token, so any kind of work is reachable.
`act_refs_for_engagement` is only a wrapper for callers holding an engagement
id.

`WorkspaceMaturityBook` (`outcome_learning::book`) supplies each tick's work:

- **Scopes** are declared in configuration, not discovered. Unlike the
  obligation sweep, this one writes evidence, so adopting a stale, system or
  eval scope would record judgements nobody chose.
- **Acts** are discovered per scope by unioning every index file under each of
  `WorkContextKind::KIND_TOKENS`; no roster is needed.
- **Cohorts** come from an `ActCohortSource`. The shipped one,
  `DeclaredPayloadVariants`, maps declared payload artifact references to a
  `(variant_ref, variant_version)` pair (see *Not built here*).

**Running it twice records nothing twice**, held three ways: `mature_silences`
skips an act that already has a `Silent` observation before re-deriving its
maturity, so a wider window cannot move a past decision; it re-reads each act's
observations, so an act answered between sweeps never matures; and
`OutcomeStore::record` is idempotent on `(act, variant, version, label)`.

The worker is off unless configured on and refuses a non-positive window at
startup rather than substituting a default. `book::spawn_configured_maturity_sweep`
is the one entry point: it reports `disabled` (no error) when the switch is off,
and `degraded` with a reason when enabled but the book cannot be built.

| What happened | State |
|---|---|
| the book named no scopes | `degraded` (an empty book and an unreadable one give the same counts) |
| a scope's sweep failed | `degraded`; other scopes still swept |
| acts found, none with a declared cohort | `degraded`, with the count |
| scopes swept, nothing ripened | `idle`, the ordinary case |

## Proposals

A proposal that cannot state its N is not made.

### What it deliberately is not

It **applies nothing.** Every change is an owner editorial decision, so the
output is a `Candidate`, and no function acts on one. It **does not score,
rank or recommend**; a test asserts the serialized candidate has no `rate`,
`score`, `rank` or `confidence` field. `engagement_counts()` returns counts,
never a rate: on six samples, `0.33` versus `0.50` is two replies versus three.

### The refusals are the feature

| Refusal | Why |
|---|---|
| sample too small | a proposal that cannot state its N is not made |
| too few counterparties | one rejection is about that counterparty, not the pitch |
| confounded | otherwise an introducer's effect is credited to a subject line |
| no evidence cited | a claim nobody can check cannot be audited into a finding |
| no difference | identical cohorts are not a finding |

`NotProposable` is **returned**, not logged: "here is what is missing" answers
an owner asking why nothing was proposed.

### Confounders: the union, not one side

Comparison walks the union of confounder kinds across both cohorts. A kind
dominant in one cohort and unrecorded in the other is no information, not no
difference. Dominance means a simple majority, not a plurality (at 40/30/30
nothing dominates). A confounder that differs without dominating becomes a
caveat on the candidate, not a block or a filter.

### Counterparties are counted by engagement

An observation with no engagement counts toward no one, so one counterparty
plus four anonymous rows does not read as five.

## Aggregation

`aggregate` is pure functions over supplied room attention: counts only,
distinct tokens not events. The `shared_with` denominator includes **ghost
tokens** (shared, never seen in the log), since dropping them inflates
engagement. A room with no attention rows has an unknowable document set and
contributes nothing, said plainly. The market floor reuses
`MINIMUM_COUNTERPARTIES`. `question_frequency` normalizes spelling but not
meaning; clustering synonyms would be a forbidden LLM judgement.

## Retirement and contradiction

Both run against the outward-assertions reverse index (what was actually said),
pure over supplied `ClaimUse` rows. A claim idle past the policy window is a
retirement candidate with its N and every use ref; a **superseded** claim is
not counted as stale. A contradiction is a use of the old claim **strictly
after** the supersession existed; uses at the boundary instant are clean. No
auto-retire, no scores.

## Storage

Append-only per act; the current picture is the fold. One act can carry several
outcomes over time (accepted, delivered, replied), so the label is part of the
observation id. A later reply does not rewrite an earlier silence; both are
true of their windows.

### Corrections land; retries do not inflate

Recording is idempotent on `(act, variant, version, label)` only for an
identical re-observation. When something about the act changed (such as the
delivery state, which arrives late), the update is appended and folded
**last-wins**, so a `delivered` act later reported bounced is corrected.
`observed_at` keeps first sight and no second cohort pointer is written, so a
retried poller or replayed webhook never grows the sample.

The cohort index is written before the row, so "the row exists" implies "the
index exists". Reading a cohort parses each act's file once, avoiding a
quadratic re-read on busy acts.

An index line that will not split on U+001F is **refused, not skipped**. Every
caller string that feeds an observation id, cohort key or index line (scope
principal and workspace, act ref, variant ref, variant version) is refused if
it contains U+001F. Skipping a line would report a short sample as whole.

## Not built here

- **Any writer other than the maturity sweep.** Nothing records `replied`,
  `accepted`, `rejected`, `opened` or `progressed` yet, and delivery signals need
  provider receipts no adapter reports (see
  `docs/components/magician/outward-assertions.md`). The sweep records the
  delivery state the act's status supports, so an unconfirmed act is `unknown`:
  an act performed, never evidence about a counterparty.
- **A cohort key any subsystem produces.** This is the live gap. `variant_ref`
  and `variant_version` exist nowhere in production outside this module. Where
  no deterministic fact exists the answer is a person's, so the book reads
  `outcome_maturity.scopes[].variants`, which maps exact payload artifact
  references to a `(variant_ref, variant_version)` pair. An undeclared act is
  **counted and left unswept**, never bound to a default, and a tick that bound
  none reports `degraded` with the count. The replacement is the deciding
  subsystem recording the variant version with the act; `ActCohortSource` is the
  seam, and a second implementor needs no edit to the book.
- **Acts no work names.** `MaturityScope::acts_without_work` counts them;
  nothing sweeps them. An act bound to an account, panel or person has no field
  on the disclosure to index under. The fix is a field on
  `OutwardActDisclosure`, not a wider axis list.

## The composition layer — what calls the feeders

Module: `magician-learning/src/outcome_learning/composition/`. It calls three of
the four `feeders.rs` conversions; the maturity sweep calls the fourth. It
lives inside this subsystem because `feeders.rs` already imports the data room,
the share ledger's access lane, the learning substrate and the
outward-assertions record, none of which imports `outcome_learning`, so it adds
no dependency edge. As with `delivery_hygiene` and `obligation_sweeps`, neither
primitive learns the other.

| Conversion | Feeds | Entry point |
|---|---|---|
| `candidates_to_learning` | a learning candidate an owner decides on | `composition::worker::OutcomeProposalWorker::spawn` |
| `room_attention_from_access` | the cross-audience market read | `GET /api/magician/v2/outcome-learning/market-read` |
| `claim_uses_from_index` | stale and contradicted claims | `POST /api/magician/v2/outcome-learning/claim-health` |
| `awaiting_outcomes` | the maturity sweep | `book::spawn_configured_maturity_sweep` (not this module) |

Worker health is at `GET /api/magician/v2/learning/outcomes/maturity/watch` and `GET /api/magician/v2/learning/outcomes/proposals/watch` (`magician-api/src/outcome_learning_api.rs`). Both return `503` when no health is attached; absent is not a page of zeroes.

### The proposal pass refuses to propose over a reply-only sample

`MaturitySweepStanding` records which tenants the maturity sweep covers and
whether one of its ticks has completed. A scope with neither has every
comparison withheld with both cohorts' sample sizes, never proposed: without a
silence sweep, `engaged 4 of 4` and `engaged 4 of 40` look the same.
Configuration alone is not enough, since an enabled sweep that declined its
window has recorded nothing. The withheld counts say which switch to turn on.

### Which comparisons get made, and the version-order gap

`comparisons_in_scope` reads `OutcomeStore::recorded_cohorts` (cohort files are
named by a hash of `variant<U+001F>version`, so pairs are recovered from the
indexed observations) and pairs **consecutive versions of each variant, ordered
by each one's earliest observation**. A variant with one version yields nothing.

**Nothing records that one version superseded another.** First-observation
order is right for a rollout and wrong for a concurrent A/B, where "baseline"
and "candidate" may be swapped. The candidate states both cohorts whole, so the
finding stays true. A fix needs version lineage on the declaration side.

### Idempotency: the decision is consulted before it is re-derived

The candidate id derives from the tenant and the three names only (not clock,
sample size or floor), and the substrate is asked whether it holds that id
**before any cohort is read**. A second pass files nothing, and a raised floor
still answers `AlreadyProposed`, not `SampleTooSmall`. A read fault in that
listing propagates rather than reading as empty, which would re-propose
everything each tick.

Every candidate is `review_required`, medium risk, proposed, with no
`confidence`. The substrate's auto-apply lanes open only for low-risk,
no-review candidates, so nothing is ever applied, structurally.

### The market read

`market_read` lists a scope's rooms, reads each room's share list **from the
grant ledger** (one token per identity at its earliest issue, revoked and
expired grants kept) and its raw access events, and passes them to
`room_attention_from_access`. Visits come from the access log's own
`sequence`, so three clicks in one sitting are one visit. The share list is
never derived from events, because a token that never appears is the
never-opened case.

`MarketRead` reports documents with `opened_by` beside `shared_with` and
`audiences` (counts, never rates), the `market_backed` flag (not a filter),
never-opened pairs as delivery questions, returning tokens, and every access
event that matched no grant.

### Claim health, and the reverse pointer that does not exist

`claim_health` answers "is this claim stale, and did we keep saying it after
correcting it" from the assertion index. An index row's `supersedes` names
assertion uses while retirement compares claims, so `claim_uses_from_index`
alone would see partial histories. The composition closes over them: convert,
load each unresolved use, add its claim, read that claim's whole history, and
repeat. A partial history would under-report, a false clean.

**The index has no reverse pointer from a superseded use to its successor.** A
supersession is declared on the newer claim's row, so asking about a stale
claim alone cannot find its correction; naming the correction finds the pair.
`ClaimHealth::claims_reached` lists what was examined, since an unexamined
claim is not a healthy one. A proper fix needs a `superseded` axis on the index.

A report over **no claims is refused**: `retirement_candidates` over an empty
slice returns an empty list, which would read as "nothing is stale".

### Configuration, and no claim auto-discovery

`MagicianConfig.outcome_proposal` (`OutcomeProposalSettings`) is read from
`magician-config.yaml`; `magician-bin` passes it into
`OutcomeProposalWorker::spawn`. The shipped block (on by default, unlike the
maturity sweep):

```yaml
outcome_proposal:
  enabled: true
  paused: false
  tick_interval_secs: 3600   # worker floors this at 60
  minimum_usable: 5
  minimum_counterparties: 2
  candidate_type: workflow_template
```

The pass reads `enabled`, `paused`, cadence, floors and destination type. It
still applies nothing.

- **No auto-discovered claims.** No enumeration of approved claims exists
  (`ClaimRecord` is ephemeral and the reverse index is keyed by hash), so
  claim health answers only about claims someone names.
