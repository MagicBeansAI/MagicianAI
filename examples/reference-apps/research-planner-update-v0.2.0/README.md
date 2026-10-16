# Research Planner 0.2.0 update candidate

This is a complete, separately generated package candidate for updating the
base `research-planner` 0.1.1 installation. It is not a patch overlay. The
schema adds nullable `research_plan.reviewed_at` and retires
`research_plan.revision_note`; the exact public update-plan input is
[`migrations/v0.1.1-to-v0.2.0.json`](./migrations/v0.1.1-to-v0.2.0.json).

The retirement is destructive, so the lifecycle proof must compile and dry-run
the exact operation list, create the encrypted pre-switch backup, and pass both
the migration run and update-plan digest to reviewed approval with explicit
destructive confirmation. No script infers success or skips review.

Generate this candidate through `magician app check ./app --write-generated`,
then test and pack it independently before `candidate-publish --attempt-kind
update`.
