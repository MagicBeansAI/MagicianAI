# Behavior operation recipes

**Contract:** `AppManifestBehaviorStep`, `AppManifestBehaviorStepGuard`
**Validation:** `validate_behavior_steps` (`apps/manifest.rs`)
**Seal:** `app_behavior_steps_digest` → `AppBehaviorGrant::steps_digest`

## The problem this closes

`behavior.operations` is an **unordered allow-set**: which LLM operations a
behavior may use, never in what order or under what conditions. It must never
be read as a recipe, "not by position and not by prose". E.g.
`[engagement_gate, compose_post]` is nearly a sequence, but `compose_post` must
run only when the gate says `engage`, which no allow-set can express. Steps add
the ordered, guarded aggregate.

The shipped Town Square package uses `runner: recipe` with a `contextual_round`
node binding one unguarded semantic step, `compose`, to `compose_post`. The host
checks policy, enrollment, opt-out, busy state and cooldown before dispatch,
provides the reviewed `context` queries, and commits validated drafts through
the App mutation owner; the prompt drafts a post or returns `quiet` and never
orchestrates writes. Per-participant and aggregate limits are part of the
reviewed recipe. The guarded grammar below remains supported independently.

## Shape

```yaml
operations: [engagement_gate, compose_post]   # allow-set, unchanged
steps:
  - id: gate
    operation: engagement_gate
    output_schema:
      type: object
      fields:
        decision: { type: enum, values: [engage, pass], required: true }
  - id: compose
    operation: compose_post
    when: { step: gate, field: decision, equals: engage }
    output_schema:
      type: object
      fields:
        post_type: { type: enum, values: [thought, reply, question, link], required: true }
        target_post_id: { type: text, nullable: true }
        body: { type: markdown, required: true }
```

A guard is **one equality against one field of one named earlier step** —
deliberately not an expression language, so a reviewer can read it in full.

## The rules, and why each exists

| Rule | Why |
| --- | --- |
| More than one operation requires `steps` | An allow-set cannot be executed without guessing an order. A single-operation behavior is exempt. |
| Every step's operation is in `operations` | The allow-set stays the authority boundary; the recipe only orders what was granted. |
| Every allowed operation is used by some step | Granted-but-unreachable authority still reads to a reviewer as capability. |
| Guards name an **earlier** step only | Self/forward references cannot resolve, so no cycles; enforced structurally (the check consults only already-validated steps). |
| The guarded field must exist in that step's schema | Otherwise the step silently never runs. |
| The guard value must be one the field can take | An enum typo yields a step that never runs and looks like a working recipe. |
| Guards compare enum, text or boolean only | Equality on formatted numbers or instants breaks when representation changes. |
| `steps` and behavior-level `output_schema` are mutually exclusive | Steps own their schemas; a second one can drift. |

## The recipe is review material

`steps_digest` is inside `app_behavior_request_digest`, compared in
`AppBehaviorGrant`/request equality, and recomputed by the scheduler against the
live manifest — otherwise a package update could reorder steps or drop a guard
under an unchanged selector/action/operation tuple. An empty recipe digests to
`None`, so step-less behaviors keep their prior review digest. Event behaviors
carry the same field.

## Executing a recipe

`apps/behavior_recipe.rs` owns the progression decision only: no I/O, no
authority, cannot dispatch. It is kept pure so a reader can check by eye that a
guard reads the right value (a wrong read does not fail; the behavior just
quietly stops doing half its job).

`next_recipe_step(steps, completed)` returns the step to run or `Complete`,
re-deriving from the durable progress record each call (no cursor), so resumed
and fresh runs agree and a lost or replayed response cannot advance the recipe.

- **`Complete` is a normal outcome.** A gate that said `pass` lands here; no
  further model call is owed.
- **Only an exact match admits a guarded call.** Absent, null, wrongly-typed,
  case or whitespace variants never match; booleans compare by value.
- **A step guarded on a skipped step is skipped too**; skipping does not end the
  recipe — later independent steps still run.
- **Corrupt progress is refused, not skipped.** Treating an unknown, duplicated
  or out-of-order record as "already ran" would skip reviewed turns.
- **Outputs are stored already validated**, so the resolver never decides from
  untrusted model bytes.

## Model admission

