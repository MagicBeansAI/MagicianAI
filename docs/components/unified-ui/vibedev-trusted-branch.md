# VibeDev cockpit — trusted projects and the Branch tab

Plan: `docs/archive/plans/2026-10-07-vibedev-trusted-repo-mode.md` §4.11.
Server side: [coding-engine isolation — trusted repo mode](../magician/coding-engine-isolation.md#trusted-repo-mode-no-shadow)
and the [API routes](../magician-api/README.md#vibedev-trusted-repo-mode).

## Turning it on

Project settings (`VibeStudio.svelte`) offers **Work location**: Original folder,
Isolated workspace, or Isolated branch. Isolated branch also shows a **Base
branch** field (empty = the repo's current branch). Saving sends `trust` / `base_branch` in the
project PATCH; the server refuses unless the caller is the owner and the repo folder is a git
repository root, and the toast shows its message. Conversations already started keep their mode.
If the folder later stops being a repository, `health` warns "Folder isn't a git repository" with
a **Make this a git repository** button (confirm, then `POST /vibedev/projects/{id}/git-init`);
meanwhile runs use a sandboxed copy with diff review, and their goal and `run_coding_task` result
(`trusted_fallback`) say so ([rail §5](../magician/vibedev-rail.md#5-where-the-prose-lives)). A
missing folder or base branch is an error: "Project needs attention: runs cannot start.", marked in
the project menu, and run starts are refused.

Original folder uses the existing branch and uncommitted files. Its shared
composer approval card shows the exact diff before edits, with Allow once,
Allow this session and Allow always. Normal and Super god permissions use the
same work-location choice; Discuss stays read-only. Starting a run refreshes its
admitted location before the composer opens it; changes to project defaults
affect new runs. Work location shares the composer’s App options / Save as task row;
both selectors use compact rounded rectangles. Original runs use no
proposal/merge workflow.

## The Branch tab

`stage/StudioStage.svelte` shows **Branch** instead of **Diff** for a trusted project. Diff stays
visible only while older staged proposals exist (a conversation that started sandboxed), and the
stage switches tabs when the project's mode makes the selected one disappear. "Apply all changes"
(status bar and Diff tab) shows only while a proposal waits (`isPendingDiffRow`); finished
proposals render as Applied/Rejected history, and an owner briefing (`isNoticeRequest`) renders as
a notice with Acknowledge (`acknowledgeVibeNotice`), never as a review.

`stage/StageBranch.svelte` is keyed by the active chain's **root** task (`chainRootTaskId` in
`stores/vibeHitlStore.ts`, walking `Parent task:` links — not the project's
`active_root_task_id`, which the cockpit re-pins to the latest run). It loads
`GET /vibedev/runs/{root}/branch` (`stage/trustedBranch.ts`) on mount, on every
`coding.branch_updated` event or hub resync, and every 15 s, and shows:

- the branch, its base and short SHA, the commit count, "`<base>` moved on by N" when the base has
  commits the branch lacks, and "turn in progress" while the clone has uncommitted changes;
- **Merge into `<base>`** (confirm: fast-forward vs merge commit, from `behind_base`), **Open PR**
  (an inline title/description form prefilled from the commits; pushes with the owner's own `git`
  and `gh` logins; once a PR exists, **View PR** plus **Update PR**, which pushes the branch's new
  commits to it), **Discard** (destructive confirm; says how many unmerged commits go). Landing is
  disabled while the clone is dirty, with no commits, or after a merge until a later turn commits
  again — a merge moves the chain's base to the merged head, so a follow-up turn's commit count, diff
  and next merge cover only what it added; Merge is disabled for a chain that started from a detached
  HEAD;
- the commits, newest first, with **Rewind here** on every commit but the head (destructive confirm).
  The server also queues the session of the turn at that point, so the agent's next turn resumes its
  conversation from there (the toast says so when it did);
- clone size, shared build dir size and free disk (`GET /vibedev/runs/{root}/disk`, cached 5 min
  server-side; refetched on open, after each turn and every 5 min; free space under 10 GB is
  highlighted);
- a read-only `DiffStrip` of `base..HEAD`.

Checks (the Tests tab and the automatic check after a run settles) send the chain root as
`task_id` for a trusted project, so they run against the branch's clone (`stage/checks.ts`
`runCheck`).

Server refusals (conflicts, a dirty checked-out base, the secret scan, a running turn) surface as
an error toast with the server's message. Empty states explain trusted mode before the first run
and point an untrusted chain at the Diff tab.

Tests: `stage/trustedBranch.test.ts` (client, PR defaults, `chainRootTaskId`).
