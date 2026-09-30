# Outcome learning — what actually worked

OPC outcome learning. Plan: `docs/archive/plans/2026-08-07-opc-outcome-learning.md`.
Module: `magician-learning/src/outcome_learning/`.

Five phases — recording, the maturity policy that turns silence into a fact,
the cohort comparison that turns observations into candidates, cross-audience
aggregation, and retirement/contradiction — plus four feeders (`feeders.rs`)
that convert real data into each phase's input: proposal candidates into what
`learning/` consumes (withholding any below the maturity policy's N floor,
*with* the count), data-room access events into a market read (visits from
`sequence`, ghosts preserved), the outward-assertions reverse index into
retirement's `ClaimUse`, and recorded acts into maturity's `AwaitingOutcome`.
No proposals are applied, nothing is scored. Recording is purely additive
because *"the data cannot be reconstructed afterwards."*

**The one writer is the maturity sweep** (`outcome_learning::sweep`, cadence
`outcome_learning::worker`, book `outcome_learning::book`, config key
`outcome_maturity`, boot entry `book::spawn_configured_maturity_sweep`). It
records `silent` for acts that went quiet. Nothing else records `replied`,
`accepted`, `rejected`, `opened` or `progressed`; those wait on each channel's
reply side. Survivorship bias runs both ways: a replies-only store looks like a
world where everybody answers, a silences-only store says nothing came back.

**The sweep ships off.** It records a judgement (no answer inside a window means
the counterparty decided) into an append-only store that cannot un-write it, so
`enabled` defaults to `false`, `scopes` to empty, and an enabled sweep naming no
scope declines to start and says so through its health snapshot.

**Settled is checked before ripe.** `mature_silences` consults what is already
recorded *before* deriving whether an act has ripened. The other order makes an
act already concluded silent report as *"still open"* once somebody widens the
maturity window — no double record, but a wrong answer a caller would act on.
Cost: one store read per act per sweep; unripe acts are bounded by the window.

## Generic, not a fundraising feature

An observation is *"this variant of this act produced this result"* — the same
shape whether the act was a pitch, a support reply, a proposal or a scheduling
message. Nothing in the module names a domain.

`Confounder` is deliberately open (`kind`/`value`) rather than an enum: the
confounders that matter are domain-specific and discovered late, and a closed set
would force the interesting ones to be recorded as nothing at all.

## The guardrail comes before the loop

§2, and it is why phase 1 is more than a table. Three of the plan's five "must
nots" are enforced by what the types **cannot express**, and two by refusals.

### Silence is refused before its window closes

Silence is the *absence* of a signal, so before maturity it is indistinguishable
from "not yet". Recording it early fills the sample with counterparties who
simply had not replied by Tuesday, and every later comparison inherits that.

`OutcomeLabel::Silent` requires `matured_at`, and the store refuses it while
`now < matured_at`. Every other label is an event, and an event is true the
moment it happens.

### The cohort key is not optional

§5: *"record which proposal version was active for every action, and compare
outcomes across versioned cohorts."* An empty `variant_version` is refused,
because without it there is nothing to compare against and the loop can only
agree with itself.

**Self-confirmation is prevented by cohort separation, not by amnesia**, so
accepted proposals stay in the sample: excluding them would throw away the
post-change observations that show whether the change helped.

### Delivery is tracked apart from response

A bounce recorded as `silent` counts as a counterparty ignoring us when nothing
arrived — the single most misleading confusion available here.
`DeliveryState::reached_someone()` gates it, and `usable_cohort` filters on it.

`cohort` and `usable_cohort` are **separate on purpose**: the full set is what an
owner should see, the usable subset is what a comparison may run on. Collapsing
them would make "we observed thirty" and "thirty are comparable" the same number,
which they are not.

### The owner is not observed

There is no field for the operator. The subject of an observation is always an
act and a counterparty, so recording the person running the system is not
something a caller can do by accident.

### No tracking is added

§3 allows only outcomes already recorded plus what the outward adapters report.
`Opened` exists because visiting a room is an act on our own surface — no pixel
or beacon is ever added to an email to manufacture it.

## Maturity (phase 2)

Phase 1 *refuses* premature silence. That is the guardrail, not the mechanism.
Something has to decide **when** a window has closed and turn the quiet acts into
observations, or the sample contains only the counterparties who replied — the
most flattering possible dataset and the least useful.

