# Learning Procedures

> **TL;DR — Magician's closed self-improvement loop.** Successful runs produce
> learning candidates → reflection judges usefulness → candidates become *draft*
> procedures (typed YAML records) → repeated successful retrieval makes them
> *active* → enough success and evidence promotes them through
> `LearningProcedureSkillPromotionBridge` into skill candidates, routed through
> the capability-evolution and eval backlogs before they reach the runtime
> catalog. Stale or harmful procedures become *deprecated*. The unit of learning
> is a typed procedure with provenance, evaluation gates and backlog routing,
> not a `SKILL.md` edited in place.

A sibling lane is the harness program-state loop; see
meta-harness close-the-loop.

Learning procedures are reusable ways of working learned from experience. They
are stored apart from semantic memory and from executable skills/tool packs, so
the system can capture "how to do this next time" without changing prompts,
wrappers or runtime catalogs.

## Relentless procedural qualification

Relentless adds an owner-authorized qualification lane.

- **Candidate.** A strict `memory_procedure` body has `title`, `workflow`,
  `decision_points`, `verification` and `failure_modes`, directly or inside one
  `procedure` wrapper. Task agents submit it with
  `propose_relentless operation:propose_guidance`, citing distinct terminal
  training run IDs from their own scoped tasks. The store keeps one idempotent
  review-required candidate; the owner notice grants no procedure or trial
  authority.
- **Trials.** The frozen policy runs paired baseline/candidate drafts on fresh
  exact-input held-out and separate canary suites through the existing
  controller, with independent review and finite separate budgets. The owner
  policy must reference every candidate training run; qualification trial
  provenance, declared training overlap and reuse of an activated suite are
  refused. The owner curates semantic independence; the code compares content
  hashes, not a similarity heuristic. Trials match the targets' native actor
  recipe, artifact kind and actor model/family, and native evidence records the
  sealed inbox's cumulative operator redirect count. Assisted trials settle
  their known cost but cannot qualify guidance. A definitive case failure stops
  the rest of the suite.
- **Adoption.** The procedure bridge projects passing guidance into a new
  deterministic policy-owned draft/active record, without merging existing
  procedures or changing the original candidate. Its
  `payload.explicit_admission_only` marker keeps it out of ambient prompt
  retrieval and the embedding index. Exact approved recurring draft tasks can
  adopt it under finite slot/expiry bounds; tools, DoD, budgets and outward
  authority are unchanged.
- **Monitoring.** Typed terminal monitoring applies declared quality and unknown
  rules. `monitor_nonacceptance_is_regression` counts any nonaccepted terminal
  status, cancellation included; unknown reviews never become passes.
  Regression, expiry or revocation stops future admission and projects
  archival.
- **Consistency.** Status projection uses the store lock plus an
  expected-version check. An owner-retired record is not reactivated
  automatically, and edited bodies are never overwritten by a retry. New roots
  revert to the reviewed baseline once unavailable guidance is disabled;
  interrupted guided reservations keep their exact snapshot; paused policies keep
  their target reservation until revoked; identical peer outcomes keep the first
  sealed proof. Revoked, expired or failed trials are durably cancelled, and
  recovery checks each trial against its frozen policy before continuing.
  Trial admissions pin the parent policy's absolute expiry in the core deadline.
  Owner evidence inspection reads the recorded content hash and validates origin,
  run scope and contract; it does not refresh a verdict.
- **Isolation.** Trials use internal task storage; the externally owned output
  path skips episode/reflection work, generic memory backfill refuses externally
  owned roots, and sealed evidence stays out of training lesson publication.
  Owner API/UI and restart recovery are source-connected and await batched
  qualification.

See [Relentless](relentless.md) and its current plan handoff.

## Storage and API

Procedure records live under the scoped V3 learning root:

```text
magician_data_v3/scopes/<principal>/<workspace>/
  learning/
    procedures/
      draft/<procedure_id>.yaml
      active/<procedure_id>.yaml
      deprecated/<procedure_id>.yaml
      archived/<procedure_id>.yaml
      decisions/<procedure_id>.jsonl
      index/          # LanceDB index (lancedb/, manifest.json, dirty.json)
```

