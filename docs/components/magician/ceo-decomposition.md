# CEO Decomposition — backend

Hierarchical goal-setting for the Fleet Civilization: goal → missions → officer
playbooks, approve-first.
The UI calls these documents [Playbooks](../unified-ui/unified-work.md#work-vocabulary);
API paths, tools and stored `program` fields retain their technical names. Design: `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`.

## Playbooks (program documents)

The [crew operating Programs](crew-programs.md) map the git-backed role
instructions to existing focus areas and describe their goal links and authority
boundaries.

**`harness::program_doc`** provides Playbook reads
(`list_program_docs`/`read_program_doc`) and **`ProgramDocEditor`**. Owner
saves edit the whole document through the API below. CEO mission edits touch
only the managed `## Missions (CEO)` section, stamp provenance, refuse new files
and reject bodies over 4 KB. Writes snapshot the prior content to
`programs/.history/`. Mutable progress remains in the separate program-state lane.

**Owner revert**: `ProgramDocEditor::revert_from_history` restores a program
from a `.history/` snapshot (default newest; named snapshots resolve
strictly against the program's own listing — never used as paths; stems are
matched strictly so `company.md` never claims `company-strategy-*`
snapshots). The current content is snapshotted FIRST — a revert is itself
revertible.

## API

- `GET /v2/programs`, `GET /v2/programs/{name}` — read-only, scope-aware
  program access (titles, content, missions section, `history` list) for the
  game's guild mission chips and the CEO surfaces.
- `PUT /v2/programs/{name}` (`{content, base_sha256?}`) — the owner's whole-document
  save from Work → Playbooks (web, iOS, Android): a signed-in owner session or
  the owner's paired iOS/Android app (an ESP or desktop Edge credential, the
  Android Apps automation credential and agent grants are refused). `base_sha256` is the `sha256` the GET returned; it is required for an
  existing program and omitted to create one, so a document the CEO tool changed
  while the owner was editing answers `409 program_changed` instead of being
  overwritten. The replaced content is snapshotted to `.history/` first, so a
  save is revertible like any other edit.
- `POST /v2/programs/{name}/revert` (`{snapshot?}`).

## Tools

- **`propose_program_missions`** (compiled handler +
  `embedded_pack_defs/propose_program_missions.yaml`) — the CEO tool:
  validates the contract (1..=5 missions w/ title/objective/success_criteria,
  rationale, existing program, 1h per-program cooldown), renders the managed
  section, applies via the editor, and reports the bound officers. Granted to
  the CEO template with a `requires_approval` rule, so every proposal pauses
  as an approval (the game's [!] loop renders it with zero new UI). The
  tool's contract text ships as the pack `guide`.
- Officers are mobilized immediately on apply: `AgentResources.agent_runtime`
  (optional, boot-injected) lets the tool call
  `trigger_goal_awaitable_in_scope` for each bound officer's matching focus
  area (`GoalSource::User`; one trigger per officer; receipts in the tool
  response). Minimal boots without the handle degrade to next-cycle pickup.
- **`review_program_missions`** (compiled handler + embedded pack; granted to
  the CEO WITHOUT an approval gate — read-only by design): per program with a
  managed missions section (or one requested `program`), returns the missions
  body, the bound officers, and each officer's program runtime state
  (`last_run_summary`, `open_loops`, `blocked`, `next_action_hints`,
  `updated_at` — written by the harness every cycle; `null` = the officer has
  not run since the missions landed). The CEO's board-review cycle calls this
  FIRST and consolidates outcomes from evidence, closing the loop:
  goal → missions → officers work → board review.