`MaturityPolicy` answers *how long do we wait for this kind of act*;
`mature_silences` applies it. They are separate because the window is a
domain judgement — an accelerator silent for two weeks has decided, a support
ticket silent for two hours has not — while the sweep is mechanical. Mixing them
would bury "we waited two weeks" in a loop instead of leaving it a configuration
somebody chose. A non-positive window is refused: zero matures silence the
instant an act is sent, recording a decision nobody had the chance to make.

### Anything that came back stops the maturation

The sweep reads the act's existing observations rather than trusting the caller's
"still awaiting" list, which is exactly the thing that goes stale — and the cost
of staleness here is a recorded claim that somebody ignored us when they did not.

**Any** recorded outcome counts, not only an engagement. A **rejection** is the
case that makes the distinction matter: it is not engagement, but the
counterparty answered, and maturing it into silence would record that they both
replied and ignored us.

Silence is stamped with the moment its window closed, not the moment the sweep
happened to run — so when the sweep is late, the data does not say the
counterparty was.

`confounder_kind` names the four §3 calls out (introduction, introducer, stage,
org size). Constants rather than an enum, because `Confounder` stays open — but
naming them stops one idea being recorded as `warm`, `warm_intro` and
`introduced` in three places, splitting a cohort that should be one.

### The sweep that runs it

`mature_silences` is a decision; `outcome_learning::sweep` is its caller and
`outcome_learning::worker` is the cadence. Three layers, so each can be tested
without the others:

- `collect_acts` loads the named acts from the outward-assertions store and joins
  each to the **cohort key it was performed under**. The key is supplied — an act
  records what went out, never which variant version was live — and an act ref
  the store does not hold is reported as `NoSuchAct`, never swept. Silence about
  an act we cannot read is a claim with no basis.
- `sweep_matured_silences` runs `awaiting_outcomes` and `mature_silences` and
  records what matured. The only function that writes.
- `run_maturity_sweep` does both, and **refuses two scopes that name different
  tenants**: reading one principal's acts while recording another's observations
  is a wrong answer both stores would report as a success.

Acts are discovered through the assertion store's reverse index. The primitive is
`act_refs_for_work(store, scope, &WorkContextKind)` — it reads the axis from the
kind's own wire token, so a programme, an engagement and any third kind of work
are all reachable without editing it. `act_refs_for_engagement` is only a
wrapper for callers that already hold an engagement id; an engagement-keyed
primitive would make every other kind of work unreachable.

`WorkspaceMaturityBook` (`outcome_learning::book`) is what a tick's work actually
comes from:

- **Scopes** are declared in configuration, not discovered from the storage root.
  The obligation sweep discovers its tenants because it writes reminders; this
  one writes observations that become evidence, so adopting a stale, system or
  ephemeral-eval scope would record judgements nobody chose.
- **Acts** are discovered, per scope, by unioning every index file under each of
  `WorkContextKind::KIND_TOKENS` — no roster of engagement ids required, because
  a roster is exactly what makes work nobody remembered to list invisible to the
  sweep whose job is remembering.
- **Cohorts** come from an `ActCohortSource`. The shipped one,
  `DeclaredPayloadVariants`, maps declared payload artifact references to a
  `(variant_ref, variant_version)` pair; see *Not built here* for why that is a
  declaration rather than a lookup.

**Running it twice records nothing twice**, held by three layers rather than one:
`mature_silences` short-circuits an act that already carries a `Silent`
observation *before* re-deriving its maturity instant, so widening the window in
configuration cannot move a decision already made; it re-reads each act's own
observations, so an act answered between sweeps never matures; and
`OutcomeStore::record` is idempotent on `(act, variant, version, label)`
underneath both.

The worker is **off unless configured on**, and refuses a non-positive window at
startup rather than substituting a default — the observations it would write
cannot be un-recorded. `book::spawn_configured_maturity_sweep` is the one entry
point: it declines *visibly* when the switch is off (`disabled`, no error, which
is what an operator who set it expects to read) and *degraded with a reason* when
the sweep is enabled and its book cannot be built.

Three zeros are told apart rather than collapsed:

| what happened | state |
|---|---|
| the book named no scopes | `degraded` — an empty book and an unreadable one produce the same counts |
| a scope's sweep failed | `degraded`, and the other scopes still swept |
| acts were found and **none** carried a declared cohort | `degraded`, with the count |
| scopes swept, nothing had ripened | `idle` — a quiet day, the ordinary case |

