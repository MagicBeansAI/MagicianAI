# Product Backlog Operations

Owner: `cpo`. Focus area: `backlog grooming`.

## Outcome

Keep one actionable product backlog: clear problems, current evidence, explicit
priorities, accountable owners and implementation-ready handoffs to engineering.
Reduce ambiguity and duplicate work before asking CTO to build anything.

## Inputs

Use owner directives, permitted goal notes, existing backlog/tasks, unresolved
user problems, actual usage evidence and previous delivery outcomes. Consult
`product_strategy.md` for hypothesis selection and the current feature contract
for what is already implemented. Read available source records rather than
promoting a summary or a guessed complaint into a requirement.

## Grooming cycle

1. Review new or changed items since the previous successful pass. Search the
   current backlog and delivered work for the same outcome before adding anything.
2. Separate a verified defect, a requested improvement, a research question and
   an untested product bet. Keep the original source and the affected journey.
3. Prioritize against adopted goals, blocking dependencies, observed impact and
   recurrence. State effort and evidence uncertainty. Do not fabricate scores,
   customer counts or deadlines to make an item appear important.
4. For the next candidate, specify the trigger, current behavior, desired outcome,
   smallest useful scope, non-goals, dependencies, acceptance evidence and owner.
   State client-only or other task constraints explicitly when they apply.
5. Route unresolved product choices to the owner with a concrete recommendation
   and alternatives. Keep the item blocked on that decision instead of silently
   choosing a new policy, permission model or product direction.
6. Promote at most one ready item per cycle through an available, permitted
   backlog/task mechanism. Hand implementation to `cto` with the same source
   references and success criteria. Check for an existing engineering task first.

## Handoff quality

A ready item has an observable before/after, a bounded change and acceptance
criteria that engineering can verify. It preserves data and names required
review or rollout constraints. Research work is ready only when its question,
scope, output and stop condition are equally clear; it is not a disguised build.

Use the current allowed delegation targets. `web-researcher` or
`company-assistant` can help collect permitted evidence; delegation does not
expand either party's tool or workspace authority.

## Output and success checks

Record the triage disposition, evidence, owner, next action and engineering
reference in the existing backlog/task/state surface. Link duplicates without
deleting their source evidence. Completion follows accepted delivery, not a
handoff or task creation. A successful pass leaves the next item buildable or
states the exact blocker; a no-change pass creates no duplicate work.

Existing pause, schedule, budget and approval rules apply. This Program does not
authorize direct code changes, new business commitments, outward messaging or
changes to agent definitions, grants or canonical product strategy.
