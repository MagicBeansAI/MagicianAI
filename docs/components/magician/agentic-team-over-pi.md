# Agentic team over Pi (persona / skill / tool projection + contribute)

How a Magician agent team produces work **through** Pi: Pi is a shared coding substrate, and
whichever agent drives a given Pi loop **projects its identity into that loop** — its persona, any
activated procedure skill, and the subset of citizen tools it's allowed to call. Non-coding agents
publish finished artifacts into the same project via `contribute_to_project`.

> Design of record (archived): `docs/archive/plans/2026-06-19-agentic-team-over-pi-persona-projection.md`.

## The principle (one mechanism, not a subsystem)

The org chart (PA → CTO → IC engineers) is just the delegation tree that decides *who* drives
*which* Pi loop with *what* projection. The projection is `{persona, activated_skill, tool_allowlist}`,
computed at `run_coding_task` dispatch from data the driving agent already has — no new subsystem.

## P1 — persona projection

A coding run layers the **executing agent's** identity/persona onto Pi via a spawn-time
`--append-system-prompt` (it LAYERS onto Pi's built-in coding base prompt, it does NOT replace it).

- `CodingEngineRequest.append_system_prompt: Vec<PathBuf>` (`coding_engine/mod.rs`) carries one or
  more append-prompt files; `pi.rs` `command_args` emits `--append-system-prompt <path>` per entry.
- `run_coding_task` renders the same persona framing the agent runs under (autonomous-controls off)
  to a file **outside the shadow tree** (the same `pi-extensions` dir the citizen extension uses,
  keyed by the per-repo `persistent_shadow_key` to avoid cross-repo races) and pushes its path.
- Persona content is operator-YAML-sourced and framed by the shipped shared renderer (32k cap),
  so no LLM prompts are hardcoded in Rust. An empty-persona agent injects nothing (Pi keeps its base).

## P1b — config-driven coding lead

The coding lead is config, not a hardcoded id: `coding.lead_agent_id` (default `cto`) in
`magician-config.yaml`. A VibeDev **build** run — gated by `is_vibedev_coding_build_run(ui_thread_id,
tags)` in `artifact_v2/models.rs` (excludes `plan`/Discuss runs) — is stamped server-side to the
configured lead at task creation, so swapping the lead is a one-line YAML edit, not a UI/code change.

## P2 — skill projection (dormant by default)

The agent's active **procedure-skill** playbook is projected the same way: `run_coding_task` reads
the executing agent's `active_procedure_skill` memory tier (read-only) and folds the playbook body
into a second `--append-system-prompt` file, layered *after* the persona. **Dormant today** — no IC
engineer's `tools:` allowlist holds a procedure skill, so the helper returns `None` and the Pi argv
is byte-identical. Granting + activating a procedure skill makes it carry content with no
coding-engine change.

## P3 — per-grant citizen tool scoping

The Pi-side citizen bridge exposes a fixed set of magician tools; P3 scopes them per coding run.

- `CitizenGrant.allowed_tools: Vec<String>` (`coding_engine/citizen.rs`) — empty == unscoped == all
  (byte-identical default); a populated list permits only those capabilities. `run_coding_task`
  derives it from `plan_only` (plan/Discuss → `["code_knowledge"]` read-only; build → all 3) and
  always sets the `MAGICIAN_CITIZEN_TOOLS` env so a stale host value can't leak in.
- `authorize_citizen(req, capability)` (`api/vibedev_api.rs`) is the single SERVER-SIDE choke point
  (bearer → resolve grant → per-tool check → `403` on miss), independent of client registration.
- The Pi-side bridge (`assets/pi-extensions/magician-citizen.ts`) only `registerTool`s a tool whose
  name is in the env allowlist (absent/empty == all).

Why: `magician_secret` (the only privileged side-effect of the 3) is never granted to a read-only
Discuss run.

## `contribute_to_project` (compiled handler)

Lets a **non-coding** agent publish finished, named artifacts into a VibeDev project's repo — so an
engineer's Pi loop later reads them as references — without that agent writing source code.

- **File contribution.** Restricted to `docs/` / `specs/` / `design/` only (a non-Pi agent cannot
  write source). The project repo is resolved via `resolve_coding_repo_binding` (in-workspace or
  external); guarded by a canonicalize + `starts_with` repo-containment check and a symlinked
  top-level-dir rejection (so e.g. `docs/` → `src/` can't escape the restriction). Source is inline
  `content` or a `source_path` copied from the agent's own task outputs (size-capped + containment-checked).
- **Write mode = config-switchable** via `coding.contribute_direct` (default `true` = direct write
  into the repo; `false` = staged diff-approval HITL, in-workspace only).
- **Recall lane.** A landed direct contribution also appends a `project_id`-stamped fact to a shared
  `vibedev-project-knowledge` recall lane (`code_knowledge` tier; const lane id in
  `agents/project_knowledge.rs`, shared by write + read so they can't diverge). The citizen
  `magician_code_knowledge` read is extended to rank that lane too, so a contribution is visible to
  WHATEVER engineer runs Pi, not just the contributor. The synthetic lane is injected into the
  hybrid memory index for semantic recall **only when it actually has facts** (existence check in
  `definition_store::list_moveable_definitions`).

## Roster

| Agent | Change |
|---|---|
| Engineering Manager "Bridge" | **Retired** — `disabled: true` (memory tiers preserved). |
| CTO "Forge" | The **coding lead** — enabled, given the delegation toolset, `max_delegation_depth: 3` + `allow_transitive_delegation: true`, full team-roster coding-lead persona. |
| CEO "Atlas" | Delegates to `cto`. |
| creative-mind "Muse" | Enhanced **additively** as the design authority — a "project design authority" persona section + a `contribute_to_project` grant (general-creative role intact); added to the CTO's `delegation_targets`. |

The CTO is the single engineering authority and delegates directly to the coding ICs (architect,
frontend / backend / principal / junior engineers); Muse (design) is also among its targets. Other
function leads are peers under the chat entry and work in magician's loop, contributing via
`contribute_to_project`.

## Key files

- `magician/src/magician_v2/execution/coding_engine/mod.rs` — `CodingEngineRequest.append_system_prompt`.
- `magician/src/magician_v2/execution/coding_engine/pi.rs` — emits `--append-system-prompt`.
- `magician/src/magician_v2/execution/coding_engine/citizen.rs` — `CitizenGrant.allowed_tools`.
- `magician/src/magician_v2/execution/compiled_handlers/run_coding_task.rs` — persona/skill append + grant mint.
- `magician/src/magician_v2/execution/compiled_handlers/contribute_to_project.rs` — the contribute handler.
- `magician/src/magician_v2/execution/embedded_pack_defs/contribute_to_project.yaml` — its pack def.
- `magician/src/magician_v2/api/vibedev_api.rs` — `authorize_citizen` + the citizen recall-lane read.
- `magician/src/magician_v2/agents/project_knowledge.rs` — `PROJECT_KNOWLEDGE_AGENT` + shared tier def.
- `magician/src/magician_v2/agents/definition_store.rs` — index-injects the lane only when it has facts.
- `magician/src/magician_v2/artifact_v2/models.rs` — `is_vibedev_coding_build_run`.
- `magician/src/config.rs` — `coding.lead_agent_id` / `coding.contribute_direct`.
- active `magician-config.yaml` — the `coding:` block (lead = `cto`).
- `magician/assets/pi-extensions/magician-citizen.ts` — Pi-side bridge (granted-only `registerTool`).
- Roster YAML (`magician_data_v3/.../agents/`): `cto`, `engineering-manager`, `ceo`, `creative-mind`.