`AppWorkflowService::admit_recipe_step` admits a behavior-bound model turn for
**one reviewed step or not at all**; it returns `AppRecipeAdmission`
(`Step`, `Complete`, `NotBehaviorBound` for ordinary workflows, or
`Deterministic` for a locked native recipe). It runs
after the immutable manifest is open (the recipe lives there), and re-verifies
the live recipe against the grant.

| Refusal | Why |
| --- | --- |
| `BackgroundBehaviorOperationStepBindingRequired` — behavior-bound, no recipe | An allow-set cannot be executed without guessing an order. |
| `StaleBehaviorRecipeBinding` — live recipe digest ≠ grant's `steps_digest` | A reordered/unguarded recipe must not ride the old grant. |
| `StaleBehaviorRecipeBinding` — grant names a behavior the manifest no longer declares | Reviewed and live material diverged. |
| `AppRecipeAdmission::Complete` — nothing left to run | Not an error (only recording another output returns `BackgroundBehaviorRecipeComplete`), but must not become an operation call. |
| `BackgroundBehaviorRunTokensExhausted` — steps still owed, run budget spent | Exhausted is not complete. |
| `BackgroundBehaviorOperationStepBindingRequired` — both bindings present | "Pick one" would let a forged task choose its grant. |

The admitted step travels on the model input as `AppAdmittedRecipeStep` (step
id, operation, reviewed output schema, behavior identity, grant-recorded purpose,
resource ceiling, and the exact manifest it was verified from), so the
dispatcher checks against reviewed material and never re-opens the package.
Operation admission stays whole-manifest: one withdrawn operation stops every
step.

## Recording step output

`AppWorkflowService::record_recipe_step_output` is the only way
`behavior_recipe_progress` grows.

- Output is validated against the step's reviewed schema with the manifest's
  `validate_value` before storage (unknown fields fail closed).
- The admitted step is re-derived under the task guard, not taken from the
  caller; an outcome naming any other step is refused (stops replayed or
  out-of-order responses).
- Recipe admission runs again inside the call, catching a package update between
  the turn and its response.
- Every recorded step carries its output tokens; step admission re-derives the
  remaining `max_tokens_per_run` from progress, so an interrupted run cannot get
  the full budget twice.

## Resolving and dispatching

`resolve_recipe_step_operation` maps a step to its `AdmittedAppLlmOperation`,
checking the **live** half: the operator's `app_platform.llm_operations`
admission list and the router's `app:` mapping. A removed operation or route
fails closed rather than falling back to a core lane.

`admit_app_workflow_execution_context` (`artifact_v2/service.rs`) loops over
`AppRecipeAdmission`:

- `NotBehaviorBound` yields an `AppWorkflowAdmittedModelTurn` (goal +
  disclosure guard for the generic loop);
- `Step` is dispatched with one fresh admission per step on
  `AppLlmOperationDispatcher` via the permit-gated `app:` route, its output
  recorded, and the loop re-admits;
- `Complete` then yields the workflow's own turn (its `may_mutate` rows and
  terminal commit) on the same attested disclosure guard;
- `Deterministic` is refused here.

The turn is built only on the `NotBehaviorBound` and `Complete` arms, both after
the recipe fork, so a reviewed operation never reaches the generic lane. Why: a
behavior on the core lane would look like a normal success while bypassing
`app:` route pinning, live operation admission and the reviewed output schema.
When the app lane cannot be taken (no live config authority, withdrawn
operation, exhausted budget, denied dispatch) the turn is refused.

A settled recipe's step outputs are not a commit; it is held to the same
durable terminal-result gate as the agentic lane (no recorded result → fails).
App-lane model spend is not yet reserved in the resource ledger, so
installation-wide and monthly ceilings do not see these turns.

`max_output_tokens` for an admitted operation is
`min(operator admission, manifest hint, physical profile)` — always all three;
the manifest hint is a request, not a grant.

## Native contextual rounds

- **Context input.** `apps/workflow_model_context.rs` builds a runtime-only
  input from the exact retained tool checkpoints for the execution, projects the
  requested context, and joins handling labels before model admission. Prompt
  assembly separately checks each source for the model target (local retention
  permission is not model-send permission). The input stays a deterministic
  recipe input, so the generic agentic boundary refuses it. Recent-context
  queries use the indexed keyset path when supported; other shapes keep snapshot
  semantics.
