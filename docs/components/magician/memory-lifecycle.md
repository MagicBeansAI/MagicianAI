# Memory consolidation and lifecycle

Owner-memory writes keep revisions, reconcile related claims and ask about
uncertain conflicts through one shared, provider-independent reviewer.

## Shared structured user-memory path

`save_preference`, user-scope `update_memory_tier`, the web preference endpoint,
learned user promotions and remembered connection clarifications share one
ingress. User extraction leaves conflict questions to the reviewer. Writes keep
revisions rather than replacing an array element because its key matches. A
preference clear retracts matching current entries but keeps their history. The
web endpoint also keeps its episode as provenance; an episode alone is not a
preference write.

A saved revision starts as `memory_lifecycle: pending_review`. Tools confirm
durable storage and schedule review without waiting past their deadline; the
scoped background worker resumes pending work. The reviewer classifies facts,
preferences, goals, instructions and observations, and proposes duplicate,
reinforce, coexist, supersede or `ask_owner` relationships, across keys and
user tiers. It never rewrites the owner's claim into model prose.

Each revision has a stable record ID, an ingress identity and source evidence.
Independent events can reinforce a claim; repeated copies of one root event do
not advance its date or evidence count, and an uncited batch counts as one
source. User promotions ground evidence in real input IDs and literal quotes,
with server dates and root execution identities. Extraction asks for a
self-contained assertion and an exact supporting excerpt; invented excerpts
cannot become evidence. Uncited attribution stores source identity and date,
not event payload. Tool writes discard caller-supplied lifecycle IDs and
evidence counters. Extraction uses JSON Schema and validates records before
persisting. Recognizable legacy descriptive-schema copies are quarantined in
the private audit journal and excluded from current memory; legacy primitive
collections are normalized lazily without losing text.

