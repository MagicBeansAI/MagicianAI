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

## Skill Evolution reference (Phases 3–15)

Reviewable user-memory learning candidates now project into the scoped feed as `learning_candidate` cards. The feed reconciles from `LearningStore` directly rather than only learning events, and Desk actions confirm, edit-and-confirm, or archive through the existing learning-memory bridge and candidate state machine. Desk feed admission is being tightened around durable user-facing cards — learnings, deliveries/routines, failed task outcomes, and completed task outcomes with summaries/artifacts/outcomes — while urgent approvals, user-input escalations, and running/paused task controls stay in Attention Bar. Published surfaces now emit `data_delivery` feed cards from `published_surface.changed` events, and scheduled task terminal executions emit `routine.result_published` into stable routine delivery cards; older active surfaces can be backfilled explicitly with `scripts/backfill_published_surface_feed_deliveries.py`. Generic assistant chat messages stay in chat instead of being duplicated as feed rows; legacy `agent_message` feed rows are removed by the explicit one-time `scripts/purge_legacy_agent_message_feed.py` maintenance script rather than by hidden feed-read cleanup.

Agent-growth learning now includes the Phase 3 memory bridge, Phase 4 skill/tool-pack evolution review chain, Phase 5 reusable skill/workflow candidate routing, Phase 6 structured eval-worker evidence, Phase 7 OPC program-state routing, Phase 8 explicit user teaching/correction, Phase 9 growth evaluation rollups, Phase 10 procedure storage, Phase 11 reusable procedure extraction, Phase 12 active procedure retrieval/prompt injection, Phase 13 procedure feedback/update/deprecation, Phase 14 procedure-to-skill promotion routing, and Phase 15 procedure quality evaluation. Skill Evolution evidence now also records compiled skill/tool calls, primitive CLI-template tools, browser primitives, direct native file/http/shell actions, and chat runtime tools under `learning/skill_invocations/dt=YYYY-MM-DD/<invocation_id>.json`, appending compact `skill_invocation_*` learning events with redacted input-shape fingerprints, structural result summaries, failure classes, and task/execution/chat provenance. Repeated failures cluster under `learning/skill_invocation_failure_clusters/` and high-confidence clusters route one review-gated Skill Evolution backlog item through the existing bridge. Skill Evolution backlog routing now fingerprints equivalent candidates, merges open duplicates into the canonical backlog item, supersedes duplicate source candidates with decision-log provenance, ranks queue actions, and exposes owner hints in the `/memory` panel. The existing API/storage substrate is still named `capability_evolution`, but the current runtime surface is scoped `skills/<skill>/...`, not a separate legacy `capabilities/` tree. Terminal task episodes, agent-cycle episodes, and meaningful chat turns can trigger a best-effort, schema-bound `learning_reflection` operation that writes audit events and learning-candidate proposals. Direct user instructions such as `remember`, `forget`, `correct`, `make_reusable`, `improve_tool`, `never_do_this`, `this_was_useful`, and `this_was_wrong` now flow through `POST /api/magician/v2/learning/teaching` or the chat-runtime `record_teaching_feedback` tool, producing the same learning events, candidates, and bridge routing instead of remaining only as transcript text. Reflection-created memory fact/preference candidates are routed immediately; only explicit low-risk user memory requests/corrections can auto-promote into `memory/users/knowledge.json`, while inferred, risky, agent-level, or malformed candidates stay review-gated. Reflection-created `memory_procedure` candidates route to draft procedure YAML under `learning/procedures/draft/` after dedupe against existing draft/active procedures, while reviewed promotion merges updates, activates draft procedures, and can apply reviewed deprecation proposals; deprecated/archived procedures are not reused as live dedupe targets. Reusable procedures therefore do not get buried in semantic memory or mutate skills directly. Active procedures are retrieved beside memory and injected as bounded task/chat/inner-loop prompt guidance with rationale events, including zero-selected audit events when candidates were withheld; retrieved procedures now receive post-use feedback that updates `last_used_at`, evidence refs, and source links; success/failure counters move only from structured procedure judgements or targeted correction, and repeated judged failures or explicit negative user correction can deprecate them so they stop being injected. Repeatedly successful active procedures can now create review-gated `workflow_template` skill-promotion candidates and paired `evaluation_case` backlog entries through the existing capability/eval bridges; source procedures remain active and keep provenance to promotion candidates. Growth-evaluation reports now distinguish memory recall from procedure reuse by scoring procedure extraction precision, retrieval relevance, misuse, helped/hurt signal, stale correction, duplicate active procedures, and procedure-to-skill promotion quality. Reflection-created `evaluation_case` candidates are written to `learning/evaluations/backlog/<candidate_id>.json` with optional meta-harness diagnosis fields; eval-worker run reports live at `learning/evaluations/runs/<candidate_id>/<run_id>.json`; Phase 9 growth-evaluation reports live at `learning/evaluations/growth_runs/<run_id>.json`; reflection-created `program_state_update` candidates can update `programs/state/*.json` only when low-risk and review-free or explicitly promoted by a reviewer; reflection-created `capability_update`, `tool_schema_update`, and `tool_wrapper_fix` candidates are written to `capability_evolution/backlog/<candidate_id>.json`; and reflection-created `skill_update` / `workflow_template` candidates use the same backlog when the proposal is about skill artifacts rather than a draft procedure. Reviewable skill/tool-pack fix/eval plans live separately at `capability_evolution/proposals/<candidate_id>.json`, proposal-generated eval cases live at `learning/evaluations/backlog/<candidate_id>.json`, validation evidence lives at `capability_evolution/validations/<candidate_id>/<validation_id>.json`, implementation bundles live at `capability_evolution/implementations/<candidate_id>/<implementation_id>.json`, dry-run/applied file-change records live at `capability_evolution/applications/<candidate_id>/<application_id>.json`, bounded steward run reports live at `capability_evolution/steward_runs/<run_id>.json`, and promotion evidence is appended to `capability_evolution/promotion_audit.jsonl`. Proposal drafting and review are separate surfaces: the draft/upsert path cannot set terminal statuses, while `/api/magician/v2/learning/capability-evolution/proposals/{candidate_id}/decision` records approved/rejected/superseded/archived reviewer decisions with provenance and synchronizes proposal, backlog, and candidate state; scoped skill approvals require non-empty `eval_plan` and `promotion_gate` fields, and high/critical-risk approvals also require explicit review evidence. The proposal drafter can seed queued backlog items with reviewable plans and full-file skill-guidance patches, including AgentSkills frontmatter for brand-new reusable workflow skills; `/evaluation/generate` can materialize a proposal `eval_plan` for meta-harness review, and the validation runner can ensure that eval backlog item exists before executing approved proposal commands under the scoped workspace root with regression evidence when requested. `POST /api/magician/v2/learning/capability-evolution/steward/run` runs one conservative steward cycle that drafts safe proposals, generates evals, runs only approved low-risk allowlisted local validations, prepares reviewable implementation bundles and scoped dry-run application records, and reports review/apply/promotion gates as attention-required actions without approving, applying, or promoting by itself. Approved proposals can then receive pass/fail/blocked validation reports; passing validation now requires material evidence and moves the backlog to `validated` and the candidate to `evaluated` without applying files. Implementation bundles bind approved proposals and passed validations to concrete file/patch payloads for inspection; application records can dry-run or apply those full-file patches to the scoped, system, or source skill surface through an explicit `target_surface`, and applied scoped/system skill records include runtime catalog visibility metadata so operators can see post-apply skill discovery, and promotion refuses applied records whose post-apply discovery failed. Promotion recording may reference either the bundle or an applied application record, but any promotion whose proposal patches/proposed files, implementation patches/applied files, or promotion applied files target `skills/` paths must reference an applied application record covering at least one of those skill targets before the backlog/candidate can be marked implemented; proposals whose `promotion_gate` requires regression checks also need validation evidence that regression ran. These substrates are exposed through the learning API, `internal_data`, and the `/memory` skill-evolution panel, which now includes ranked backlog recommendations, priority reasons, owner hints, explicit teaching, recent-candidate review, procedure review, procedure-to-skill promotion, and the Phase 9 growth eval runner; proposal/validation/implementation records remain inert until the explicit apply endpoint is called.