## Proposals (phase 3)

§4: *"A proposal that cannot state its N does not get made."*

### What it deliberately is not

It **does not apply anything.** §8's first control is that every change is an
owner editorial decision, so the output is a `Candidate` — a thing to look at —
and there is no function that acts on one. That absence *is* the control.

It **does not score, rank or recommend.** A loop that ranked its own candidates
would be optimising a metric nobody chose, which §5 forbids. A test asserts the
serialised candidate contains no `rate`, `score`, `rank` or `confidence` field.

`engagement_counts()` returns counts, never a rate: on a sample of six, `0.33`
versus `0.50` is two replies versus three, and the decimal reads as a precision
that is not there.

### The refusals are the feature

| refusal | why |
|---|---|
| sample too small | §8 — a proposal that cannot state its N is not made |
| too few counterparties | §5 — one rejection is a fact about that counterparty, not the pitch |
| confounded | §3 — otherwise an introducer's effect gets attributed to a subject line |
| no evidence cited | §9 — a claim nobody can check looks like a finding and cannot be audited into one |
| no difference | two cohorts that behaved identically are not a finding |

`NotProposable` is **returned**, not logged. *"We cannot say anything yet, and
here is what is missing"* is the useful answer to an owner asking why nothing has
been proposed — silence from a learning loop is indistinguishable from a broken
one.

### Confounders: the union, not one side

Comparison walks the **union** of confounder kinds across both cohorts. Iterating
only the baseline's misses the worse case: a kind dominant in one cohort and
entirely **unrecorded** in the other. That is *no information*, not *no
difference*, and treating an absent confounder as absent-in-fact is how a cohort
that simply was not measured passes as comparable.

Dominance is a simple **majority**, not a plurality: at 40/30/30 no value
explains the cohort, and calling the 40 dominant would block proposals on what is
really just variety.

A confounder that differs without dominating becomes a **caveat carried on the
candidate**, not a block and not a filter — hiding it would hand the owner a
cleaner story than the data supports.

### Counterparties are counted by engagement

An observation with no engagement cannot be attributed to anyone, so it does not
count toward diversity. Otherwise one counterparty plus four anonymous rows would
read as five.

## Aggregation (phase 4)

§6: *"the financials were opened by nine of eleven; the team slide by two —
a fact about what this market finds load-bearing, obtained from thirty
conversations you were having anyway."* `aggregate` is pure functions over
supplied room attention: counts only, distinct tokens not events, and the
`shared_with` denominator includes **ghost tokens** — shared, never appeared in
the log — because dropping them inflates apparent engagement, the exact
dishonesty the module exists to prevent. A room with no attention rows has an
unknowable document set and contributes nothing, said plainly. The market floor
reuses `MINIMUM_COUNTERPARTIES`: one audience is a fact about them, not the
market. `question_frequency` normalises spelling but deliberately not meaning —
clustering synonymous questions would be an LLM judgment §5 forbids.

## Retirement and contradiction (phase 5)

Surfaced *"against the reverse index in outward assertions — what was actually
said, rather than a separately-maintained record of what we meant to say."*
Pure over supplied `ClaimUse` rows. A claim idle past the policy window is a
retirement candidate carrying its N and every use ref; a **superseded** claim is
excluded from staleness (replaced is a different fact from stale). A
contradiction is a use of the old claim **strictly after** the supersession
existed — uses at the boundary instant are clean, because order there is
unknowable. No auto-retire, no scores: descriptions for the owner.

## Storage

Append-only per act; the current picture is the fold. One act legitimately
carries several outcomes over time — accepted, then delivered, then replied — so
the label is part of the observation id and those are distinct observations
rather than corrections of each other. A later reply does not rewrite an earlier
silence: both are true of their own windows, and an outcome that can be edited
afterwards is not evidence.

### Corrections land; retries do not inflate

Recording is **idempotent** on `(act, variant, version, label)` — but only for a
genuinely identical re-observation. When something known about the act has
changed, almost always the **delivery state** (which arrives after the outcome it
describes), the update is appended and folded **last-wins**.

