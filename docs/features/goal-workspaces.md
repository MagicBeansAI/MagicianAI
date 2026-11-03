# Goal workspaces

A **goal workspace** is one place to see an owner's goals, the sub-goals under
them, the facts behind them and their progress. It is built from things
Magician already has: notes, tasks, monitors, program docs and a dashboard. A
template puts them together. There is no goal store, goal API or goal tool.

Design and findings: goal workspaces design ·
plan: goal workspaces plan ·
UI: [Unified Work](../components/unified-ui/unified-work.md) ·
template: [`goal-workspace-startup`](../../skillshub/goal-workspace-startup/SKILL.md)

## What is in a workspace

Everything lives under `Workspaces/<workspace>/` in the notes space.

| Piece | What it is |
| --- | --- |
| Goal notes | One Markdown note per goal, with frontmatter (below), in one folder per main goal under `Goals/`. |
| `README.md` | `members:` in its frontmatter (who may see the workspace), the goal list, programs, metrics, the dashboard route and a change log. |
| `Facts.md`, `Do not do.md` | Verified facts with sources, and things agents must not do. |
| Metric tasks | Ordinary recurring tasks. Each run adds one row to `Metrics/<metric>.csv`. |
| State task | An ordinary daily task that writes the state dashboard. |
| Monitors | Ordinary [monitors](monitors.md) for pages that move a goal. |
| Dashboard | One dashboard published from the state task, pinned in Today. |

After setup, all of this is ordinary content. You can change any of it.

## Layout

A workspace can hold hundreds of main goals, so each main goal has its own
folder:

```text
Workspaces/<workspace>/Goals/
  Ship Picker v1/
    Ship Picker v1.md                 the main goal
    Pilot with three warehouses.md    its sub-goals, at any depth, side by side
    Pass safety certification.md
  Hire two field engineers/
    Hire two field engineers.md       a peer goal: main-level, its own folder
```

- Every goal under a main goal sits flat in its folder; a peer goal
  (`kind: peer`) is main-level. `parent:` places a goal in the tree; the folder
  only keeps files in order (a goal moved under another main goal moves too).
- Links use the goal's name only (`[[Ship Picker v1]]`), so names are unique
  per workspace.
- A goal with `status: done` or `dropped` is closed. Every goal list (the Goals
  screen, the All work card, the iOS and Android apps, the state dashboard)
  shows only open goals; the Goals screen's **Closed** list holds the closed
  ones.
- A dropped main goal moves whole to `Archive/Goals/`, which the Goals screen
  does not read. Older notes directly in `Goals/` still show; Magican can move
  them into folders.

## Goal notes

Each goal is a note named after the goal, in its main goal's folder (above).

```yaml
---
magician_kind: goal
workspace: Acme Robotics
kind: sub
parent: "[[Ship Picker v1]]"
peers: []
owner: cro
status: active
target: 2027-03-31
visibility: internal
program: "warehouse_pilots.md"
goal_id: harness:cro:warehouse-pilots
metrics: ["Signed pilot sites"]
---
```