Every `knowledge.json` write through `persist_user_knowledge`, and every
`forget_memory` deletion, bumps `memory/users/intention_generation.json` under
the same flock. `forget_memory` holds the file's exclusive lock across each
tier-field read-modify-write, so a concurrent save is not lost. Preference
alignment compares this generation to detect owner knowledge that changed after
an assessment. The record stores a BLAKE3 fingerprint of the knowledge bytes,
so a failed bump does not fail the write; it reads back as stale. See
[preference alignment](preference-alignment.md#sources-and-generation-p1).

## Change policy

- **Corrections.** A clear explicit correction can supersede an older claim
  about the same subject, aspect and context. Project and engagement boundaries
  hold even if the model calls two items equivalent.
- **Coexistence.** Different people, attributes, projects or conditions can
  coexist. A temporary exception does not replace a standing preference.
- **Coverage.** Duplicate and reinforce describe a relationship, not
  redundancy. Review declares separately whether an existing memory covers the
  **entire** incoming statement (`incoming_coverage: full|partial|unknown`).
  Except for identical text with matching applicability, only full coverage
  retires the incoming record, and different validity boundaries still block
  the merge. Partial support does not add to the older claim's evidence count.
  Rewording or spelling out something already implied is not new information.
- **Inferences.** An inferred pattern needs at least three independent
  observations over two days before it can replace another inference, or
  unsettle one through clarification. Observation count can never silently
  retire an explicit goal, preference or instruction; authority comes from
  provenance, not model confidence. A weak incoming inference stays pending
  even if the model picks `ask_owner`; new independent evidence resumes it.
- **Expiry** follows a recorded validity interval backed by quoted source text.
  Inactivity or rare retrieval does not make a standing goal false.
- **Failure.** Model failure, invalid output or changed source revisions defer
  the change. Short references are bound to the offered record snapshots;
  unknown references fail rather than being matched by similarity. Source IDs
  and referenced revisions are checked before apply.

Review decoding is the same for every provider. Repeated identical JSON members
pass only if their parsed values agree; conflicting duplicates at any depth,
unknown fields, wrong types, trailing content and responses over 64 KiB are
rejected before mutation. Nothing is repaired or retried with a second call.
Missing coverage is `unknown`.

The classifier is domain-independent. The backend enforces identity, authority,
scope and lifecycle; semantic matching and question usefulness are model
judgments measured by the behavioral eval. Full coverage is a judgment, not an
entailment proof. Retired records are not resurrected automatically.

## Clarification and recall

Ambiguous contradictions use the durable `memory_clarification` HITL surface.
The owner can keep the earlier memory, replace it, explain in free text, or
dismiss. These choices affect memory only; they cannot approve or run an
external action.

The review label `ask_owner` requests missing information (the old `clarify`
spelling is still accepted). A concrete, nonempty question of at most 600
characters is required before any part of the plan applies, so an empty
question never yields a generic prompt. A blocked replacement keeps its
separately governed confirmation fallback.

Questions carry their cited source versions and related claims. A stale answer
cannot apply to newer evidence; unrelated writes do not invalidate it. A
free-text answer is reconciled with both cited memories and must address them
before the conflict resolves. If review calls a reaffirmation a duplicate, the
full owner answer becomes the current version, keeping its qualifications;
earlier evidence and validity bounds survive. Decided conflicts stay in the
private lifecycle journal so a restart or replay does not repeat them.
Dismissal keeps the uncertainty; new evidence may reopen it.

Prompt and search candidates exclude pending, superseded, retracted and expired
revisions; unresolved claims are labelled uncertain. Prompt text holds the
claim and its applicability, not its evidence history. Stable record IDs keep
current and pending versions apart in the index, and admitted candidates
resolve cross-tier successor links for history. Temperature and retention are
separate from currency. Attention shadow conditioning also excludes pending and
retired claims, and unresolved claims cannot suppress attention.

## Scheduling and bounds

`memory_lifecycle_review` is mapped to `gpt6luna-responses-toolsany` in the
repo-root `llm-router.yaml` seed and appears in Settings through the dynamic
operation catalog. It is background work with its own route, not a
chat turn's model.

| Bound | Value |
| --- | --- |
| Records offered per pass | 48 |
| Incoming claim | 4,000 characters |
| All offered claims | 24,000 characters |
| Whole prompt with metadata | 32,000 characters |
| Provider deadline | 40 seconds |
| Calls per scope | 6 per rolling hour (durable reservation; failures count) |
| Retry wait per record | at least 15 minutes |

The prompt holds a bounded evidence sample plus separately computed counts and
date span. Questions and expiry reconcile without the model. Unmapping the
operation leaves writes pending; it never makes them current by fallback.

App contribution envelopes keep their own eligibility path. Agent and goal
operational tiers keep their schemas and merge rules; promoting their content
into owner memory enters this lifecycle, but execution state is never turned
into owner preferences automatically. The owner-edited profile note stays a
separately approved instruction source, already in connection recall. Profile
add, retract and restore decisions have distinct identities, so replaying an
old approval cannot undo a later retraction. Review never edits that note.

## Focused qualification

- `make test-memory-lifecycle`: deterministic lifecycle regressions.
- `make build-memory-lifecycle-eval` builds the real-model fixture runner;
  `make test-memory-lifecycle-live-eval` (or **Evals → Lane →
  `test-memory-lifecycle-live-eval` → Run**) runs it. Both lanes are discovered
  from Makefile annotations.
- `make test-memory-lifecycle-evals-integration` checks the wrapper and
  Run/history/report API contracts without provider calls.

Fixtures live in `scripts/fixtures/memory_lifecycle/cases.json`. The runner
uses production ingress, storage, review, rendering and owner-request handling
on synthetic scoped data, and records raw responses, usage, expected and actual
states, recall and replay. Fixtures cover a chat turn through consolidation
then a correction and recall, downstream connection and delivery decisions, and
free-text clarification through the real owner request service with a runtime
teardown and reopen. Recall is warmed first so checks include index refresh.
Scheduler timing and browser or device input are not covered.

The live lane defaults to all 19 journeys (17 cases in `cases.json` plus the
capture and clarification journeys) × 3 repeats; partition
`all|development|validation`, 1–3 repeats. CLI controls:
`MEMORY_LIFECYCLE_EVAL_PROFILES` (comma-separated, up to three),
`MEMORY_LIFECYCLE_EVAL_REPEATS`, `MEMORY_LIFECYCLE_EVAL_PARTITION`. Profiles
come from the Settings runtime catalog; empty uses configured routes. A
selection overrides only `memory_lifecycle_review`, `memory_connection_review`
and `memory_user_promotion` in a private config snapshot; the live config is
untouched. A profile can change provider and generation settings, not just
model.

UI and API runs keep options and status in Evals history; CLI runs keep reports
but no task history. Each report is bound to its run ID under
`coverage/evals/memory-lifecycle/{live,deterministic}/runs/` and links raw
evidence, logs, models, usage and config, fixture and binary hashes. Missing
evidence, build failures, timeouts and failed journeys cannot pass. Spend shows
as **unknown** in the task-ledger Cost tab; router-estimated cost is in the
report.

Related: [memory index](memory-index.md), [memory connections](memory-connections.md),
[taste profile](taste-profile.md).