Keeping the first line instead would make corrections unreachable: an act
recorded `delivered` that a provider later reports as bounced would stay
`delivered` for ever and be counted as a counterparty ignoring us when nothing
arrived. `observed_at` is held at first sight, so a correction arriving on Friday
does not make the outcome look like it was observed on Friday, and no second
cohort pointer is written, so the sample size does not grow. A poller that
runs twice or a replayed webhook must not inflate the sample — a count that grows
on retry is the quietest possible way to make a small sample look significant.

The cohort index is written when the observation is, and **before** the row, so
"the row exists" implies "the index exists". Reading a cohort parses each act's
file **once**: resolving every index entry independently re-reads a whole act
file per observation in it, which degrades quadratically on exactly the act that
accumulated the most outcomes.

An index line that will not split on U+001F is **refused, not skipped**. Every
line is an act ref and an observation id joined by that separator, and every
caller string that feeds an observation id, the cohort key or the index line —
the scope's principal and workspace, the act ref, the variant ref, the variant
version — is refused if it carries one. Reading past a line that cannot be split
would drop a recorded observation out of the cohort and report a short sample as
a whole one, and the smaller a sample is the more decisive a difference in it
looks.

## Not built here

- **Any writer other than the maturity sweep.** Nothing records `replied`,
  `accepted`, `rejected`, `opened` or `progressed` yet, and the delivery signals
  need the provider receipts that no adapter reports (see
  `docs/components/magician/outward-assertions.md`). The sweep carries whatever
  delivery state the act's status supports, so an act nobody confirmed is
  recorded as `unknown` — it counts as an act performed and never as evidence
  about a counterparty.
- **A cohort key any subsystem produces.** This is the live gap. `variant_ref`
  and `variant_version` exist nowhere in production outside this module: an
  outward act records what went out, to whom, on which exact payload and when,
  and never which variant version was live when it did. Plan §2 is explicit that
  where no deterministic fact is available the answer is a person's, so the book
  takes the declaration from configuration — `outcome_maturity.scopes[].variants`
  maps exact payload artifact references to a `(variant_ref, variant_version)`
  pair. An act whose payload nobody declared is **counted and left unswept**,
  never bound to a default: an observation filed under a cohort key nobody chose
  is comparable to nothing and cannot be withdrawn. A tick that found acts and
  bound none of them reports `degraded` with the count, because that state
  otherwise produces the same zeroes as a quiet day.

  What would replace the declaration: the deciding subsystem recording the
  variant version alongside the act at the moment it acts. `ActCohortSource` is
  the seam for it — a second implementor needs no edit to the book.
- **A count of acts no work names.** `MaturityScope::acts_without_work` reports
  them; nothing sweeps them. An act bound to an account, a panel or a person has
  no field on the disclosure to be indexed under, so no work axis reaches it. The
  fix is a field on `OutwardActDisclosure`, not a wider axis list.

## The composition layer — what calls the feeders

Module: `magician-learning/src/outcome_learning/composition/`.

`composition/` is the caller for three of the four `feeders.rs` conversions;
the maturity sweep is the caller for the fourth.

It lives **inside** this subsystem rather than beside it for a specific reason:
`feeders.rs` already imports the data room, the share ledger's access lane, the
learning substrate and the outward-assertions record, and none of those four
imports `outcome_learning`. A coordinator here therefore adds no dependency edge
at all, where a sibling module would add four. The rule the coordinator keeps is
the one `delivery_hygiene` and `obligation_sweeps` keep: neither primitive learns
the other.

| conversion | what it feeds | entry point |
|---|---|---|
| `candidates_to_learning` | a learning candidate an owner decides on | `composition::worker::OutcomeProposalWorker::spawn` |
| `room_attention_from_access` | the cross-audience market read | `GET /api/magician/v2/outcome-learning/market-read` |
| `claim_uses_from_index` | stale and contradicted claims | `POST /api/magician/v2/outcome-learning/claim-health` |
| `awaiting_outcomes` | the maturity sweep | `book::spawn_configured_maturity_sweep` (not this module) |

Worker health is mounted at `GET /api/magician/v2/learning/outcomes/maturity/watch` and `GET /api/magician/v2/learning/outcomes/proposals/watch` (`magician-api/src/outcome_learning_api.rs`). Both refuse with `503` when no health is attached — absent is not a page of zeroes.

### The proposal pass refuses to propose over a reply-only sample

