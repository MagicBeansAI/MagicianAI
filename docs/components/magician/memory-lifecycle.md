# Memory consolidation and lifecycle

Owner-memory writes retain revisions, reconcile related claims and ask about
uncertain conflicts through a shared, provider-independent reviewer.

## Shared structured user-memory path

`save_preference`, user-scope `update_memory_tier`, the web preference endpoint,
learned user promotions and remembered connection clarifications use shared
ingress. User extraction leaves conflict questions to this post-ingress reviewer.
Writes retain revisions instead of replacing an array element merely because
its key matches. A preference clear retracts matching current entries;
it does not erase their history. The web endpoint also retains its episode as
provenance; recording an episode alone is not the preference write.

A saved revision initially carries `memory_lifecycle: pending_review`. Tools
acknowledge durable storage and schedule review without waiting beyond their
execution deadline. The scoped background worker also resumes pending work.
The reviewer classifies facts, preferences, goals, instructions and observations,
then proposes duplicate, reinforce, coexist, supersede or `ask_owner` relationships.
It can match different keys and different user tiers. It does not rewrite the
owner's claim into model-generated prose.

Each revision has a stable record ID, an ingress identity and source evidence.
Independent source events can reinforce an existing claim; repeated copies of
one root event do not advance its observation date or increase its evidence
count. User promotions ground model-supplied evidence in actual input IDs and
literal source quotes, inheriting server dates/root execution identities. The
extraction schema requests a self-contained assertion and an exact supporting
excerpt, so a terse extracted value can retain its original correction context
in verified evidence. Invented excerpts cannot become evidence. An
uncited batch conservatively contributes one source, not one observation per
episode. Uncited attribution stores source identity/date rather than copying
arbitrary event payload into memory. Tool writes discard caller-supplied lifecycle
IDs and evidence counters. User extraction uses JSON Schema and validates actual
records before persistence. Recognizable legacy descriptive-schema copies are
quarantined in the private audit journal and excluded from current memory.
Legacy primitive collections are normalized lazily without discarding their text.

## Change policy

- A clear explicit correction can supersede an older claim about the same
  subject, aspect and context. Project/engagement boundaries remain enforced
  even if the model claims two items are equivalent.
- Different people, attributes, projects or applicability conditions can
  coexist. A temporary exception does not replace a standing preference.
- Duplicate/reinforcement labels describe a relationship, not full redundancy.
  Review separately declares whether an existing memory covers the **entire**
  incoming statement (`incoming_coverage: full|partial|unknown`). Except for
  literal text identity with matching applicability, partial, absent or unknown
  coverage keeps both records current. Only full coverage can retire incoming
  as redundant; different declared validity boundaries still prevent that merge.
  Partial support does not copy a compound statement into the older claim's
  independent-evidence count. Multiple partial relationships can coexist.
  Coverage concerns meaning and logical implication: rewording or spelling out
  something already implied is not new information.
- An inferred pattern requires at least three independent observations spanning
  two days before automatic replacement of another inference. A model cannot
  use observation count to silently retire an explicitly stated goal,
  preference or instruction. Shared memory provenance supplies the authority
  classification; model confidence alone does not authorize replacement.
  For a conflict between two inferred claims, the same evidence floor applies
  before clarification can unsettle the older claim. A weak incoming inference
  stays pending even when the model selects `ask_owner`; mature uncertain changes
  can still ask the owner. Independent new evidence resumes the pending review.
- Expiry follows a recorded validity interval supported by quoted source text.
  Inactivity or infrequent retrieval does not make a standing goal false.
- Model failure, invalid output or changed source revisions defers an uncertain
  change. Model-visible short references are bound to the offered record snapshots;
  unknown references fail rather than being repaired by similarity. Source IDs
  and every referenced revision are checked before apply.

Review decoding is the same for every provider. Identical repeated JSON members
are accepted only when their parsed values agree; conflicting duplicates at any
depth, unknown fields, wrong types, trailing content and responses over 64 KiB
are rejected before mutation. This does not choose between ambiguous values,
repair unknown source references, add a provider-specific API requirement or
make a second model call. Missing coverage is conservatively `unknown`.

The classifier is domain-independent. The backend enforces identity, authority,
scope and lifecycle constraints; semantic matching and usefulness of a question
remain model judgments measured by the behavioral evaluation.
Full semantic coverage is a model judgment, not an entailment proof; partial
overlap alone never retires a whole statement. Records already retired are not
automatically resurrected.

## Clarification and recall

Ambiguous contradictions use the existing durable `memory_clarification` HITL
surface. Choices retain the earlier memory, replace it with the new memory,
explain what applies in free text, or dismiss. These choices affect memory only;
they cannot approve or execute an external action.