Each record holds activation rules, workflow steps, decision points,
verification steps, failure modes, evidence refs, source task/chat/candidate
links, success/failure counters, version, timestamps and optional owner-agent
metadata. The directory gives the status; the decision log records every
transition with actor, reason and evidence. New records are always `draft`;
moving to `active`, `deprecated` or `archived` goes through the status endpoint
so a decision is logged. Duplicate status files for one id are refused on read
and surfaced by the API as a conflict, not a missing procedure.
Listing and reading reuse a process-wide parse of each YAML file, revalidated by
size + mtime and, when those change or are recent, a BLAKE3 content hash, so a
file is parsed once per revision; status/content writes re-read it fresh.
The post-turn "already reflected?" check (`has_completed_reflection_for_episode`)
reads the day-partitioned `learning/events/<date>.jsonl` logs through a
process-wide index of `learning_reflection_completed` (episode, boundary) pairs,
so each event line is deserialized once: a settled, unchanged stamp (size, mtime,
dev/inode) skips the read; an append parses only lines after the last indexed
newline, once the indexed prefix still matches its BLAKE3 hash; a shrunk,
replaced or rewritten log is rescanned from the start; and an unterminated final
line is never indexed until its newline lands. Malformed complete lines still
fail the check and are not cached.

REST surfaces:

- `GET /api/magician/v2/learning/procedures`
- `POST /api/magician/v2/learning/procedures`
- `GET /api/magician/v2/learning/procedures/{procedure_id}`
- `POST /api/magician/v2/learning/procedures/{procedure_id}/status`
- `POST /api/magician/v2/learning/procedures/{procedure_id}/skill-promotion`

Internal-data actions: `list_learning_procedures`, `read_learning_procedure`.

The `/memory` learning dashboard lists recent procedures and moves them between
`draft`, `active`, `deprecated` and `archived`.

## Extraction

Terminal episode persistence is post-answer work. Root executions share the
revision-keyed terminal auxiliary worker with continuation/media indexing; child
executions use their own idempotent episode worker. Both use the execution id as
the episode id, and startup reconciliation reschedules a missing root or child
episode, so a process exit after the answer cannot drop the run from learning.
Reflection and consolidation consume the persisted episode asynchronously and
never gate the answer.

- Explicit teaching actions (`make_reusable`, `this_was_useful`) create
  `memory_procedure` candidates with a `proposed_change.procedure` payload.
- Reflection prompt v1.10.1 sees existing draft/active procedure summaries and
  workflow-signature hints, and emits procedure candidates only for likely
  reusable workflows. Its input is curated valid JSON within a 46,000-character
  aggregate budget (episode, related episodes, candidates, procedures, extra
  context); arrays, strings, depth and width are bounded, and identity,
  workflow-detection and procedure-feedback keys survive reduction. The durable
  episode stays intact.
- The procedure bridge turns candidates into draft YAML and dedupes against
  draft/active procedures by source candidate id, explicit procedure id or
  workflow signature.
- Reviewed promotion merges improvements into existing draft/active procedures
  and activates drafts, making them retrieval-eligible.
- Deprecated and archived procedures are not dedupe targets; a new candidate
  with a retired id gets a suffixed id.
- The semantic memory bridge refuses `memory_procedure` candidates, so
  procedures do not get buried in memory tiers.

## Retrieval

- Direct task execution queries with goal, success criteria, active agent, task
  id and execution id, and injects the top active procedures as
  `{procedure_memory_section}`. Chat queries with the user message plus
  agent/session context and injects `{procedure_memory_block}`. Inner-loop tool
  runs inherit the rendered block without a second retrieval.
- Retrieval takes the top few records from a durable scoped LanceDB index over
  active procedure text, merged with direct activation matching (`use_when`,
  `example_goals`, title/summary), which is also the failure fallback. Canonical
  YAML stays authoritative.
- Indexed text changes and active-status transitions write a durable dirty
  marker and notify one background maintainer; feedback-only counter, timestamp
  and provenance updates schedule nothing. A retrieval-time fingerprint check
  catches external edits and missing indexes. The maintainer embeds only changed
  text, deletes rows no longer active, and rebuilds the table when its
  indexed-text schema or embedding contract (model, dimensions, logical
  context, physical `num_batch` ceiling, preprocessing version) changes, so
  incompatible vectors never mix.
