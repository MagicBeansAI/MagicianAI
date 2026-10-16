# Harness Supplemental Profile

Owner: harness lane.

## What it is

The one thing a harness may propose changing about itself: versioned
supplemental **operating guidance**, held per program, revised only through an
evidence-gated and owner-approved transaction, and reversible exactly.

It is not the program document. That doc, the agent's authority, tool grants,
trust policies, schedules and approval rules are beyond this path's reach: a
harness can improve *how it works*, never *what it is permitted to do*.

## Why the guard is an allowlist

A revision may address only these sections:

| Section | What it holds |
|---|---|
| `operating_notes` | how this program's cycles should be run |
| `approved_procedures` | references to already-approved procedures |
| `skill_references` | references to already-granted skills |
| `subagent_role_guidance` | how a subagent is briefed, not which exist |

Anything else is refused by construction; a denylist would go wrong the first
time a new authority-bearing field was not added to it. The tests assert the
specific forbidden names (`persona`, `tools`, `trust_level`, `trust_policies`,
`approval_rules`, `schedule`, `program`, `authority`) *and* that an invented
future field is refused. Because references point only at already-approved
procedures and already-granted skills, no allowed section can widen the
agent's permissions.

## What a proposal must carry

`apply()` refuses a proposal missing any of: episode evidence, an evaluation
id, expected benefit, confidence, risk, rollback plan, a targeted evaluation
case, or a measurable acceptance condition, each with its own typed refusal,
so a half-answered proposal never reaches an owner. Approval is passed
separately from the proposal, so **there is no path from a reflection to live
guidance** without it; this is enforced structurally, not by convention.

## Revert restores; it does not reconstruct

Every revision retains the exact prior text of each section it touched,
including the fact that a section *did not exist*. Reverting therefore
restores byte-for-byte, and reverting a revision that created a section
removes it rather than leaving an empty heading. A reverted revision is marked,
never deleted, so failed evaluations, rejected approvals and reverted changes
stay auditable with their evidence.

A proposal written against text that has since moved is refused, so a later
cycle cannot silently clobber an earlier one on the same section.

## The reader is what makes it real

The profile renders into the harness cycle prompt in
`append_harness_program_context`, positioned after the program document and
before runtime state: it supplements the program and can never replace it. An
empty profile contributes nothing rather than an empty heading.

Guidance whose scope or program path does not match the cycle is refused
however it reached disk. A load failure degrades the cycle rather than blocking
it.

## The route from a reflection to live guidance

Three separate calls on `learning::LearningHarnessProfileBridge`, on purpose.
The other learning bridges auto-apply a low-risk candidate; this one has no
auto-apply arm to reach.

1. **Stage.** Reflection's dispatcher hands a `harness_profile_revision`
   candidate to `route_candidate`, which parses `proposed_change` as a
   `ProfileRevisionProposal`, validates it against the allowlist, and
   stages it for review (`harness_profile_candidate_staged`, state →
   Triaged). A proposal that cannot answer an owner's questions, or does not
   name its program in `proposed_target`, is recorded as
   `harness_profile_candidate_invalid` and goes nowhere. Nothing is applied
   here, whatever the candidate's state or risk level.
2. **Evaluate.** `record_evaluation` is the controlled run: it writes a
   `HarnessProfileEvaluationRecord` — the proposal's own evaluation case and
   acceptance condition, what the runner observed, and whether it passed —
   at `programs/supplemental/evaluations/<candidate_id>.json`, and emits
   `harness_profile_evaluation_recorded`. A failing run is recorded like a
   passing one and blocks the apply.
3. **Apply.** `apply_approved_candidate` is the only writer. It needs the
   candidate in the lane's `Approved` state, an owner's name, and a *passing*
   evaluation record it looks up itself, and stamps the proposal's
   `evaluation_id` from that record before calling `apply`, since
   model-authored text can name an evaluation that never ran. The history
   therefore carries the id of the run that happened. Emits
   `harness_profile_revision_applied` and moves the candidate to Promoted.
   Applying twice is refused by the stale-`before` guard. The owner's apply is
   the promote transition on `POST /api/magician/v2/learning/candidates/{id}/state`.

Persistence is `harness::SupplementalProfileStore`: one file per program at
`programs/supplemental/<program path, `/` and `\` replaced by `_`>.json`, atomic writes, and a per-path
lock around apply so two cycles cannot interleave a read-modify-write. The
path is spelled once, in `ArtifactV2Workspace::programs_supplemental_profile_path`,
and both the cycle-prompt reader and the store call it, so writer and reader
cannot drift apart.

The harness's own `read_program_state` tool also returns
`supplemental_guidance` (revision and rendered block) beside the runtime
state, so a cycle reading its program through the tool sees the same
guidance the chat-driven prompt does.

## Key files

- `harness/supplemental_profile.rs` — the artifact, the allowlist guard, the
  proposal contract, apply/revert.
- `harness/supplemental_profile_store.rs` — persistence and the locked
  apply-revision transaction.
- `learning/harness_profile_bridge.rs` — stage, evaluate, apply; the
  evaluation record.
- `learning/reflection.rs` — the dispatch that hands the candidate to the
  bridge.
- `magician-api/src/web_api.rs` — `load_harness_supplemental_profile` and the prompt block
  in `append_harness_program_context`.
- `execution/harness_provider.rs` — `supplemental_guidance` on
  `read_program_state`.
- `learning/types.rs` — `LearningCandidateType::HarnessProfileRevision` and
  `is_harness_profile_candidate()`, the one candidate that must never
  auto-apply.

## Evaluation cadence

Nothing drives the controlled run automatically. A runner (a harness cycle
exercising the proposal's evaluation case against its acceptance condition)
posts its result to
`POST /api/magician/v2/learning/candidates/{id}/harness-profile-evaluation`,
which calls `record_evaluation`; no scheduled job calls that route.

The full boundary plan is [Bounded Research and Harness
Adaptation](../../archive/plans/2026-08-09-bounded-research-and-harness-adaptation-plan.md).