`MaturitySweepStanding` carries two facts — which tenants the maturity sweep
covers, and whether one of its ticks has actually completed — and a scope
neither fact holds for has **every comparison withheld with both cohorts'
sample sizes**, never proposed. A cohort read out of a store that only ever
receives replies is not evidence about a variant: everyone who ignored us is
missing from it, so `engaged 4 of 4` and `engaged 4 of 40` are the same number.
Configuration alone is not enough for the same reason a config switch is not a
running loop: a sweep that was enabled and then declined its own window has
recorded nothing.

The withheld counts are the point. *"We have no evidence"* and *"we have three
observations, the floor is five, and no silence sweep is running"* are different
answers, and only the second says which switch to turn on.

### Which comparisons get made, and the version-order gap

`comparisons_in_scope` reads `OutcomeStore::recorded_cohorts` — a new store
enumeration, because the cohort files are named by a hash of
`variant<U+001F>version` and the pair is recoverable only from the observations
the index points at — and pairs **consecutive versions of each variant, ordered
by the earliest observation in each**. A variant with one version yields nothing.

**Nothing records that one version superseded another.** First-observation order
is right for a version rolled out after another and wrong for two run
concurrently as an A/B, where whichever was observed first reads as the
baseline. The candidate states both cohorts whole either way, so the finding is
still true; the words "baseline" and "candidate" would be the wrong way round.
A fix needs a version lineage on the declaration side.

### Idempotency: the decision is consulted before it is re-derived

The candidate id is derived from the tenant and the three names and from nothing
that moves — not the clock, not the sample size, not the floor — and the
substrate is asked whether it already holds that id **before any cohort is
read**. Same rule as `mature_silences`: consult the recorded fact before re-deriving.
So a second pass files nothing, and a floor raised
tenfold between passes still answers `AlreadyProposed` rather than
`SampleTooSmall`. The listing that answers it propagates a read fault rather
than folding to "no candidates": an unreadable substrate read as empty would
re-propose everything on every tick.

Every candidate is `review_required`, medium risk, in the proposed state, and
carries no `confidence` — the substrate's auto-apply lanes open only for a
low-risk candidate needing no review, so *"it never applies anything"* holds
structurally rather than by anyone remembering it.

### The market read

`market_read` lists a scope's rooms, reads each room's share list **from the
grant ledger** (one token per identity at its earliest issue, revoked and
expired grants kept) and each room's raw access events, and hands all three to
`room_attention_from_access`. Nothing in the composition derives a visit: that
comes from the access log's own `sequence`, so three clicks in one sitting are
one visit and only a second sitting is a return. The share list is never derived
from the events, because a token that never appears **is** the never-opened case
— dropping it would report a room half of whose shares went silent as fully
read.

`MarketRead` reports documents with `opened_by` beside `shared_with` and
`audiences` (counts, never a rate), the `market_backed` flag beside them rather
than filtering on it, the never-opened pairs as delivery questions, the tokens
that returned, and every access event that could not be attributed to a grant.

### Claim health, and the reverse pointer that does not exist

`claim_health` answers *"is this claim stale, and did we keep saying it after
correcting it"* from the assertion index. The translation `claim_uses_from_index`
performs — an index row's `supersedes` names assertion **uses**, and retirement
compares **claims** — can only run over the rows it is handed, and reading one
claim's axis hands it rows that name uses it does not contain. So the
composition closes over them: convert, load each unresolved use by id, **add its
claim and read that claim's whole history**, and repeat. Loading only the
pointed-at use would resolve the pointer and leave that claim's own history
partial, and a partial history under-reports offences — a false clean.

**The index has no reverse pointer from a superseded use to the use that
superseded it.** A supersession is declared on the newer claim's row, so asking
only about a stale claim can never discover its own correction; naming the
correction finds the pair. `ClaimHealth::claims_reached` is what keeps the
partial answer legible — a claim absent from it was not examined, which is not
the same as one examined and found healthy. Closing it properly needs a
`superseded` axis on the assertion index.

A report over **no claims is refused**, not answered: `retirement_candidates`
over an empty slice returns an empty list, which reads exactly like *"nothing is
stale"*.

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

`enabled` / `paused` / cadence / floors / destination type are what the pass
actually reads. It still applies nothing: every candidate is `review_required`,
medium risk, proposed.

- **Auto-discovered claims.** There is no enumeration of approved claims
  anywhere — `ClaimRecord` is ephemeral by design and the reverse index is keyed
  by hash — so the claim-health route answers about claims somebody names.
