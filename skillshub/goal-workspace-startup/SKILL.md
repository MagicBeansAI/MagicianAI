---
name: goal-workspace-startup
version: 0.4.1
description: Procedure skill — set up, review or change a goal workspace for a company or startup. A goal workspace is one place to see main goals, sub-goals, peer goals, facts, metrics and progress, assembled from ordinary goal notes under Workspaces/ in the notes space (one folder per main goal) plus ordinary tasks, monitors and one state dashboard. Covers the five-question interview, the goal folder layout, the exact goal-note frontmatter, the README's members list (who may see the workspace), mapping goals to programs and agent focus areas, metric tasks, the daily state dashboard, monitors, later changes, visibility, and saving a setup as a new template. Use it whenever the user asks to set up, review or change a goal workspace.
metadata:
  magician:
    skill_type: procedure
---

# Goal workspace: startup template

A goal workspace puts an owner's goals, the facts behind them and their
progress in one place. Nothing in it is special storage. It is:

- goal notes (markdown with frontmatter) under `Workspaces/<ws>/Goals/`, one
  folder per main goal;
- a README whose frontmatter lists the workspace's `members:`, a facts note
  and a do-not-do note beside them;
- ordinary recurring tasks for metrics and for the daily state run;
- monitors for pages that move a goal;
- one dashboard published from the state task.

**Work → Goals** in the app reads the goal notes and draws the tree. Everything
this template creates is a seed. The owner can change any of it later, and so
can you, through the steps in "Changing a workspace later".

Rules for the whole procedure, whichever agent runs it:

- Write only under `Workspaces/` in the notes space. Do not create, edit or
  delete anything else in the notes space or on disk; scratch files go in the
  task working directory or `/tmp`.
- Write and change notes with the `files` tool (see section 2). A refusal
  from `write_file` or `edit_file` under `Workspaces/` is not a reason to ask
  the owner for access.
- Create only ordinary tasks, monitors and dashboards. Do not change agents,
  programs or config; when a goal needs a program change, tell the owner what
  to change and where.
- Never send anything outward: no email, messages, posts, form submissions or
  uploads. Goals marked `private` or `internal` never appear in anything meant
  for someone else.
- Read every source the owner or the brief names, in full, before you propose
  anything from it (section 0, "Reading program docs and notes").
- Show the owner each change before you make it, and make it after they agree:
  the file and its content, the task and its schedule, the monitor contract or
  the dashboard route. One agreement covers the change you showed, not later
  ones. Ask the way section 1 says, with the whole proposal in the pause's
  `hint`.
- Settle everything the write depends on (the notes root, conflicting files)
  before you ask, so one approval covers the write.
