# Agentic team over Pi (persona / skill / tool projection + contribute)

How a Magician agent team produces work **through** Pi. Pi is a shared coding
substrate, and whichever agent drives a Pi loop **projects its identity into
that loop**: its persona, any activated procedure skill, and the citizen tools
it may call. Non-coding agents publish finished artifacts into the same project
with `contribute_to_project`.

> Design of record (archived): `docs/archive/plans/2026-06-19-agentic-team-over-pi-persona-projection.md`.

## The principle (one mechanism, not a subsystem)

The org chart (PA → CTO → IC engineers) is just the delegation tree that decides
*who* drives *which* Pi loop with *what* projection. The projection is
`{persona, activated_skill, tool_allowlist}`, computed at `run_coding_task`
dispatch from data the driving agent already has.

## Persona projection

A coding run layers the **executing agent's** persona onto whichever coding
engine runs it. It adds to the engine's built-in coding base prompt; it does
not replace it.

- `CodingEngineRequest.append_system_prompt: Vec<PathBuf>`
  (`coding_engine/mod.rs`) carries one or more append-prompt files (persona,
  then playbook) for every engine. Each adapter delivers them its own way:

  | Engine | Channel | Resumed session |
  |---|---|---|
  | Pi | `--append-system-prompt <path>` per file (`pi.rs` `command_args`) | sent again on every spawn |
  | Claude Code | `--append-system-prompt <text>`; text over 96 KiB goes through `--append-system-prompt-file` (one file, joined beside its sources when there are several) | sent on every launch: the system prompt is not session history |
  | Codex app-server | `developerInstructions` on `thread/start` (keeps Codex's base instructions) | not resent: the thread holds them from its start |
  | Grok ACP, Agy | no system-prompt channel: a fenced `STANDING INSTRUCTIONS` block ahead of a new session's first prompt | not resent: the block is in the session's first prompt; a resume that falls back to a fresh session gets it again |

  A file that cannot be read is skipped with a `coding_engine` warning naming
  the engine.
- `run_coding_task` renders the persona framing the agent runs under
  (autonomous controls off) to a file **outside the shadow tree** (the
  `pi-extensions` dir the citizen extension uses, keyed by the per-repo
  `persistent_shadow_key` to avoid cross-repo races) and pushes its path.
- Persona content comes from operator YAML and is framed by the shared renderer
  (`render_prompt_identity_section`, 32,000-char cap), so no LLM prompt is hardcoded in Rust. An empty persona injects
  nothing.

## Config-driven coding lead

The coding lead is `coding.lead_agent_id` in `magician-config.yaml` (the
shipped seed sets `cto`; with no value set, the run keeps the agent the client
asked for, else `personal-assistant`). A VibeDev **build** run, gated by
`is_vibedev_coding_build_run(ui_thread_id, tags)` in `artifact_v2/models.rs`
(which excludes `plan`/Discuss runs), is stamped server-side to the configured
lead at task creation, so swapping the lead is a one-line YAML edit.

## Skill projection

`run_coding_task` reads the executing agent's `active_procedure_skill` memory
tier (read-only) and folds the playbook body, and the files it links to
(within a 256 KiB budget), into a second append-prompt file after the persona,
delivered to every engine as above. With no active procedure skill the helper
returns `None` and the engine's launch is unchanged.

## Per-grant citizen tool scoping

The Pi-side citizen bridge exposes a fixed set of magician tools, scoped per
coding run:

- `CitizenGrant.allowed_tools: Vec<String>` (`coding_engine/citizen.rs`):
  empty means unscoped (all); a populated list permits only those
  capabilities. `run_coding_task` derives it from `plan_only` (plan/Discuss →
  `["code_knowledge"]` read-only; build → all 3) and always sets the
  `MAGICIAN_CITIZEN_TOOLS` env so a stale host value cannot leak in.
- `authorize_citizen(req, capability)` (`magician-api/src/vibedev_api.rs`) is
  the single server-side choke point (bearer → resolve grant → per-tool check;
  `401` for a missing or unknown token, `403` for an ungranted tool), independent of client registration.
- The Pi-side bridge (`assets/pi-extensions/magician-citizen.ts`) only
  `registerTool`s a tool named in the env allowlist (absent or empty = all).

So `magician_secret`, the only privileged side effect of the three, is never
granted to a read-only Discuss run.

## `contribute_to_project` (compiled handler)

Lets a **non-coding** agent publish finished, named artifacts into a VibeDev
project's repo, which an engineer's Pi loop later reads as references, without
that agent writing source code.

- **File contribution.** Only into `docs/` / `specs/` / `design/`. The repo is
  resolved with `resolve_coding_repo_binding` (in-workspace or external) and
  guarded by a canonicalize + `starts_with` containment check and rejection of
  symlinked top-level dirs (so `docs/` → `src/` cannot escape). Source is
  inline `content` or a `source_path` copied from the agent's own task outputs
  (at most 10 MiB, `MAX_FILE_READ_BYTES`; containment-checked).
- **Write mode** is `coding.contribute_direct` (default `true` = direct write
  into the repo; `false` = staged diff-approval HITL, in-workspace only).
- **Recall lane.** A landed direct contribution also appends a
  `project_id`-stamped fact to the shared `vibedev-project-knowledge` recall
  lane (`code_knowledge` tier; one lane-id constant in
  `agents/project_knowledge.rs` shared by write and read). The citizen
  `magician_code_knowledge` read ranks that lane too, so any engineer running
  Pi sees the contribution. The synthetic lane is injected into the hybrid
  memory index **only when it has facts** (existence check in
  `definition_store::list_moveable_definitions`).

## Roster

| Agent | Change |
|---|---|
| Engineering Manager "Bridge" | **Retired**: `disabled: true` (memory tiers preserved). |
| CTO "Forge" | The **coding lead**: enabled, given the delegation toolset, `max_delegation_depth: 3` + `allow_transitive_delegation: true`, full team-roster coding-lead persona. |
| CEO "Atlas" | Delegates to `cto`. |
| creative-mind "Muse" | Design authority, added on top of its general-creative role: a "project design authority" persona section and a `contribute_to_project` grant; in the CTO's `delegation_targets`. |

The CTO is the single engineering authority and delegates directly to the
coding ICs (architect, frontend / backend / principal / junior engineers) and
to Muse. Other function leads are peers under the chat entry; they work in
magician's loop and contribute through `contribute_to_project`.

## Key files

- `magician/src/magician_v2/execution/coding_engine/mod.rs`: `CodingEngineRequest.append_system_prompt`.
- `magician/src/magician_v2/execution/coding_engine/pi.rs`, `claude.rs`, `codex.rs`, `grok.rs`, `agy.rs`: deliver `append_system_prompt` per the table above.
- `magician/src/magician_v2/execution/coding_engine/citizen.rs`: `CitizenGrant.allowed_tools`.
- `magician/src/magician_v2/execution/compiled_handlers/run_coding_task.rs`: persona/skill append + grant mint.
- `magician/src/magician_v2/execution/compiled_handlers/contribute_to_project.rs`: the contribute handler.
- `magician/src/magician_v2/execution/embedded_pack_defs/contribute_to_project.yaml`: its pack def.
- `magician-api/src/vibedev_api.rs`: `authorize_citizen` + the citizen recall-lane read.
- `magician/src/magician_v2/agents/project_knowledge.rs`: `PROJECT_KNOWLEDGE_AGENT` + shared tier def.
- `magician/src/magician_v2/agents/definition_store.rs`: index-injects the lane only when it has facts.
- `magician/src/magician_v2/artifact_v2/models.rs`: `is_vibedev_coding_build_run`.
- `magician/src/config.rs`: `coding.lead_agent_id` / `coding.contribute_direct`.
- active `magician-config.yaml`: the `coding:` block (lead = `cto`).
- `magician/assets/pi-extensions/magician-citizen.ts`: Pi-side bridge (granted-only `registerTool`).
- Roster YAML (`magician_data_v3/system/agent_templates/agents/<id>/definition.agent.yaml`): `cto`, `engineering-manager`, `ceo`, `creative-mind`.
