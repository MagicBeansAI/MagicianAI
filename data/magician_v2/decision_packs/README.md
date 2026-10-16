# Decision packs

Versioned, vendor-neutral question packs for `magician-decision`.
Layout: `<pack_id>/<version>.json`.

## `tool_action_judge`

The shared action rail uses one pack for every authorized tool: `next_action`
selects a complete call, then `evidence_sufficient` and `action_applicable`
review that exact call. Decision Engine constructs the candidates from schemas,
current evidence and validated continuation plans. Missing content or inadequate
confidence requests the selected generative planner.

Thresholds belong to `decision-engine.yaml`, keyed per model. Packs contain
question instructions and contrastive criteria, with no provider-specific fields
or execution authority. Browser, Android and CUA have no separate judge packs.

See [the living contract](../../../docs/components/magician/structured-decision.md).

## `channel_classify`

Thirteen closed thread judgments; lane, follow-up kind, action owner, urgency and repeat status gate classification. The caller derives text from distill and retains its normal LLM path for unresolved threads.
Version 1.1.0 clarifies bulk-message boundaries and task ownership; its receipts and qualification cannot inherit version 1.0.0 evidence.

## `plan_parameter_priority`

Planner parameter priority, one item per parameter of a plan. 1.1.0 asks priority only and names the parameter kind (`tool_selection`, `slot`, `consent`); the host derives ask timing from priority. The planner keeps its LLM classifier as the shadow reference and for unresolved parameters.

## `query_intent`

The planner's per-turn intent, task clarity and complexity bucket. The unified query-analysis LLM stays the shadow reference; when gated, only these three fields come from the engine and the LLM still supplies extraction.

## `answer_interpretation`

The type of a user's reply to a planner clarification question (`answer_type`: direct answer, correction, clarification, rejection, multi-value, ambiguous) and whether it changes the plan (`requires_replan`). The answer-interpretation LLM stays the shadow reference; when gated, a correction still goes to the LLM for its corrected value.

## `progress_review`

The adversarial progress reviewer's next step for a task-backed run stuck in read-only steps (`decision`: continue, converge, redirect, escalate). The reviewer LLM stays the shadow reference; when gated, `escalate` and `converge` need no LLM text, while `continue` and `redirect` still go to the LLM for their steer.

## `action_alignment`

Version 1.3.0 (current) adds `owner_decides` (does the current owner turn make the one decision this intention reserves for the owner: agreeing to a request, or what a reply says; never for limits on time, money, health, diet, safety or a kind of act). Version 1.2.0 adds `muted` (does another of the owner's offered statements, in `context.intentions` when more than one is offered, say not to raise this one for this act); its other wording is 1.1.0's. Version 1.1.0 asks once per offered intention: how the proposed outward or commitment act relates to it (`relation`: not_applicable, consistent, conflict, unclear, acknowledged_exception), whether the current owner turn cancels the act (`owner_cancels`), whether that turn itself chose the detail that matters (`owner_specified`), whether the intention is a firm limit (`firm_limit`), and whether it is about the person the act is for (`same_subject`). The host composes the whole `alignment_decision.v2` from those answers, the intention's evidence force, lifecycle and kind, and the owner's policy (`magician-alignment` `composer.rs`); the model never picks the intervention. Version 1.0.0 (three whole-decision heads with no intention citations) is superseded. The generative reference `action_alignment_judge` is the host's fallback when the typed path is unresolved; a failed or uncertain judgment never falls back to a permissive answer. See [preference alignment](../../../docs/components/magician/preference-alignment.md).

## `recipe_match`

Which of up to three candidate Task Recipes, or `none`, has the same task shape as a new task. Candidates are injected per item (`recipe` has no fixed options); the item is the matcher's shape-only payload. The recipe_match LLM stays the shadow reference; when gated, only a confident `none` skips it, since a match still needs the LLM's input extraction.

## `claim_support`

Whether one claim of a tool-backed chat reply is backed by that turn's tool results (`verdict`: supported, contradicted, absent, not a claim) and whether it is part of the answer or a side remark (`core`). One item per claim, with its retrieved evidence lines; the owner's request is the shared context. Figures are checked in code first. The engine replaces the LLM claim judge on this path, so it runs gated only; an unanswered claim keeps the figures-only rule.