- **Dispatch.** The bridge keeps task/execution/agent/step correlation, records
  the disclosure admission receipt, revalidates the canonical node claim, and
  uses the ordinary App LLM dispatcher and resource owner. Named App operations
  and disclosure-guarded calls require an installed dispatcher queue; without
  one the runtime is unavailable — never direct provider routing.
- **Progress and spend.** `apps/workflow_rounds.rs` stores fair progression and
  exact physical reservation refs in the sealed run state; context entries point
  at retained checkpoints without duplicating prompts. A logical attempt is
  persisted before queue dispatch and linked to the frozen physical operation
  before reservation; participant token/cost totals narrow the reservation, and
  retries cannot bill twice. Schema-validated output and canonical spend are
  retained together, so a prepared result commits after recovery without
  re-buying the call.
- **Accounting** matches the canonical resource journal by operation,
  reservation and observation; cached input is not double-counted; a missing or
  uncertain observation never becomes zero cost. A reservation is released only
  against its canonical pre-I/O proof; before any reservation, a failed call
  settles zero only with a runtime-only completion marker set after the
  protected dispatcher returns (drop/cancel does not set it). Post completion
  matches the native commit's stored result before a participant is committed.
- **Execution.** The round gathers permitted context, runs participants with
  bounded concurrency, and commits through the App mutation owner. Invalid output
  is a failed participant; an uncertain retained commit stays recoverable; quiet
  bookkeeping never counts as a post. Terminal output aggregates actual outcomes,
  spend and receipts. Admission and cancellation keep polling accepted
  participant futures (a participant may hold the task guard the control
  operation needs); unfinished physical calls drain through the dispatcher and
  resource settlement. Pre-I/O participant settlements report to the resource
  progress owner; claiming or retrying does not reset the idle guard.
- **Result slots.** Rounds reserve protected result slots from the reviewed query
  shapes, source-page bound, attested candidate count and commit attempts, sealed
  with the execution and recipe binding before queries run (instead of the
  agentic 24-result cap). Per-result, aggregate and sidecar byte ceilings, source
  labeling and disclosure checks still apply.
- **Stack.** Native lifecycle, context-query and host-read futures are boxed at
  await boundaries; the provider-free native-owner regression runs without a
  `RUST_MIN_STACK` override.

## Deterministic execution input

Native `runner: recipe` workflows use the shared execution input for governed
store reads and tool disclosure with admission state `Deterministic`; empty
operation/step declarations are valid. The locked runner, step digest and absence
of model-step progress are still checked. The model-processing boundary rejects
this state before profile lookup and it cannot record a model-step output or
grant an implicit agentic completion turn.

Model bindings are checked twice: manifest admission requires explicit reviewed
semantic steps for declared model operations; the dependency lock then requires
a contextual round to bind exactly its named unconditional semantic step, while
mechanical recipes reject model operations and semantic steps.

**Store-backed model input.** A scheduled recipe carries its owner-reviewed store
selector into model admission; the workflow owner reopens the record under
current installation and grant authority and compares content, field set,
revision and source policy with the accepted input. Only that proof populates
approved disclosure projections (also when admitting the turn after steps
finish).

## Commit and run-state fencing

- Native commits renew their canonical claim before taking the task file lock,
  then revalidate it under the borrowed guard (replaying abandoned writes;
  checking active root, claim epoch/owner, cancellation, deadlines) before intent
  persistence and entity I/O, holding the guard through the commit so a
  replacement worker cannot cross it.
- Reducer admission queues by task lock path before entering a blocking worker;
  the cross-process file lock protects each journal, and no reducer-wide mutex
  stalls unrelated tasks.
- Run-state reads authenticate one immutable registry snapshot on the blocking
  lane, then repair the filesystem cache from those bytes, keeping
  scope/task/execution HMAC binding and the 2 MiB / depth-64 / 100,000-node
  limits.

## Widget actions must accept the empty object

V1 has no row-to-action input mapping, so a widget button may expose only a
governed action whose workflow accepts exactly `{}`; required inputs would force
a client to invent a binding or smuggle input authority through UI state. The
rule fires before the duplicate-action check.
