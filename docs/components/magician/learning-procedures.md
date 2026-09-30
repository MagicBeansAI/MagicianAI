# Learning Procedures

> **TL;DR — Magician's closed self-improvement loop.** Successful runs
> generate learning candidates → reflection prompts judge usefulness →
> candidates become *draft* procedures (typed YAML records) → repeated
> successful retrieval promotes them to *active* → enough success + evidence
> auto-promotes them through `LearningProcedureSkillPromotionBridge` into
> skill candidates, routed through capability-evolution + eval backlogs
> before reaching the runtime catalog. Stale or harmful procedures move to
> *deprecated*. The unit of learning is a typed procedure with provenance,
> evaluation gates, and capability-backlog routing rather than a free-form
> `SKILL.md` file edited in place.

A sibling self-improvement lane is the harness program-state loop; see
meta-harness close-the-loop.

Learning procedures are reusable ways of working learned from experience. They
are stored separately from semantic memory and from executable skills/tool
packs, so the system can capture "how to do this next time" without immediately
mutating prompts, wrappers, or runtime catalogs.

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

Each record contains activation rules, workflow steps, decision points,
verification steps, failure modes, evidence refs, source task/chat/candidate
links, success/failure counters, version, timestamps, and optional owner-agent
metadata. The current status is visible from the directory layout; the decision
log records every status transition with actor, reason, and evidence. New
procedure records are always created as `draft`; promotion to `active` and
movement to `deprecated` or `archived` must go through the status endpoint so a
decision entry is recorded. Store reads reject duplicate status files for the
same procedure id instead of guessing which status is authoritative; the API
surfaces that condition as a conflict rather than a missing procedure.

REST surfaces:

- `GET /api/magician/v2/learning/procedures`
- `POST /api/magician/v2/learning/procedures`
- `GET /api/magician/v2/learning/procedures/{procedure_id}`
- `POST /api/magician/v2/learning/procedures/{procedure_id}/status`
- `POST /api/magician/v2/learning/procedures/{procedure_id}/skill-promotion`

Internal-data actions:

- `list_learning_procedures`
- `read_learning_procedure`

The `/memory` learning dashboard lists recent procedures and lets operators
move them between `draft`, `active`, `deprecated`, and `archived`.

## Extraction

Terminal episode persistence is post-answer work. Root executions share the
revision-keyed terminal auxiliary worker with continuation/media indexing;
child executions use their own idempotent episode worker. Both use the
execution id as the deterministic native episode id. Startup reconciliation
checks that exact scoped episode path and reschedules a missing root or child
episode, so a process exit after answer readiness cannot drop the run from
the learning input stream. Reflection and consolidation remain
asynchronous consumers of the persisted episode and never gate the answer.

- explicit teaching actions such as `make_reusable` and `this_was_useful`
  create `memory_procedure` candidates with a `proposed_change.procedure`
  payload;
- post-run learning reflection prompt v1.10.0 receives existing draft/active
  procedure summaries plus workflow-signature hints and emits procedure
  candidates only when the workflow is likely reusable;
- reflection receives curated, valid-JSON sections with fixed aggregate
  budgets (46,000 characters across episode, related episodes, candidates,
  procedures, and extra context); nested arrays, strings, depth, and object
  width are bounded before dispatch while the durable episode remains intact.
  Identity, workflow-detection, and procedure-feedback keys are retained before
  caller-owned fields when a wide object must be reduced;
- the learning procedure bridge converts those candidates into draft procedure
  YAML records and dedupes against draft/active procedures by source candidate
  id, explicit procedure id, or workflow signature;
- reviewed candidate promotion merges concrete improvements into existing
  draft/active procedures and activates draft procedures so accepted procedure
  candidates become retrieval-eligible;
- deprecated and archived procedures are not reused as live dedupe targets; a
  new candidate with the same retired id is written under a suffixed id instead;
- the semantic memory bridge refuses `memory_procedure` candidates so
  reusable procedures do not get buried in user/agent memory tiers.

## Retrieval

- direct task execution builds a relevance query from goal, success criteria,
  active agent, task id, and execution id, then injects the top matching active
  procedures as `{procedure_memory_section}`;
- chat builds the query from the current user message plus agent/session context
  and injects `{procedure_memory_block}`;
- inner-loop tool runs inherit the same rendered block from the outer context,
  so browser and CLI inner loops can use procedure guidance without running a
  second retrieval step;
