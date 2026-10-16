# Run state — durable multi-session runs with out-of-band verification

Composable Work Modules, **Module C's primitive**. Module: `magician/src/magician_v2/run_state/`.

Durability for *"a multi-session authenticated form that spans days and
requires an out-of-band verification step"* — grant applications, CFPs, vendor
onboarding, KYC, procurement portals.

**Reachable from `work_modules_api`.** `magician-api/src/work_modules_api.rs`
mounts `/api/magician/v2/work/runs` — open, declare, answer, raise and resolve
gaps, raise and fulfil waits, and submit. `GET /work/runs/{run_id}` is the
owner's read: the run, what it is waiting for (`expectations_awaiting`) and what
needs a human (`gaps_for_owner`), in one call. `POST /work/runs/{run_id}/submit`
goes through `RunStateStore::submit`, which is the Form-channel write point
`docs/components/magician/outward-assertions.md` names — it records the covering
act *before* the run seals, so a sealed run always has a disclosure to account
for it.

`all_runs` exists because run ids are **derived** from
`(scope, purpose, resource_ref)` and nothing indexes them. Without a listing an
owner who could not restate that tuple character-for-character could not reach
a form holding days of their own answers, and an unlistable directory answered
as *"no runs"* would then have `open` fabricate a blank one beside it — so
absent is the only condition that reads as empty.

## Grounding is structural, not advisory

An `Answer` with no evidence refs is **unrepresentable as a stored answer** —
`record_answer` refuses it, and the caller records a `Gap` instead. *"Never
answer from nothing: an invented metric on a real application is unrecoverable
in a way a missed deadline is not."* A gap is resolved only by a **named
person** with a grounded answer (the owner's reply is itself the evidence), and
resolution writes the answer into the field — one truth, not two.

## The inbox-coupled wait, decoupled from any inbox

An `Expectation` records that the run is waiting and where the event will
arrive. **The module never reads an inbox** — the caller extracts the code and
feeds it back with an event ref. Fulfilling an unknown or already-fulfilled
expectation is refused: feeding the wrong event to the wrong wait is how a
stray email resumes the wrong run.

## The submit gate no machine can fire

`ReadyForReview` is derived — every required field grounded, no open required
gaps, no open expectations, and **a run with zero fields is never ready**
(vacuous truth guard). `record_submission` requires a named person and the
outward act's ref, mirrors `commitments::confirm`, refuses unless ready, and
the first submission wins — defensively in the fold too, which is what makes
the unavoidable check-then-append race converge instead of double-firing.
After submission everything refuses: the record is evidence of what went out.

## Resumability

Run ids derive from `(scope, purpose, resource_ref)`, so a crashed session
resumes THE run. Revisions are one run-wide monotonic sequence stamped onto the
changed field, so a single `changed_since` watermark answers "what changed
since I last looked". Identical-answer replays are no-ops. Torn-tail and
absent-vs-unreadable semantics come from `magician_v2::jsonl`.