- After writing a file, read it back and report the path you actually wrote.
- Inside a scheduled task this template created ("Metric: ...", "Workspace
  state: ..."), follow that task's description: the owner agreed to those
  steps when the task was created. Anything beyond them still needs a yes.
- Each goal note names its owning agent. When you run as that agent, keep
  your own goals current; change another agent's goal only when the owner
  asks.
- Only members see a workspace (section 8, "Who can see a workspace").
  Magican (`personal-assistant`) is always a member and is the only agent that
  writes the README, adds or removes members, or creates, renames or deletes
  a workspace or its `Goals/` folder. Magican runs setup and every structural
  change; an owning agent only keeps its own goal notes current.

## 0. Find the notes space

The notes API, and the Goals screen, read paths relative to the notes space.
The file tools need the absolute path.

1. Call `search_notes` with a word the owner certainly has in a note (try
   `Workspaces` first, then any title they mention). Take any hit's `path`.
2. Call `open_note` with that `path`. It returns `absolute_path`. The notes
   space root is `absolute_path` with the `path` suffix removed.
3. Fallback, when the space has no notes yet: create a placeholder note with
   `create_note` (`title: Start here`, `body: "# <ws>"`,
   `target_dir: Inbox`), then read `absolute_path` from its response.
   `create_note` adds its own frontmatter (`magician_kind: note`) and
   slugifies the file name, so never use it for goal notes or the README:
   write those with `files`, after the owner approves (section 2).

A root derived from `open_note` on an existing note is the notes space the
Goals screen reads; take it as is. Only a note write (`create_note`,
`append_note`) reports `used_fallback`. When a write reports `true`, the note
landed in the fallback provider, not that notes space: stop and tell the owner
where it went and why (`fallback_reason`).

Say the root you found to the owner once, in plain words: "the folder
`Workspaces/` at the root of your notes space".

Writing there needs the folder in the file and shell sandboxes, which the
`files` tool and `shell` follow. If a `files` or `shell` write asks for
approval or is refused, stop. Tell the owner which folder you need and why, and
let them approve it. Never write the file somewhere else.

### Reading program docs and notes

The file sandbox's read-only roots let you read the owner's program docs and
the rest of the notes space, but never change them.

- Program docs are the `*.md` files in
  `<MAGICIAN_ROOT_DIR>/scopes/<principal>/<workspace>/programs/`: the
  read-only root whose path ends in `/programs`. The brief from chat may name
  it. List that folder with `files` (`action: list`) and read a doc with
  `read_file` or `files` (`action: read`), not `shell`: shell output is cut
  short. Read every doc the owner or the brief names, in full, before you
  propose anything from it; name any you could not read in the proposal.
- Do not guess at other paths. A path outside the sandbox asks the owner for
  approval, and a delegated run cannot wait for the answer. If you cannot
  find the folder, say so in your answer.
- Never write, edit, move or delete a program doc. A program changes only
  through the owning agent's program revision, which the owner approves.
  Propose the change in your answer instead.
- Notes outside `Workspaces/` are read-only too. Read them with `open_note`
  or `read_file`.

## 1. Interview

Ask the owner by pausing with `need_user_input`; the owner answers from
Work → Needs you or the card in chat, and the same run continues. Use
`input_type: "text"` (or a `form` of up to 3 questions) for the interview and
`input_type: "choice"` for an approval. For every write, put what you will
create or change in the pause's `hint`, as Markdown (the goal tree, each file
with its path and key fields, every move as old path → new path, then tasks, monitors and the dashboard), and keep the
`question` to one line that names the write, for example "Approve the Magican
goal tree and write 15 notes?". The owner reads the hint under "Details" on
the chat card and in Work → Needs you; nothing else you write before the
answer reaches them, so a file you wrote is no substitute. Every approval ask
carries the whole current proposal in `hint`, including a revised one after
`change`. Ask with options such as `approve_notes` ("Approve
notes"), `approve_all` ("Approve everything") and `change` ("Change…"); on
`change`, ask once more with `input_type: "text"`. When the brief says to ask
before writing, the approval is approval to write: offer the write and make it
on a yes, rather than asking the owner to approve a proposal that changes
nothing. Never treat the brief itself as a yes unless it quotes the owner.
When the brief already answers a question, do not ask it again.

Ask at most five questions, in one message:

1. **Workspace name** (for example the company name).
2. **Main goals**: one to three outcomes, each in one line.
3. **Deadline** for each main goal, if there is one.
4. **Existing programs**: for each sub-goal, the agent id and the focus-area
   name that works on it, and the program document's name. Take the agent
   id and focus area from the owner's answer or from what chat's brief
   names; you cannot look them up yourself. You can check a program
   document's name by listing the program docs (section 0).
5. **What to measure**: one to four numbers that show progress.

Everything after the interview is a default you state and the owner may
change: sub-goals, owners, visibility (`internal` unless the owner says
otherwise), the members, the metric cadence (weekly), the state hour (07:00
in the owner's time zone).

**Members.** Propose the workspace's `members:` from the goals' owners: every
agent id that owns a goal (not `user`), deduplicated. Magican is always a
member and is not listed. Show the list on its own line in the plan
("Members — agents that may see this workspace: cro, cpo") and ask the owner
to approve or change it; an agent left out cannot see the workspace, not even
its own goals. Never add an agent the owner did not approve.

Then show the whole plan in the approval's `hint` as a short list, the members
first, then files, tasks, monitors and the dashboard, and wait for a yes.

## 2. Files to create

Write goal notes with the `files` tool (`action: write` with the whole note,
or `append`), or `shell` with a quoted heredoc, never with `create_note`, at
`<notes root>/Workspaces/<ws>/...`. To change a note, write it whole again.
`write_file` and `edit_file` reach only the task working directory and an
owner-approved folder, so they refuse `Workspaces/`; that refusal is not a
reason to ask the owner for access. Quote every path: names have spaces. Read
each file back with `read_file` and report the path you actually wrote.

### Layout: one folder per main goal

A workspace can hold hundreds of main goals, so each main goal gets its own
folder, and every goal under it lives flat in that folder:

```text
Workspaces/<ws>/
  README.md
  Facts.md
  Do not do.md
  Goals/
    <Main goal>/
      <Main goal>.md          the main goal note
      <Sub-goal>.md           every goal under it, at any depth, side by side
      <Sub-sub-goal>.md
    <Peer goal>/
      <Peer goal>.md          a peer goal is main-level: its own folder
  Metrics/
  Archive/Goals/<Main goal>/  dropped main goals, moved here whole
```

- The folder name and the main goal's file name are the main goal's name:
  `Goals/Ship Picker v1/Ship Picker v1.md`.
- A sub-goal's file name is its name, in the folder of the main goal it leads
  up to, even when its parent is another sub-goal:
  `Goals/Ship Picker v1/Pass safety certification.md`. Do not make folders
  for sub-goals.
- A peer goal (`kind: peer`, no parent) is main-level and gets its own folder.
- The `parent:` field, not the folder, places a goal in the tree. The folder
  only keeps the files in order; when you change `parent:`, move the file too
  (section 7).
- Links use the goal's name only, never the folder: `[[Ship Picker v1]]`.
  So a goal's name must be unique in the whole workspace, and must not be the
  name of another note there (`README`, `Facts`, `Do not do`).
- A goal name cannot contain `/`, `\`, `#`, `|`, `[` or `]`, and does not
  start with `.`.
- Older workspaces may have goal notes directly in `Goals/`. The Goals screen
  still reads them; offer to move them into folders (section 7).

### Creating goals

1. Check that every new name is free in the workspace. Run
   `find "<notes root>/Workspaces/<ws>" -iname "<name>.md"` with `shell`
   (case does not matter: links match without it). It must print nothing. If
   a name is taken, propose another name before you ask for approval.
2. For each main or peer goal, create its folder with `files`
   (`action: mkdir`, `path: "<notes root>/Workspaces/<ws>/Goals/<Main goal>"`),
   then write `<Main goal>.md` inside it.
3. Write each sub-goal into the folder of the main goal it leads up to:
   `Goals/<Main goal>/<Sub-goal>.md`. A sub-goal of a sub-goal goes in the
   same folder.
4. Read every file back and report the paths you wrote.

### `Workspaces/<ws>/Goals/<Main goal>/<goal>.md`

The frontmatter must start on the first line, with flat fields only. The Goals
screen reads exactly these keys:

```markdown
---
magician_kind: goal
workspace: <ws>
kind: main
owner: user
status: active
target: 2027-06-30
visibility: internal
program: "<program doc name>"
goal_id: harness:<agent>:<focus-slug>
peers: []
metrics: ["<metric name>"]
---

# <Goal title>

## Why
<one paragraph>

## Done when
- <observable condition>

## Links
- Facts: [[Facts]]
- Do not: [[Do not do]]
```

Field values:

| Field | Values |
| --- | --- |
| `kind` | `main`, `sub` or `peer` |
| `parent` | `"[[<parent goal>]]"`. Required for `sub`; leave it out for a main or peer goal. |
| `peers` | `["[[<goal>]]", ...]` or `[]` |
| `owner` | `user` or an agent id |
| `status` | `proposed`, `active`, `blocked`, `done` or `dropped`. `done` and `dropped` goals are closed: the Goals screens list them only under Closed. |
| `target` | `YYYY-MM-DD`; optional |
| `closed` | `YYYY-MM-DD` the goal was marked done or dropped; set it when you close a goal, remove it when you reopen one |
| `visibility` | `private`, `internal` or `public` (see section 8) |
| `program` | program document name; optional |
| `goal_id` | `harness:<agent>:<focus-slug>`; optional (see section 3) |
| `metrics` | metric names, matching the metric tasks |

### `Workspaces/<ws>/README.md`

The frontmatter must start on the first line and hold `members:`, the agent
ids the owner approved (a YAML list; Magican is always a member and is not
listed). Without valid `members:` only Magican can reach the workspace.

```markdown
---
members: [<agent id>, <agent id>]
---
# <ws>

Goal workspace. Goals live in `Goals/`, one folder per main goal; Work → Goals
shows the tree.

## Goals
- [[<main goal>]]
  - [[<sub-goal>]]
- Peer: [[<peer goal>]]

## Facts and rules
- [[Facts]] — what is true and verified, with sources.
- [[Do not do]] — things agents must not do for this workspace.

## Programs
- <program doc name> — <agent id>, focus area "<focus area>" → [[<goal>]]

## Metrics
- <metric name> — `Metrics/<metric-slug>.csv`, task "<task title>"

## Dashboard
- Workspace state: <ws> — route `/briefing/workspace-<ws-slug>`
- Daily task "Workspace state: <ws>" at <HH:MM> <time zone>

## Change log
- <YYYY-MM-DD> created from the goal-workspace-startup template
```

Also create `Facts.md` and `Do not do.md` with a heading and the facts or rules
the owner gave in the interview, one bullet each. Leave them nearly empty
rather than invent content.

## 3. Mapping goals to work

- Each sub-goal names the program that plans it (`program:`) and the agent
  focus area that works on it.
- Set `goal_id: harness:<agent>:<focus-slug>`, from the agent id and
  focus-area name the owner gave. `<focus-slug>` is the focus-area name in
  lowercase, with every character that is not an ASCII letter or digit turned
  into `-`, runs of `-` collapsed into one, leading and trailing `-` removed,
  and, if longer than 64 characters, cut back to the last `-` within the first 64; an empty slug becomes `unnamed` (`warehouse pilots` →
  `warehouse-pilots`, `Q3: EU launch!` → `q3-eu-launch`).
- Only tasks that the agent's harness focus area creates carry this
  `goal_id`. Tasks you create do not (see section 4), so the goal's own tasks
  are found by the goal named in their title or description.
- If the owner names no focus area, leave `goal_id` out and say which agent
  would need one. You do not change agents or programs.

## 4. Metrics

For each metric, create one recurring task with `create_task`:

- `title`: `Metric: <metric name> (<ws>)`;
- `schedule`: weekly by default, for example
  `{"kind": {"Cron": {"expression": "0 6 * * 1", "timezone": "<IANA tz>"}}}`;
- `description`: the goal it serves (with its `goal_id`, when it has one), how
  to get the number, and these steps:
  1. Work out the current value from the named source.
  2. Append one row to `<notes root>/Workspaces/<ws>/Metrics/<metric-slug>.csv`
     with header `date,value,note`; create the file with the header when it is
     missing.
  3. End the output with `Latest: <value> (<date>)`.

`create_task` decides the owning agent and the output mode itself and sets no
`goal_id`, so do not pass `agent_id`, `output_mode` or `goal_id`: put
everything the run needs in `description`, and name the goal there. Only tasks
created by an agent's harness focus area carry a `goal_id`.

A CSV through `shell` needs no other skill. If the owner wants a Google Sheet
or an xlsx file instead, say that the run needs an agent holding `sheets`,
`presto-sheets` or `office-excel`, and let the owner decide.

## 5. The state dashboard

Create one daily task with `create_task`:

- `title`: `Workspace state: <ws>`;
- `schedule`: ask the owner the hour; default 07:00 in their time zone:
  `{"kind": {"Cron": {"expression": "0 7 * * *", "timezone": "<IANA tz>"}}}`.

Each run, as written in the task's `description`:

1. Reads every goal note under `Workspaces/<ws>/Goals/`: the notes in each
   main goal's folder, and any notes directly in `Goals/`. List the folders
   with `files` (`action: list`); `Archive/` is not part of the state. Leave
   out closed goals (`done`, `dropped`); an open goal under a closed one goes
   under its nearest open goal, or at the top level.
2. Reads the last line of each metric CSV and the latest metric task outputs
   (`list_tasks`, `get_task_details`).
3. Lists the workspace's tasks that are waiting on input, and the goals
   marked `blocked`.
4. Writes this fixed line under "Needs you", because the run cannot read
   approvals, anomalies or program state: `Needs you: see Work → Needs you;
   program progress: see Work → Crew`.
5. Writes the state as its final output, in the Markdown layout below.

### What `create_dashboard` can draw

`create_dashboard` turns the task's latest output into dashboard components:

- Markdown output → one `Markdown` section. This renders everything below,
  including tables. **Use this by default.**
- JSON output → top-level scalar fields become `MetricCard` tiles (at most 8),
  and the first array of objects becomes one `Table` (at most 20 rows and 8
  columns). Nothing else in the JSON is drawn.

`Tree` and `ActivityFeed` are real renderer components, but `create_dashboard`
does not build them from a task output today. Do not emit a component tree and
expect it to draw. For reference only, the renderer's node shape is:

```json
{
  "id": "goals",
  "component_type": "Tree",
  "props": {
    "nodes": [
      { "id": "ship-picker-v1", "label": "Ship Picker v1", "status": "active",
        "children": [
          { "id": "safety-cert", "label": "Pass safety certification", "status": "failed" }
        ] }
    ]
  }
}
```

The Tree status words it colours are `active`, `completed`, `failed`,
`pending` and `timeout`. Map goal statuses this way: active → `active`, blocked
→ `failed`, proposed → `pending`. Closed goals (done, dropped) are not drawn.

Default Markdown output for each run:

```markdown
# Workspace state: <ws>
_Updated <YYYY-MM-DD HH:MM> <time zone>_

## Goals
- **[active] <main goal>** · target <date> · owner <owner>
  - [blocked] <sub-goal> · <one-line reason>
  - [active] <sub-goal> · <progress in words>
- [proposed] <peer goal> · peer of <main goal>

## Metrics
| Metric | Latest | 7 days ago | Goal |
| --- | --- | --- | --- |
| <metric> | <value> | <value> | <goal> |

## Last 7 days
- <date> · <what happened> · <task title>

## Needs you
Needs you: see Work → Needs you; program progress: see Work → Crew

| Item | Goal | Since | Where |
| --- | --- | --- | --- |
| <task waiting on input, or blocked goal> | <goal> | <date> | <task title> |
```

If the owner prefers tiles, use the JSON form instead: put each metric as a
top-level field (`"Signed pilots": "1 of 3"`) and the needs-you list as the
first array (`"needs_you": [{"item": "...", "goal": "...", "since": "..."}]`).
Tell them the goal tree then appears only in Work → Goals.

### Publishing

1. Run the task once (`run_task`) and wait until `get_task_details` shows the
   run finished.
2. Show the owner the route and title, then call `create_dashboard` with
   `task_id`, no `source_output_id`, `route: /briefing/workspace-<ws-slug>`,
   `title: Workspace state: <ws>`, `placement_kind: workspace`,
   `pinned: true`.
3. No tool reads a published dashboard, so check with the owner: `run_task`
   once more, wait until it finishes, and ask the owner to confirm that the
   dashboard's "Updated" time changed. If it did not, ask for a yes to
   republish; with it, `unpublish_dashboard` the old `surface_id` and call
   `create_dashboard` again with the same arguments. Note the new
   `surface_id` in the README.

## 6. Monitors

Offer a monitor for each page that moves a goal: a deadline page, a public
counter, a launch or listing page. For each one the owner wants:

1. `preview_monitor` with `objective` in the owner's words, `urls` (a list,
   for example `["https://lab.example/booking"]`) and `schedule`, for example
   `{"kind": {"Cron": {"expression": "0 8 * * *", "timezone": "<IANA tz>"}}}`.
2. Show the returned contract as a short card: objective, sources, schedule,
   strictness, notification policy.
3. After the owner confirms, `create_monitor` with the same arguments and the
   `preview_fingerprint`.
4. Add it to the README under the goal it serves.

## 7. Changing a workspace later

Change the real notes, tasks, monitors and dashboard, never this skill. For
every change, show the owner the change first: the file diff, or the old and
new values. Then make it, read it back and add one line to the README change
log.

| Request | What to change |
| --- | --- |
| Add a main or peer goal | Check the name is free (section 2), create `Goals/<Main goal>/` and write `<Main goal>.md` in it; add it to the README. |
| Add a sub-goal | Check the name is free; write it in the folder of the main goal it leads up to; add it to the README. |
| Mark a goal done | Set `status: done` and `closed: <today>`; keep the file where it is. Its open sub-goals stay open. |
| Reopen a closed goal | Set `status: active` (or the status the owner names) and remove `closed:`. |
| Drop a sub-goal | Set `status: dropped` and `closed: <today>`; keep the file where it is. Delete only when the owner asks. |
| Drop a main goal | Ask which of its sub-goals to drop with it and which to keep. Re-parent the ones to keep first (below). Set `status: dropped` and `closed: <today>` on the main goal and the dropped sub-goals, then move its folder whole to `Workspaces/<ws>/Archive/Goals/<Main goal>`. Delete only when the owner asks. |
| Restore an archived goal | Move its folder back to `Goals/`, set a new `status` and remove `closed:`. |
| Re-parent a sub-goal under the same main goal | Edit `parent:`; check the new parent exists. The file stays. |
| Re-parent a sub-goal under another main goal | Edit `parent:`, then move its file, and the file of every goal under it, into the new main goal's folder. |
| Make a sub-goal a main goal | Remove `parent:`, set `kind: main`, create `Goals/<Name>/`, then move its file, and every goal under it, there. |
| Make a main goal a sub-goal | Set `kind: sub` and `parent:`, then move every note in its folder into the new main goal's folder and remove the emptied folder. |
| Rename a goal | Check the new name is free. Move the file to `<New name>.md` (for a main goal, move the folder to `Goals/<New name>` first, then the note inside it), change its H1, and change every link to the old name (below). |
| Add or remove a peer | Edit `peers:` on both goals. |
| Move legacy notes into folders | For notes directly in `Goals/`: give each main and peer goal its folder and move its note in; move each sub-goal into its main goal's folder. Show all the moves as one proposal. Links do not change. |
| Change the state hour or a metric cadence | `update_task` with a new `schedule`. |
| Change a monitor | `update_monitor`. |
| Remove a dashboard tile | Edit the state task's `description` (`update_task`); the next run drops it. |
| Add a metric | Section 4, then add the name to the goal's `metrics:`. |
| Add or remove a member | Magican only. Show the old and new `members:` list and say what the change opens or closes ("cmo will see every internal goal"). After the yes, write the README whole with the new frontmatter, and log it. |
| Give a goal to an agent that is not a member | Propose adding the agent to `members:` in the same change; without it the agent cannot reach the goal. |

Moving files and folders:

- Move with `files` (`action: move`, `source` and `destination`, both
  absolute and quoted). It moves a file or a whole folder and creates missing
  folders on the way.
- A move replaces a file already at the destination without asking. Check
  first with `files` (`action: exists`) that nothing is there; if something
  is, stop and show the owner both paths.
- Show every move in the proposal, old path → new path, with the field
  changes. Edit the frontmatter first, then move, then read each note back at
  its new path and check the old path is gone (`action: exists`).
- Remove a folder only after `files` (`action: list`) shows it empty, with
  `action: delete`, `recursive: true`. An empty folder left behind is
  harmless; never delete a folder that still holds anything.

Changing links after a rename: find every note that links the old name with
`shell`, for example
`grep -rlF -e "[[<old>]]" -e "[[<old>|" -e "[[<old>#" "<notes root>/Workspaces/<ws>"`,
and change `parent:`, `peers:`, the README and the body links to the new name.
Show each changed note in the proposal.

For a small edit to an existing note, read it first and write it whole again
with `files`, keeping the text the owner wrote around your change.

## 8. Visibility

| Value | Who may use it |
| --- | --- |
| `private` | The owner, Magican and the goal's `owner` agent (when it is a member); never in any outward draft or public page. |
| `internal` | Every member of the workspace; never stated outward. |
| `public` | Every member; may be stated outward. |

### Who can see a workspace

Magican enforces this for every agent and every tool:

- Only Magican and the README's `members:` reach the workspace; other agents
  get a refusal, and `search_notes` leaves it out (`withheld_count`).
- A private goal (or one with no `visibility`) is open only to Magican and its
  `owner` agent, if that agent is a member. `owner: user` means Magican only.
  No member can move or delete a folder holding a private goal it may not
  touch.
- A missing or unparseable README, or one with no `members:`, leaves the
  workspace to Magican alone; a goal note whose frontmatter does not parse is
  private with no owner. Keep both valid YAML and read the README back.
- A refusal of this kind is the rule working, not a missing folder: do not
  ask for sandbox access. Tell the owner which agent would need to be a
  member, and propose the change (section 7).

- `private` and `internal` goals never appear in an outward draft, a public
  page or anything meant for someone other than the owner.
- When the owner asks for something meant for others (an update to a partner,
  a public page), include only `public` goals and say which ones you left out.
- A dashboard placed in the workspace is for the owner; it may show all goals.

## 9. Saving as a template

When the owner asks to save their setup as a template:

1. Copy the shape, not the content: the folder layout, the frontmatter
   fields, the questions you asked, the metric and state task descriptions
   with names replaced by `<placeholders>`, and the dashboard layout.
2. Write it as a procedure-skill draft at
   `Workspaces/<ws>/Template draft.md`, with the same frontmatter as this
   skill (`metadata.magician.skill_type: procedure`) and a new `name`.
3. Tell the owner the draft is ready for review, and that installing it as a
   skill goes through the skill-authoring flow. You do not install skills.

Never copy goal names, facts, metric values or people into the draft.

## Worked example: Acme Robotics

Acme Robotics (a made-up company) builds a warehouse picking robot. The
interview answers:

1. Workspace: `Acme Robotics`.
2. Main goal: ship Picker v1.
3. Deadline: 2027-06-30.
4. Programs: `warehouse_pilots.md` (agent `cro`, focus area "warehouse
   pilots"), `certification.md` (agent `cpo`, focus area "safety
   certification").
5. Measure: signed pilot sites, open safety findings.

The plan shown to the owner:

- Members — agents that may see this workspace: `cro`, `cpo` (the goals'
  owners; Magican always). README frontmatter: `members: [cro, cpo]`
- `Workspaces/Acme Robotics/README.md`, `Facts.md`, `Do not do.md`
- `Goals/Ship Picker v1/Ship Picker v1.md` (main)
- `Goals/Ship Picker v1/Pilot with three warehouses.md` (sub)
- `Goals/Ship Picker v1/Pass safety certification.md` (sub)
- `Goals/Hire two field engineers/Hire two field engineers.md` (peer of Ship
  Picker v1, proposed)
- Tasks: `Metric: Signed pilot sites (Acme Robotics)` and
  `Metric: Open safety findings (Acme Robotics)`, Mondays 06:00;
  `Workspace state: Acme Robotics`, daily 07:00 Europe/Berlin
- Monitor: the test lab's public booking calendar page, daily
- Dashboard: `/briefing/workspace-acme-robotics`, pinned

`Goals/Ship Picker v1/Pilot with three warehouses.md`:

```markdown
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

# Pilot with three warehouses

## Why
Three running pilots are the evidence the v1 launch needs.

## Done when
- Three sites have signed pilot agreements and a robot on the floor.

## Links
- Parent: [[Ship Picker v1]]
- Facts: [[Facts]]
```

`Goals/Hire two field engineers/Hire two field engineers.md` uses `kind: peer`,
`peers: ["[[Ship Picker v1]]"]`, no `parent`, `owner: user`,
`status: proposed`, `visibility: private`. It shows beside the main goal on the
Goals screen and never appears in anything sent to a pilot site.

A run of `Workspace state: Acme Robotics` ends with:

```markdown
# Workspace state: Acme Robotics
_Updated 2027-01-12 07:00 Europe/Berlin_

## Goals
- **[active] Ship Picker v1** · target 2027-06-30 · owner user
  - [active] Pilot with three warehouses · 1 of 3 signed
  - [blocked] Pass safety certification · waiting for a test lab slot
- [proposed] Hire two field engineers · peer of Ship Picker v1

## Metrics
| Metric | Latest | 7 days ago | Goal |
| --- | --- | --- | --- |
| Signed pilot sites | 1 | 1 | Pilot with three warehouses |
| Open safety findings | 4 | 6 | Pass safety certification |

## Last 7 days
- 2027-01-11 · open safety findings fell from 6 to 4 · Metric: Open safety findings (Acme Robotics)
- 2027-01-09 · test lab calendar page changed · Monitor: test lab calendar

## Needs you
Needs you: see Work → Needs you; program progress: see Work → Crew

| Item | Goal | Since | Where |
| --- | --- | --- | --- |
| Goal blocked: waiting for a test lab slot | Pass safety certification | 2027-01-09 | Goals/Ship Picker v1/Pass safety certification.md |
| Task waiting on input: which pilot contact to use | Pilot with three warehouses | 2027-01-10 | Metric: Signed pilot sites (Acme Robotics) |
```

Later the owner says "make the state digest weekly on Mondays". Show the old
and new schedule (`0 7 * * *` → `0 7 * * 1`), call `update_task` after the yes,
and add `- 2027-01-12 state task moved to Mondays 07:00` to the README change
log.

Later still the owner says "put certification under the pilots goal". Both sit
under Ship Picker v1, so only `parent:` changes, to
`"[[Pilot with three warehouses]]"`, and the file stays in
`Goals/Ship Picker v1/`. Had they asked to make certification a main goal of
its own, the proposal would also show the move
`Goals/Ship Picker v1/Pass safety certification.md` →
`Goals/Pass safety certification/Pass safety certification.md`.