- Prompt rendering never embeds documents or builds a temporary table. While
  maintenance is pending it returns lexical results; once current it spends at
  most one priority query embedding on hybrid search. Hybrid hits may select
  procedures with no lexical overlap, while owner-agent visibility, strong
  `avoid_when` matches and harmful/stale feedback still block or down-rank them.
- Each render reports `direct`, `lancedb_hybrid` or `direct_fallback`.
  `make benchmark-chat-context-retrieval` runs the same renderers without a
  generation call and writes `coverage/evals/chat-context-retrieval/latest.json`.
- Passes with active candidates append `learning_procedure_retrieval_rendered`
  (selected ids, scores, rationale, including zero-selected passes), so evals can
  tell useful selection from correct withholding.

## Feedback

- Post-run reflection collects the episode's
  `learning_procedure_retrieval_rendered` events and injects the selected
  procedures under `extra_context.procedure_feedback`. Prompt v1.10.1 asks for
  exactly one evidence-based judgement per retrieved procedure, even when no
  candidates are emitted.
- Low-risk bookkeeping is applied directly: `last_used_at`, evidence refs,
  source task/chat ids, bounded feedback entries and
  `learning_procedure_feedback_recorded` audit events.
- Success/failure counters move only on a targeted explicit correction or a
  structured judgement, never merely because a procedure was injected. A
  terminal failure status wins over contradictory success labels, and
  too-broad/too-narrow judgements count as negative pressure.
- Substantive workflow, activation, avoidance or failure-mode changes stay
  `memory_procedure` candidates with `existing_procedure_id`.
- Deprecation from feedback is owned by
  `procedure_feedback.deprecation_recommended`: the runtime creates one reviewed
  deprecation candidate and suppresses duplicate model-emitted ones.
- Repeated judged failures or explicit negative correction can move a procedure
  to `deprecated`, which stops retrieval (only `active` is injected).

## Procedure-to-skill promotion

- Active procedures stay procedural memory by default.
- With repeated successful evidence, low failure pressure, workflow steps and
  enough provenance, the post-use feedback bridge can create a review-gated
  `workflow_template` promotion candidate, routed through the skill/capability
  evolution backlog rather than editing skill files.
- A paired eval-backlog item under the same candidate id lets reviewers validate
  the guidance before approval, validation, application and promotion.
- The operator API (`POST /learning/procedures/{procedure_id}/skill-promotion`)
  always uses this review-gated route with both backlogs materialized, so a
  caller cannot create orphaned promotion records.
- The procedure stays active; its payload records bounded
  `phase_14_skill_promotions` entries linking to the candidate and backlog
  paths.

## Growth evaluation

`run_learning_growth_evaluation` scores procedure extraction precision,
retrieval relevance, misuse rate, helped/hurt signal, stale correction,
duplicate active procedures and promotion quality beside the memory and
skill-growth dimensions. Scenarios cover explicit teaching reuse, repeated
success converging to one active procedure, irrelevant withholding, user
correction, stale/harmful deprecation and evidence-gated graduation.

Scoring rules:

- Current procedure YAML is scored as current state even if created outside the
  event window; events, backlog items and run reports stay windowed.
- Relevance uses only feedback correlated to the retrieval event id or its
  selected ids; any correlated negative, harmful, stale, misleading, too-broad,
  too-narrow, irrelevant or deprecation-recommended judgement fails it.
- Correction and stale/harmful checks are per procedure id and need later
  correlated update/deprecation evidence (a correction candidate alone is not
  enough); one addressed target cannot hide another.
- Irrelevant withholding needs a zero-selected retrieval after irrelevant
  feedback, when such feedback exists.
- `too_broad`/`too_narrow` create correction pressure, not deprecation pressure,
  unless the feedback recommends deprecation.
- Promotion quality checks every expected candidate id and its capability and
  eval backlog records; an audit event alone cannot pass.
- Evidence comes from the same YAML, events, feedback and backlog records that
  drive runtime behavior. Missing evidence is `blocked`; only measured bad
  outcomes are `failed`.
