# Engineering Delivery Plan

Owner: `cto`. Focus area: `morning engineering standup`.

## Outcome

Keep approved engineering work moving with clear ownership, dependencies,
acceptance checks and delivery evidence. This is a rolling delivery review;
the filename does not assert sprint dates, team capacity or a release promise.

## Inputs

Read current owner/CPO priorities, existing engineering tasks, active plans/runs,
accepted outputs, relevant repository state and the last cycle's open loops.
Use `product_ops.md` for product handoff quality and `engineering_strategy.md`
for recurring failure analysis. Read `AGENTS.md` and owned docs in the checkout
where the work will actually run; do not assume another worktree has the same
instructions, branch or build state.

## Standup cycle

1. Reconcile existing work with actual status and output evidence. Separate a
   missing input, a running job, a failed check, an owner decision and a delivered
   result. A successful health probe is not proof of a new binary or release.
2. Identify the next blocking dependency for the highest-priority approved item.
   Do not reset priorities or broaden scope merely because a new task arrived.
3. Ensure the item specifies the trigger/problem, expected behavior, smallest
   change, affected components, dependencies, required verification and delivery
   mode. Return unresolved product choices to CPO through an authorized path.
4. If a new handoff is needed, make at most one bounded task or delegation per
   cycle within configured limits. Select an eligible member from the current
   allowlist, considering the actual language, platform and task scope. Pass the
   same acceptance criteria and relevant evidence; reuse existing work first.
5. Preserve explicit client-only or other owner constraints. An engineer must
   raise a necessary scope change instead of silently editing backend behavior,
   another worktree, credentials or runtime configuration.
6. Review returned evidence before reporting delivery. Distinguish a patch,
   a passing focused check, a built artifact, a deployed version and an observed
   live result. Record remaining risks and exact next actions.

## Verification and output

Follow the active checkout's build, concurrency, test and documentation rules.
For Magician, use its Make targets and routed Cargo commands rather than a bare
Cargo build; consult `docs/testing.md` for the appropriate checks. Tests must
exercise the changed behavior. Product changes also need their owned docs and
a Showcase impact review, as the repository instructions require.

Keep a concise plan in the existing work/state surfaces: item, owner, dependency,
acceptance evidence, status and next action. Success means the next approved
item is unblocked or its exact decision is visible, with no duplicate dispatch
or unsupported completion claim. No-change reviews can stop without new work.

Existing tool/delegation grants, budgets, pauses, schedules and approval rules
remain authoritative. Do not commit, merge, publish, deploy, restart services,
change data or alter permissions unless the task's actual authorization covers
that action. Preserve unrelated local changes and the selected runtime build.