- retrieval is bounded to the top few records and queries a durable scoped
  LanceDB index over active procedure text, with direct activation matching
  (`use_when`, `example_goals`, title/summary) merged into the result and kept as
  the failure fallback. Canonical YAML remains authoritative. Successful indexed
  text changes and active-status membership transitions write a durable dirty
  marker and notify a single background maintainer; feedback-only counter,
  timestamp, and provenance updates do not schedule index work. Retrieval-time
  fingerprint comparison also catches external edits and missing indexes. The
  maintainer embeds only changed procedure text, deletes rows that are no longer
  active, and rebuilds the derived table when its indexed-text schema or full
  embedding contract (model, dimensions, logical context, physical `num_batch`
  ceiling, input preprocessing version) changes, so incompatible vectors are
  never mixed with current query vectors.
  Prompt rendering never embeds procedure documents or
  creates a temporary table: while maintenance is pending it returns lexical results
  immediately, and once current it spends at most one priority query embedding
  on hybrid search. Hybrid hits may select semantically relevant procedures
  without direct lexical overlap, while owner-agent visibility, strong
  `avoid_when` matches, and harmful/stale feedback still block or down-rank
  candidates before prompt injection;
- each render reports `direct`, `lancedb_hybrid`, or `direct_fallback` as its
  retrieval backend. `make benchmark-chat-context-retrieval` exercises the same
  renderers without a generation-model call and writes latency data to
  `coverage/evals/chat-context-retrieval/latest.json`;
- retrieval passes with active candidates append
  `learning_procedure_retrieval_rendered` with selected ids, scores, and
  rationale, including zero-selected passes, so prompt projections and growth
  evals can distinguish useful selection from correct withholding.

## Feedback

- post-run reflection collects matching `learning_procedure_retrieval_rendered`
  events for the episode and injects the selected procedures under
  `extra_context.procedure_feedback`;
- reflection prompt v1.10.0 asks for exactly one evidence-based judgement per
  retrieved procedure, even when no learning candidates are emitted, before
  counters move;
- low-risk bookkeeping is applied directly: `last_used_at`, evidence refs,
  source task/chat ids, bounded feedback payload entries, and
  `learning_procedure_feedback_recorded` audit events;
- success/failure counters change only from targeted explicit correction or a
  structured procedure judgement, not merely because a procedure was injected
  into a successful or failed run;
- terminal failure status/summary wins over contradictory success labels before
  useful judgements can increment success counters, and too-broad/too-narrow
  judgements count as negative procedure pressure;
- substantive workflow/activation/avoidance/failure-mode changes remain
  `memory_procedure` candidates with `existing_procedure_id`;
- deprecation from retrieved-procedure feedback is owned by top-level
  `procedure_feedback.deprecation_recommended`; the runtime creates one reviewed
  deprecation candidate and suppresses duplicate model-emitted deprecation
  candidates for the same procedure;
- repeated judged failures or explicit negative user correction can move a
  draft/active procedure to `deprecated`, which stops future retrieval because
  only `active` procedures are injected.

## Procedure-to-skill promotion

- active procedures remain procedural memory by default;
- once an active procedure has repeated successful evidence, low failure
  pressure, workflow steps, and enough provenance, the post-use feedback bridge
  can create a review-gated `workflow_template` promotion candidate;
- the promotion candidate is routed through the existing skill/capability
  evolution backlog instead of editing skill files directly;
- a paired eval-backlog item is written under the same promotion candidate id
  so reviewers can validate the promoted guidance before proposal approval,
  validation, application, and promotion;
- the public operator API always uses the review-gated route
  (`POST /learning/procedures/{procedure_id}/skill-promotion`), with capability
  and eval backlog materialization enabled, so caller payloads cannot create
  orphaned promotion records;
- the procedure stays active, and its payload records bounded
  `phase_14_skill_promotions` entries linking back to the promotion candidate
  and the capability/eval backlog paths.

## Growth evaluation

`run_learning_growth_evaluation` scores procedure extraction precision,
retrieval relevance, misuse rate, helped/hurt outcome signal, stale correction,
duplicate active procedures, and procedure-to-skill promotion quality beside the
memory and skill-growth dimensions. Scenarios cover explicit workflow teaching
reuse, repeated success converging to one active procedure, irrelevant
withholding, user correction updates, stale/harmful deprecation, and
evidence-gated graduation into skill evolution.

Scoring rules:

- current procedure YAML is evaluated as current state even if created outside
  the event window; events, backlog items and run reports stay windowed;
- retrieval relevance uses only feedback correlated to the retrieval event id or
  its selected procedure ids; any correlated negative, harmful, stale,
  misleading, too-broad, too-narrow, irrelevant or deprecation-recommended
  judgement fails relevance;
- correction and stale/harmful checks are per procedure id and require later
  correlated update/deprecation evidence (a correction candidate alone does not
  satisfy a user-correction scenario); one addressed target cannot hide another;
- irrelevant withholding requires a zero-selected retrieval after irrelevant
  feedback when such feedback exists;
- `too_broad`/`too_narrow` create correction pressure, not deprecation
  pressure, unless the feedback explicitly recommends deprecation;
- promotion quality verifies every expected candidate id plus its capability and
  eval backlog records, so an audit event cannot pass by itself;
- evidence comes from the same YAML, events, feedback and backlog records that
  drive runtime behavior; missing evidence is `blocked`, only measured bad
  outcomes are `failed`.
