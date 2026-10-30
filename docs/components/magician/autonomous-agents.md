# Autonomous Agents

Work calls the member control **Autopilot**, its scheduled reviews **Scheduled
check-ins**, and its instruction documents **Playbooks**. The [Work vocabulary](../unified-ui/unified-work.md#work-vocabulary)
explains these controls; `harness`, `autonomous_config` and `program` remain the
configuration fields described below.

Most agents act when spoken to. An agent with an `autonomous_config` block acts
on its own schedule, with no user turn to trigger it. That block's presence is
the enablement — there is no `enabled` flag.

## The declaration

From [`ambassador`](../../../magician_data_v3/system/agent_templates/agents/ambassador/definition.agent.yaml):

```yaml
autonomous_config:
  schedule: "8,38 * * * *"          # twice an hour
  focus_areas:
    - name: inbound sweep
      description: Read what arrived on the company's own mailbox and number
        since the last cycle. Answer what the charter already answers, escalate
        what needs the founder, and leave nothing silently unread.
      priority: high
      schedule: "8,38 * * * *"
      program: outward_charter.md
    - name: follow through
      priority: medium
      schedule: "50 */6 * * *"
```

Three things are worth noticing:

- **Focus areas carry their own schedule.** An agent can sweep its inbox every
  half hour while chasing follow-through every six.
- **A focus area names a `program`** — a markdown charter the cycle is run
  against, so the standing instruction lives in the store rather than in Rust.
- **Priority is declared**, not inferred, so a cycle that cannot do everything
  has an ordering.

## What ships

Nine shipped agents declare the block. **Seven fire:**

| Agent | Schedule |
| --- | --- |
| `ambassador` | `8,38 * * * *` |
| `cro` | `5,35 * * * *` |
| `cmo` | `10,40 * * * *` |
| `cpo` | `15,45 * * * *` |
| `cto` | `20,50 * * * *` |
| `ceo` | `25,55 * * * *` |
| `harness-sre` | `7 * * * *` |

Two more — `creative-mind` and `personal-assistant` — are declared but parked on
`0 0 31 2 *`. February 31st never occurs, so they never fire. That is a
deliberate off-switch that keeps the configuration intact.

The staggered five-minute offsets across the C-suite set are not incidental:
those agents are an ongoing experiment in whether an organisation's standing
work can be decomposed across scheduled roles. Treat that set as a bet, not a
shipped product.

## How a cycle differs from a chat turn

An autonomous cycle is a distinct code path, not a simulated user message.

- `pipeline/agent.rs` carries `is_autonomous_cycle` through the run.
- The `ExecutionRun` has no chat session id — `realtime_events.rs` documents
  `None` for autonomous origins, and `artifact_v2/models.rs` treats
  scheduler/API/autonomous origins as background-spawned work the user owns.
- `learning/work_ledger_program_state.rs` distils progress against
  `autonomous_config.focus_areas`, so a cycle's output is attributable to the
  focus area that produced it rather than to an anonymous background run.

Because the run is owned but unattended, everything in
[The OS Primitives](../../architecture/os-primitives.md) applies with more force:
an unattended agent is exactly the one that should be spending against a budget
it cannot exceed.

Each cycle is identified by `(agent, goal, trigger_seq)`, and a tuple an
episode already records never runs again. Deleting an agent removes its
definition and scheduler entry but keeps its episodes, so sequences are
allocated above every sequence already used for the agent/goal (runtime state
and stored episodes), not from the scheduler counter alone. A re-created agent
with an old agent's id therefore gets fresh tuples and runs. A trigger that
does name an already recorded tuple is refused before anything is provisioned
(`409 duplicate_trigger_tuple` for a manual trigger), so the API never answers
"triggered" for a cycle that will not run. See
[v3-complete-flow.md](v3-complete-flow.md) and the
[API notes](../magician-api/README.md#agents).

## Program context

A cycle's goal prompt carries its program deterministically (`magician-api`
`append_harness_program_context`). How much it gets depends on the agent:

- **Harness agents** (personal, with `harness:`) get the full harness context:
  the program, shared context, the Harness Progress Contract, approved
  supplemental guidance, runtime state, their open anomalies, harness health,
  the company backlog, repo state and live success metrics.
- **Program-only agents** (personal, *without* `harness:`, whose focus area
  names a `program:` — e.g. an outward-facing agent deliberately denied the
  harness toolset) get the program and shared context, and nothing else. They
  have no harness tools, so harness state would only be noise.
- Every other agent gets no program context.

Harness `inspect_agent` and `system_status` include `runtime_metadata`: saved versus
cached definitions, durable scheduler registrations and configured routing defaults,
limited to the same delegation/focus scope. Magican delegates company inspection to
Atlas instead of holding these tools directly. Explicitly granted non-harness personal
agents retain metadata-only inspection without tasks, episodes, goals or hidden agents.
Inspection reads persisted definitions without refreshing caches, materializing templates or granting control. It reports scoped definition
paths/hashes and cache load times, complete registration identities with schedule/pause
checks, and program source hashes with content-free cycle prompt-assembly receipts.
Missing historical receipts mean unknown. Optional `routing_context` resolves parent,
request-shape and run overrides through the dispatch resolver without calling a model;
unspecified future inputs and execution-time admission remain separate checks. Program
mission reviews use the same workspace definitions. Self-inspection grants no delegation.
Answer verification preserves inspected identities, HTML report rows and native-result
priority; typed tools exclude internal context fields from public argument parsing.
Generated task files declared with `artifact_path` or `output_path` are captured with
provenance/checksums and download references within existing filesystem boundaries.

Magican routes company coordination through the named `ceo` delegate. Atlas has
explicit department/reliability routes and permits sub-delegation when called by
Magican (`allow_transitive_delegation: true`, depth limit 2). Each specialist keeps
its own grants and coordination limits; program revisions still require owner
approval. This route does not enable the company loop or change its schedules.

The program is introduced as `Source: <path> (sha256 <hex>)`; the hash is of the
whole document, so an agent can propose a revision against exactly the version
it read.

This applies whether or not the agent declares a `prompt_pipeline`. The
assembled goal (focus text, program context, pipeline sections) becomes the
cycle task's description, and the model reads it once: the run's goal, and the
execution's runtime-context seed, whose `## Effective Goal` already is that
description (`## Description` is written only when the goal does not contain
it).

### Shared context

Some documents should reach every cycle, not only the agent whose own program
they are: positioning, facts, policies. List them once in
`magician-config.yaml`:

```yaml
harness:
  shared_program_docs: [positioning.md, facts.md]
```

Names are bare `*.md` program documents in each scope's `programs/` folder.
They are rendered after the cycle's own program as a `### Shared Context` block,
in list order, each as `#### <title> (<name>, sha256 <first 12 hex>)` followed by
its content. The cycle's own program is never repeated and duplicates are
dropped. Each document is capped at 24 KiB and the block at 64 KiB of document
content, truncated on a character boundary with a `[truncated: N bytes
omitted]` marker. A missing document is listed as missing and an invalid name
as skipped, so a typo stays visible. The list is read per cycle, so an edit
applies to the next cycle without a restart; the seed ships it empty.

A focus area can override the list:

```yaml
    - name: inbound sweep
      program: outward_charter.md
      shared_programs: [facts.md]   # replaces the global list; [] = none
```

Leaving `shared_programs` out uses the global list. The crew editors (web, iOS,
Android) offer Use global list / None / Choose documents per focus area: Use
global list removes the key, None writes `[]`, and Choose writes the checked
program names. A stored name that is not a program document is shown as not
found and can be removed.

### Proposing revisions

`propose_program_revision` lets an agent propose the next version of an
existing program document, its own or a shared one:
`{program, content (the full revised document), base_sha256, rationale,
evidence? (up to 10)}`. `base_sha256` is the hash shown in the cycle context
(the full hex, or the 12-character prefix shown for shared documents).

The main assistant template also grants this tool with an owner-approval rule,
so chat can propose program revisions without God mode or generic file access.

The tool is approval-gated by definition, not by code: an agent definition
grants it and adds a `requires_approval` rule for it, so each proposal waits for
the owner. Only after approval does the
handler save through `ProgramDocEditor::save_document`: the prior version is
snapshotted to `programs/.history/` (revertible from Work → Playbooks), and a
document that changed since the proposal is refused rather than overwritten.
The agent is then told to re-read and re-propose. Like any approval-rule act
without a known class, it classifies as a commitment and is never covered by a
standing envelope ([consequence classes](consequence-classes.md)). The tool
never creates a document.

The approval card is the tool's [approval preview](hitl-attention.md#approval-previews),
built when the pause is created and not by a model: "<agent> wants to change
`<program>`", the rationale labelled as the agent's words (plain text), the
change as `+N/−M` lines and only the changed hunks with two lines of context
(capped at 200 lines / 16 KiB, with a count of what was left out), and the
evidence. When `base_sha256` no longer matches the current document, the card
warns that approving will be refused and the agent should re-propose; a missing
document is said so too. The raw call stays on the card under "Raw call".

## Related

- [Recurring Monitors](recurring-monitors.md) — standing watches, the narrower cousin
- [Resurfacing](resurfacing.md) — what unattended work surfaces to Today
- [Agent templates](../../../magician_data_v3/system/agent_templates/README.md) — the full schema
