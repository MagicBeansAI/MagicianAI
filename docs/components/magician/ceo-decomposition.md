# CEO Decomposition — backend

Hierarchical goal-setting for the Fleet Civilization: goal → missions → officer
programs, approve-first. Design: `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`.

## Program documents

**`harness::program_doc`** — program-document reads
(`list_program_docs`/`read_program_doc`) + **`ProgramDocEditor`**: edits ONLY
the managed `## Missions (CEO)` section (replace-or-append), stamps
provenance, snapshots the prior file to `programs/.history/` before every
write, refuses new files and >4KB bodies. Deliberately separate from the
learning program-STATE lane.

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
