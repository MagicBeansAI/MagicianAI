# Engineering Quality and Agent Improvement

Owner: `cto`. Focus areas: `sprint review` and `agent improvement`.

## Outcome

Improve engineering delivery by addressing evidenced recurring failures with
small, verifiable changes. Keep routine delivery in `sprint_plan.md`; this
Program owns review and bounded improvement proposals, not an unrestricted
refactoring or agent-policy mandate.

## Inputs

Use existing engineering tasks and outputs, failed checks, scoped run evidence,
previous repair results, and approved priorities. Read `AGENTS.md`,
`docs/testing.md` and relevant component contracts in the active checkout.
Consult `harness_reliability.md` for scheduler/anomaly boundaries and the Quality
checks contract when a task is eligible for that existing workflow.

## Sprint review

1. Reconcile delivered work with acceptance evidence. Keep cancelled, failed,
   unverified and still-running work distinct. Do not infer delivery quality from
   the number of tasks or scheduler triggers.
2. Group concrete blockers by cause and dependency. Deduplicate repeated records
   of the same failure before calling it a recurring pattern.
3. Examine observation windows, denominators and missing telemetry before making
   throughput or reliability claims. Keep unmeasured change explicitly unmeasured.
4. Select the most consequential evidenced pattern within approved priorities.
   If there is no actionable recurrence, record that and stop.

## Agent improvement

1. Check whether the selected issue already has an investigation, repair or
   accepted workaround. Preserve the links and avoid parallel duplicate work.
2. Separate a product defect, missing instruction, environment problem, model
   limitation and permission/approval block. A denied operation is not evidence
   that the policy should be weakened.
3. Propose the smallest change, with source task/execution references, expected
   behavior, a regression or evaluation, cost/time bounds and a stop condition.
   Preserve the owner's current task constraints and privacy/model boundaries.
4. Use at most one new bounded investigation or implementation handoff per cycle,
   through an available tool and current allowed target. Match the target to the
   problem; do not modify another member's definition or memory as a shortcut.
5. Freeze acceptance criteria before the attempt. Use an existing independent
   review or Quality workflow when available and appropriate; never describe an
   artifact's self-review as independent verification.
6. Recheck the returned fix against the original failure. Where recurrence can be
   observed, compare equivalent windows; otherwise report the test result and
   the remaining live-validation limit separately.

## Output and success checks

Record the pattern, evidence, likely cause, existing repair, proposed change,
acceptance check and next owner in normal task/program state. Keep progress for
each focus area's existing goal identity distinguishable. A success is a
verified bounded repair or a decision-ready proposal, not a larger prompt,
another investigation or an unsupported claim of improved performance.

Global process, architecture, agent-definition, routing, permission and Program
changes require their existing owner review. No tool or spending authority is
granted by this document. Preserve the current pause state, schedules, limits
and required approvals; no change is automatically deployed or activated.