The review label `ask_owner` explicitly requests missing information. The old
`clarify` spelling remains accepted for compatibility, with the same requirements.
A concrete, nonempty question of at most 600 characters is required before any
part of that review plan can apply. An empty question cannot produce a generic
follow-up prompt, including when the owner has already explained the conflict.
This distinguishes asking for information from information that resolves an
ambiguity. A blocked replacement retains its separately governed confirmation
fallback.

Questions carry their cited source versions and related claim context. A stale
answer cannot apply to newer evidence; unrelated writes do not invalidate it.
A free-text answer enters reconciliation with both cited
memories and must address those relationships before the conflict is resolved. If the
review calls a reaffirmation a duplicate, the complete owner answer becomes the
current version so its qualifications remain visible in recall. Earlier evidence
and applicable validity bounds survive the merge.
Decided conflicts remain in the private lifecycle journal so a restart or replay
does not repeat a decision. Dismissal retains uncertainty instead of silently
deciding which claim is true; independent new evidence may justify reconsideration.

Normal prompt/search candidates exclude pending, superseded, retracted and
expired revisions. Unresolved claims are explicitly labelled as uncertain.
Prompt text contains the claim and its applicability, not a serialized history
of earlier evidence. Stable record identities keep current and pending versions
from colliding in the index; admitted candidate sets resolve cross-tier
successor links for history inspection. Temperature/retention remains separate
from whether a memory is current. Attention shadow conditioning also excludes
pending/retired claims, and does not let unresolved claims suppress attention.

## Scheduling and bounds

`memory_lifecycle_review` is explicitly mapped to
`gpt6luna-responses-toolsany` in the live, repository and fallback router files.
It appears in Settings through the existing dynamic operation catalog. It is
background memory work with its own route, not a chat turn's response model.

One pass reviews one revision against at most 48 offered records. Current input
bounds are 4,000 characters for the incoming claim, 24,000 characters across
offered claims and 32,000 characters for the entire prompt including metadata.
The prompt contains a bounded evidence sample plus independently calculated
counts/date span. The provider deadline is 40 seconds. A durable reservation limits
review to six calls per scope per rolling hour; failures also spend a reservation
and a record waits at least 15 minutes before retry. Questions and expiry are
reconciled independently of model availability. Disabling the mapping leaves
unreviewed writes pending; it does not grant them current status by fallback.

App contribution envelopes retain their separately governed eligibility path.
Agent/goal operational tiers retain their declared schemas and merge rules;
promoting their information into owner memory enters this lifecycle. They are
not automatically converted from execution state into owner preferences.
The owner-edited profile note remains a separately approved instruction source,
already included by connection recall. Profile add/retract/restore decisions
have distinct identities and replaying an earlier approval cannot undo a later
retraction. Semantic memory review does not silently edit that note.

## Focused qualification

- `make test-memory-lifecycle` — deterministic lifecycle regressions only.
- `make build-memory-lifecycle-eval` builds the real-model fixture runner;
  `make test-memory-lifecycle-live-eval` (or **Evals → Lane →
  `test-memory-lifecycle-live-eval` → Run**) runs it. Both lanes are discovered
  from Makefile annotations.
- `make test-memory-lifecycle-evals-integration` checks the wrapper and
  Run/history/report API contracts without provider calls.

Frozen fixtures live in `scripts/fixtures/memory_lifecycle/cases.json`. The
runner uses production ingress, storage, review, candidate rendering and durable
user-request handling with synthetic scoped data, and records raw responses,
usage, expected/actual states, recall and response replay. Fixtures also cover a
persisted chat turn through the consolidation rule executor followed by a
correction and recall, downstream connection/delivery decisions, and free-text
clarification through the real owner request service with runtime
teardown/reopen to model restart. Recall is warmed before changes so checks
include index refresh. Background scheduler timing and browser/device input are
not covered.

Live lane defaults: all 19 journeys × 3 repeats; partition
`all|development|validation`, 1–3 repeats. CLI controls:
`MEMORY_LIFECYCLE_EVAL_PROFILES` (comma-separated, up to three),
`MEMORY_LIFECYCLE_EVAL_REPEATS`, `MEMORY_LIFECYCLE_EVAL_PARTITION`. Profile
choices come from the Settings runtime catalog; empty uses configured routes
including Settings overrides. A selection overrides only
`memory_lifecycle_review`, `memory_connection_review` and
`memory_user_promotion` in a private config snapshot; the live config is
unchanged. A profile comparison can change provider and generation settings,
not just model.

UI/API runs retain options and terminal status in Evals history; CLI runs keep
reports but create no task-history records. Each report is bound to its run ID
under `coverage/evals/memory-lifecycle/{live,deterministic}/runs/`, so later runs
cannot redirect an earlier link, and links raw evidence, logs, models, usage and
config/fixture/binary hashes. Missing evidence, build failures, timeouts and
failed journeys cannot pass. The evaluator's spend shows as **unknown** in the
task-ledger Cost tab; router-estimated cost is in its report.

Related: [memory index](memory-index.md), [memory connections](memory-connections.md),
[taste profile](taste-profile.md).