Phase 4 Skill Evolution drafting adds `/api/magician/v2/learning/capability-evolution/proposals/{candidate_id}/implementation/draft` for approved low-risk scoped `SKILL.md` guidance, `tool_schema.yaml`, and wrapper script proposals. Proposal drafting scaffolds schema/wrapper full-replacement patch metadata from observed failure evidence plus local artifact metadata with `metadata.reviewed_patch_content=false`; implementation drafting refuses those artifacts until a reviewer confirms `metadata.new_content` and sets the marker to `true`. It creates reviewable implementation bundles from validated full-replacement proposal patches, stamps exact target-file, artifact-kind, rollback, docs/changelog/version, and review-before-apply metadata, and leaves source-skill and non-low-risk implementation bundles on the manual review path.

Phase 5 Skill Evolution validation drafting now seeds `validation_plan.phase5_generated` with target-specific command suggestions for scoped skill changes: AgentSkills frontmatter validation for `SKILL.md`, scoped `tool_schema.yaml` shape validation through `scripts/validate_skill_artifact.py`, Python wrapper syntax checks, wrapper `--help` regression smoke commands, scoped skill-local Python unittest discovery or shell test scripts when `skills/<skill>/tests` already exists, and plan-only `fixture_cases` with schema-shaped sample inputs when real failure evidence exists. The fixture validator can confirm the wrapper/schema pair can produce a redacted regression fixture without running arbitrary skill code; validation reports and evaluation run payloads now record fixture cases, command-result coverage, and covered observed failure classes. The existing validation runner and steward still apply regression filtering and local command allowlists before executing anything.

Phase 6 Skill Evolution application records now add `payload.phase6_rollback_snapshot` with per-file restore/remove actions, previous/new content hashes, byte sizes, and restorable-file counts while keeping full bodies in `changed_files`; runtime catalog refresh payloads also include `changed_skill_discovery` so operators can see whether each changed skill was rediscovered, which manifest path won, and which changed skills were missing before promotion. Phase 6 rollback recommendations are recorded under `capability_evolution/rollback_recommendations/<candidate_id>/<recommendation_id>.json` when catalog refresh fails after apply or a later validation failure/blockage can be tied back to an applied change, project into Attention as Skill Evolution escalation rows while `recommended`, and can be reviewed, dismissed, or superseded from the Skill Evolution dashboard.

Phase 7 Skill Evolution post-promotion monitors are recorded under `capability_evolution/post_promotion_monitors/<promotion_id>.json` when promotions are recorded or rerun from `POST /api/magician/v2/learning/capability-evolution/promotions/{promotion_id}/monitor`; monitors compare bounded skill invocation evidence before and after promotion, list success-rate and failure-class deltas, surface in the Skill Evolution dashboard, create rollback recommendations or review-gated follow-up `skill_update` candidates when regressions are detected, project unresolved monitor-only regressions into Attention, and feed `post_promotion_stable_promotions` plus `post_promotion_regression_rate` dimensions into growth evaluation.