| Field | Values |
| --- | --- |
| `magician_kind` | `goal`. Notes without it are skipped. |
| `workspace` | The workspace name. |
| `kind` | `main`, `sub` or `peer`. |
| `parent` | `"[[<parent goal>]]"`. None for a main goal. |
| `peers` | `["[[<goal>]]", ...]` or `[]`. |
| `owner` | `user` or an agent id. |
| `status` | `proposed`, `active`, `blocked`, `done` or `dropped`. |
| `target` | `YYYY-MM-DD`. Optional. |
| `closed` | `YYYY-MM-DD` the goal was marked done or dropped. Orders the Closed list; removed on reopen. Optional. |
| `visibility` | `private`, `internal` or `public` (see [Visibility](#visibility)). |
| `program` | The Playbook document that gives this goal ongoing direction. Optional; one bare runtime Program filename. |
| `goal_id` | `harness:<agent>:<focus-slug>`. Ties tasks to this goal. Optional. |
| `metrics` | Metric names, matching the metric tasks. |

The frontmatter must start on the first line and use flat fields only. The
body holds the why, the definition of done, and links to facts, don'ts and
related goals.

When a field is missing, the Goals screen uses a default:

| Missing field | Default |
| --- | --- |
| `kind` | `sub` when `parent` is set, otherwise `main` |
| `status` | `proposed` |
| `visibility` | `private` |
| Title | The first line of the body when it is an H1, otherwise the file name |

## Templates

A template is a procedure skill (`metadata.magician.skill_type: procedure`) in
`skillshub/`. It holds the recipe and the starter files: the goal-note layout,
the interview, the metric and state task prompts, the dashboard layout,
suggested monitors and program-doc stubs.

| Template | For |
| --- | --- |
| [`goal-workspace-startup`](../../skillshub/goal-workspace-startup/SKILL.md) | A company or startup. The worked example is Acme Robotics, a made-up robot maker. |

A template only seeds a workspace. Other templates (personal goals, job
search, a product launch, a research project) follow the same shape.

To save your own setup as a template, ask in chat. Magican writes a
procedure-skill draft to `Workspaces/<workspace>/Template draft.md`. The draft
keeps the shape and leaves out names, facts and values. Installing it as a
skill goes through the normal skill-authoring flow.

## Setting up and changing a workspace

You do this in chat. Magican (the personal assistant) runs the
`goal-workspace-startup` skill itself. Each goal note names its owning agent,
and that agent keeps its own goals current with the same skill and the
`files` tool.

Examples:

- "Set up my company workspace from the startup template."
- "Add a peer goal for hiring."
- "Make the state digest weekly on Mondays."
- "Drop the certification goal."
- "Rename Ship Picker v1 to Ship Picker 1.0." (moves the folder and the note,
  and changes every link to the old name)
- "Move my old goal notes into folders."

How setup goes:

1. Magican asks at most five questions: the workspace name, the main
   goals, their deadlines, the programs and focus areas behind each sub-goal,
   and what to measure. It reads every program doc you or the brief name, in
   full, and names any it could not read.
2. It shows the whole plan: members first, then files, tasks, monitors and
   the dashboard.
3. After you agree, it writes the notes, creates the tasks, previews the
   monitors and publishes the dashboard.

Rules the skill holds for whichever agent runs it:

- It writes only under `Workspaces/` in the notes space.
- It shows each change before making it. One yes covers only the change it
  showed. New monitors and dashboards are part of that change.
- It creates only ordinary tasks, monitors and dashboards. It does not change
  agents, programs or config. When a goal needs a program change, it tells
  you what to change.
- It never sends anything outward.

When it needs your decision it pauses and asks, in Work → Needs you and in the
chat that started the work, with choices such as Approve notes, Approve
everything and Change…. What it will write is under **Details** on the
question. Answering either place, or typing your answer in that chat,
continues the same run.

You can also edit the notes yourself at any time. The Goals screen reads them
as they are.

## Work → Goals

**Work → More → Goals** (`/work?view=goals`) shows the goal tree. **New folder**
groups goals; **New goal** creates a main goal or a sub-goal alongside its parent.
New goals are private, proposed and owned by you, with optional target date and
description. **Playbook (optional)** lists the scoped runtime Playbooks and saves
the selected filename in `program:` as part of the initial note. A failed catalog
read does not prevent creating a goal without a Playbook. Creation writes a complete
note without overwriting an existing file; linking a Playbook starts no tasks or
schedules and changes no crew assignment.

Goal rows name their linked **Playbook**. A successful catalog lookup that does
not contain the document shows **Playbook missing**; a failed lookup says the
Playbook could not be checked and offers Retry. A missing document and an
unavailable catalog are distinct. The catalog follows the current authenticated
scope, and an older scope's response cannot replace it.

The current note schema links one Playbook per goal. A main goal can use several
Playbooks through sub-goals, each with its own owner, `program:` and existing
`goal_id:`. A document being present does not prove a cycle consumed it or that
the goal is achieved. Keep outcomes, success criteria and actual evidence in the
goal notes; an unfilled brief or missing operating instructions remain setup blockers.

Click a row's date chip, or focus it and press Enter or Space, to open the native
calendar; × clears the date. Edits set or clear `target:` through
`PATCH /api/magician/v2/notes/front-matter`, which changes only that key and
leaves the rest of the note byte-for-byte. The edit carries the note revision
the screen read; if the note changed since (an agent or another device wrote
it), the server refuses with 409 and the screen reloads and says the date was
not saved. Everything else changes in chat or in the notes. The iOS and
Android apps show the tree under Work → More → Goals.

Only open goals are drawn and counted. Workspaces whose goals are all closed
appear only under Closed; empty workspaces remain under Open, and partially
read workspaces stay visible in both lists. **Mark done** on a row sets
`status: done` and `closed: <today>` through the same endpoint (with the same
409 reload); **Open N** / **Closed N** switch between the tree and the Closed
list (`/work?view=goals&goals=closed`). The Closed list shows each workspace's
done and dropped goals, most recently closed first (no `closed:` date last),
with the parent goal, the date and **Reopen**, which sets `status: active` and
removes `closed:`. An open goal under a closed one moves up to its nearest open
goal, or heads its own row, and says "Its parent goal “…” is closed."

The All work landing's Goals card shows 3 open main goals and reads less: per
workspace 6 main-goal folders one level deep (a closed main goal still takes a
folder), 3 notes from each (the main goal and 2 sub-goals) and 3 older notes
directly in `Goals/` (at most 7 listings and 21 reads). Its count shows `N+`
when it stopped early, and it reuses a read from the last minute until
**Refresh** or an edit.

- It lists each folder under `Workspaces/` as a workspace and reads the goal
  notes under its `Goals/`: each main goal's folder, and older notes directly
  in `Goals/` (`GET /api/magician/v2/notes/tree` one level per call,
  `GET /api/magician/v2/notes/file`, four requests at a time). A note without
  `magician_kind: goal` is skipped.
- Per workspace it reads up to 3 folder levels under `Goals/`, 1000 folders
  and 2000 notes, skips folders the notes API reports as empty, and says what
  it left out ("N more notes in Goals/ not shown", "N folders in Goals/ not
  read"). Notes or folders that could not be read are listed; the other goals
  still show.
- The tree comes from each note's `parent:`, not from its folder. Main goals
  are top-level rows, sub-goals sit under their parent, and peer goals are
  top-level rows marked **Peer goal**. The tree shows 8 levels; deeper goals
  are counted and reached through their note.
- **Not linked** holds goals that do not lead up to a main goal: the parent is
  not found, the goal names itself as its parent, or the parents form a loop.
  Their sub-goals come with them.

Each row shows:

| Item | Shows |
| --- | --- |
| Status | The `status` value. |
| Visibility | The `visibility` value, with its meaning on hover. |
| Owner | "you" for `user`; an agent id links to its Crew page. |
| Target | The `target` date; the chip sets or clears it (web). |
| Peers | Each peer, linked to its note when it is in the same workspace. |
| Metrics | The metric names. |
| Links | **Playbook** opens Work → Playbooks; **Note** opens the goal note. |

The header links to **Needs you**. With no workspaces, the screen says so and
links to chat.

## The state dashboard

The daily task `Workspace state: <workspace>` writes the state as Markdown:

- the tree of open goals with each goal's status and progress (closed goals
  are left out);
- a metrics table (latest value, the value 7 days ago, the goal);
- the last 7 days;
- needs you: tasks waiting on input and blocked goals.

Magican publishes it once with `create_dashboard`. It passes the task id,
no `source_output_id`, the route `/briefing/workspace-<workspace-slug>`,
`placement_kind: workspace` and `pinned: true`, so it is pinned in Today.

`create_dashboard` draws a Markdown output as one section. A JSON output
becomes up to 8 metric tiles plus one table. It does not draw a goal tree, so
the tree lives on the Goals screen.

**Check that it follows later runs.** After publishing, Magican runs the
task once more and asks you whether the dashboard's "Updated" time changed. If
it did not, it asks to republish: it unpublishes the old surface and publishes
again.

## Visibility

| Value | Who may use it |
| --- | --- |
| `private` | The owner, Magican and the goal's `owner` agent (when it is a member). Never in any outward draft or public page. |
| `internal` | Every member of the workspace; never stated outward. |
| `public` | Every member; may be stated outward. |

- Anything meant for someone else (an update to a partner, a public page)
  includes only `public` goals. Magican says which goals it left out.
- The workspace dashboard is for the owner and may show all goals.
- A goal note without `visibility` is treated as `private`.

## Who can see a workspace

A workspace is open only to Magican (`personal-assistant`, always a member)
and the agents listed in `members:` in the frontmatter of its `README.md`:

```markdown
---
members: [cmo, cro]
---
# Acme Robotics
```

Everyone else can neither read nor write anything in it, by any tool. Inside
it, goals follow [Visibility](#visibility) (`owner: user`: Magican only), and:

- only Magican changes `README.md` (so `members:`), and creates, renames or
  deletes a workspace, anything directly under `Workspaces/`, or a
  workspace's `Goals/` folder. A member cannot move or delete a folder that
  holds a private goal it may not touch;
- a missing or unparseable `README.md`, or one with no `members:`, leaves the
  workspace to Magican alone. A goal whose frontmatter does not parse is a
  private goal with no owner. Every Markdown note in a workspace, at any
  depth, is governed by its frontmatter.

**Delegation carries read access.** A delegate may read everything in the
workspaces (and private goals) its delegator may read, for that delegated
work, by every tool and inside its shell's OS box (opened read-only). It writes
there only if it is a member itself; a refused write says the access is
read-only through that delegator and suggests the run's work folder. Magican is
a member of every workspace, so whoever Magican delegates to may read them all.
Access is transitive: Magican → Vera → Muse lets Muse read what Magican or Vera
may read. A handover carries it too: the agent handed to reads what the handing
agent may, for the run the handover starts and what that run starts (giving
control back never hands reads upward). A read allowed this way logs
`[GOAL-WORKSPACES] read allowed through delegation`; a refused read names the
delegators that could not open it either.

Delegators come only from the run's own identity: the agents that delegated to
it within the run, and a delegated child's source as the server recorded and
checked it. The run reads this lineage (`read_lineage`) from its own execution
record at setup, never from a checkpoint: the verified source when a chat
delegation or handover starts it; the parent's lineage, suspended owners and
owner for a delegated child; the pipeline's owner for its stages and their
children. In the owner's VibeDev session (which acts as Magican), engineers
read as Magican. `read_lineage` counts for reads only: delegation depth and "no
transitive delegation" limits still see `delegation_chain` alone.

It applies to the file tools, the notes tools (open, search), shells,
harness and coding-engine boxes, and Plane-driven runs of a delegated agent.
Nothing carries to Work tasks, tasks an agent creates (the work outlives the
delegation), OPC missions and autonomous cycles (use `members:`), an app's
agent-tool children (an app is a trust boundary), or public or Envoy surfaces.
An agent you or Magican delegate to directly does carry the delegator's reads
even if it is public-facing (the ambassador), since such a delegation is always
deliberate.

Processes no OS box holds (governed CLIs, DuckDB) are judged for the acting
agent alone, since nothing would keep them read-only; to hand one a workspace
file, copy it into the run's work folder first (the copy is a delegated read).
On Linux a writable folder inside a workspace opened only to read stays
read-only.

To add members, ask Magican; every member change is shown before it is made.
An `owner` that is not a member cannot reach its own goal (logged as
`[GOAL-WORKSPACES]`). You are not fenced: the Goals screen, notes browser and
notes API show everything. God mode does not lift the fence.

Code: `magician/src/magician_v2/execution/goal_workspace_access.rs`. Config:
`execution.goal_workspace_access` (`enabled`, default `true`; `admin_agent`,
default `personal-assistant`; live-reloaded). Any `Workspaces/` folder of a
notes space, and any sandbox root named `Workspaces`, is fenced from startup.

### Where it is enforced

"Fenced" below means an agent that some workspace is closed to.

| Surface | Enforcement |
| --- | --- |
| Acting agent | Taken from the run, never from tool arguments: the active owner (a delegated agent acts as itself); a compiled dispatch can narrow it, never widen it; no agent is a non-member. An owner-started VibeDev session (login session, not an API token, bot or terminal grant) acts as Magican in its root frame only. |
| File tools | `files`, `read_file`, `write_file` / `edit_file` / `apply_patch` (at staging, and again at diff-approval apply for the staging agent), `grep`, `glob`: every path, checked on the descriptor actually opened. Listings drop what the agent may not read; a hard link to a hidden goal is that goal. |
| Notes tools | `search_notes` leaves hidden notes out and reports `withheld_count`; `open_note`, `create_note`, `append_note`, `save_selection_to_note` answer `notes_permission_denied`. Apps get no workspace content. Observe skips `Workspaces/`. The notes hybrid index is closed to every agent. |
| Shell, CLI skills, coding engines | OS box (`sandbox-exec` / `bwrap`) closes every `Workspaces/` folder and re-opens only member workspaces, so a workspace created later stays closed. No working box: no shell for a fenced agent. CLI path arguments into a hidden workspace are refused. Governed CLIs (not boxed) are refused when their working directory or an argument holds hidden content; for every non-admin the notes root does. A workspace-mode governed CLI with no admitted folder and no task work root (a content reader, a discovery adapter) runs in its own empty per-call lease, never the scope root, which holds other tasks' evidence folders. |
| Browser | Navigation to `http(s)`, `about:`, `data:`, `blob:` only; no `file:` while a workspace exists; no upload, state, screenshot, PDF or download path the agent may not use; options only Magician sets (executable, args, proxy, CDP, profile, session, config, extensions, …) refused in any form; `batch` only as a `commands` matrix. Fenced: public hosts only, its own `--fenced` session through the egress proxy's strict listener (none, no browser), no CDP mode, Lightpanda, `data:` / `blob:`, `read <url>` or daemon-admin commands. |
| Network | Local services (supervisor control port, Magicutor, CDP, any registered local notes server, Magician's API, the plane listener, UI dev servers) are denied in every box, the `http` tool and the egress proxy, on every address; dev servers stay open to Magican. `http` checks every redirect hop. Fenced: public hosts and `network_policy.allow_private` only (god mode cannot widen it), no unix socket but the box's own; on Linux a fenced shell or CLI gets no network. |
| Harness turns | Magician's tools come from a loopback listener that serves only `/plane/mcp`. Each turn's grant is bound to its own loopback channel and refused anywhere else; its private files live in a per-session folder other boxes cannot list. A coding run's citizen token is bound the same way. On Linux a fenced plane harness or VibeDev coding engine is refused. |
| DuckDB | Fenced: a fresh connection limited to the task folder, external access off, any link out of it refused; SQL naming `Workspaces` is refused. |
| Evidence | What an execution reads from a workspace is recorded (`goal_workspace_reads.json`, runtime root) through the shared atomic writer with file and parent-directory sync; its execution folder is then closed to agents that may not read those paths. An unreadable record fails closed for non-admins outside their own task folder. |

Known gaps: the Codex app-server harness keeps its own sandbox (unwrapped),
and Codex and Grok read their grant from an environment variable (useless off
its channel). A channel opened after a box was built is not in its deny list.
Governed CLIs are checked, not boxed. A hard link to a goal from outside the
workspaces is not hidden in boxes; DuckDB's link scan runs before the query.
Content copied into the shared task folder, and chat tool results outside an
execution, are not tracked as reads. Folder names under `Workspaces/` are
visible to the shell. On Linux `bwrap` hides only what exists at start, and
fenced harnesses wait on a private network namespace (`pasta` /
`slirp4netns`). Notes Observe ingested before the fence stay until removed.
Shell changes are seen within a second.

## Limits

- **The sandbox is global; membership is not.** Agent definitions cannot set
  their own file or shell roots, so adding `Workspaces/` to the sandbox opens
  the folder to every trusted agent's tools; `members:` and `visibility`
  then decide per agent. The service's working directory (`.`) also stays
  writable without approval.
- **The dashboard is Markdown** and may not follow later runs: the published
  record stores the resolved output id. Check it after the second run.
- **Magican is not a harness agent.** Run from chat, the skill cannot read
  program runtime state, proposals, approvals or anomalies. The state
  dashboard points to Work → Needs you and Work → Crew for those.
- **`create_task` sets no `goal_id`.** Only tasks from an agent's harness
  focus area carry one. Tasks the skill creates name their goal in the title
  or description.
- **The web and desktop Goals screen sets target dates and creates folders
  and goals** (Unified UI). iOS and Android show the same tree. The notes
  API reads one folder or note per call, so a workspace with hundreds of
  goals takes hundreds of requests.
- **Goal names must be unique in a workspace.** Links name a goal, not its
  folder; two notes with the same name link as the first one listed.

## Config

Agents' file and shell sandboxes allow the task workspace, `.` and `/tmp`.
The notes space is outside them, so a goal workspace needs one folder added.

| Setting | Add |
| --- | --- |
| `execution.file_sandbox.allowed_roots` | `<notes space path>/Workspaces` |
| `execution.shell_sandbox.allowed_working_dirs` | `<notes space path>/Workspaces` |

- Use the absolute path. The notes space path differs per machine. Read it
  from `space_path` in
  `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/notes/settings.json`,
  then add `/Workspaces`.
- Set it in the live runtime config,
  `$MAGICIAN_ROOT_DIR/magician-config.yaml`, on each machine. Create the
  folder if it is missing.
- Program docs and the rest of Notes are readable, not writable, through
  `execution.file_sandbox.read_only_roots`. List the scope's programs folder,
  `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/programs`, and the
  notes space path, both absolute. Agents then read program docs and notes
  with `read_file` / `files`, and a program changes only through its
  owner-approved program revision. `Workspaces/` stays writable inside the
  read-only notes space, because the deepest configured root decides; a
  folder the owner approves during a run never makes a read-only folder
  writable. Notes tools (`create_note`, `append_note`) and program
  revisions still write there. Without these entries the rest of Notes
  stays closed to agents.
- `file_sandbox.allowed_roots` governs the `files` tool and `shell`; the
  staged writers `write_file` and `edit_file` do not read it. They write
  only inside the task's working directory or a folder the owner approved
  during the run, so they refuse `Workspaces/`. The skill writes notes
  with `files` (`action: write`, the whole note) or `shell`.
- Reload with `POST /api/magician/v2/settings/magician-config/reload`. The
  reply names any section that needs a restart; if it names the sandbox,
  restart the service.
- Optional: turn on `task_publishing.auto_publish_completed` if task notes
  should feed the timeline.

If a `files` or `shell` write asks for approval or is refused, the folder is
missing from one of the two lists. Magican stops and tells you which folder it needs.
