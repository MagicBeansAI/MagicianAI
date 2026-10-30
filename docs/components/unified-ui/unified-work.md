# Unified Work

Unified UI 0.1.42 adds paged scheduling/delivery follow-through and keeps the CLI Agent drawer aligned with the shared shell dimensions.

Status: implemented for web, native iOS and Android. Design:
implementation plan.
Checks and limits: [qualification record](unified-work-qualification.md).
Inventory, route retirement, specialist integration and native parity:
complete migration plan.

`/work` composes the existing task, Crew, program, Attention, quality and
Team routines surfaces over their APIs. It adds no task identity, persistence
store, execution runner or authority model. `program.md` remains durable
direction; Relentless remains bounded improvement of one artifact revision.

## Navigation and detail

Primary views: All work, Needs you, Crew. Under More: Goals, Playbooks, Quality
checks and Team routines. Tasks, Monitors and System work filter All work.

Task links use `/work?selected=<task_id>` (with `execution_id` for an exact run).
Saved `/tasks/<task_id>` and thread-scoped task links resolve to Work, preserving
run/plan focus, repeated query values and fragments; API and artifact paths stay separate.

### Work vocabulary

Web, iOS and Android use the same purpose-oriented vocabulary:

| Display name | Purpose | Internal contract |
| --- | --- | --- |
| Goals | An owner's goals, the sub-goals under them and who owns each | Goal workspace notes under `Workspaces/*/Goals/`, `/work?view=goals` |
| Playbooks | Reusable goals, instructions and success checks for ongoing work | Programs, `program.md`, `programs` routes |
| Autopilot | A crew member uses its playbook to choose useful work | Member `harness` definition |
| Scheduled check-ins | Periodic reviews of a member's responsibilities, bounded by task and step limits | `autonomous_config` and work cycles |
| Team routines | Shared schedule and manual sequence for company roles | Company loop and `/harness/runtime` |
| Quality checks | Review and improve an output against criteria before accepting it | Relentless runs and reviews |
| System work | Background jobs and crew cycles | Internal task feed |

Info buttons explain these concepts, Goals, Crew, focus areas, delegation,
tasks and monitors. Playbooks provide ongoing direction; procedure skills
provide reusable steps. Web help opens on hover, focus or click and dismisses with Escape or
an outside click; phone clients expose separate tap targets and themed popovers.
Reading help never changes a setting or starts work. Copy changes do not rename
stored keys, URLs, IDs, document filenames or permissions.

**Needs you** merges canonical questions with cached feed projections. A
delivered pending question hides a stale question-less warning for the same
principal, workspace and execution; nothing else is hidden, and stale or failed
requests hide nothing. Local review routes in the request context open the review
surface, for live and restored requests alike; external, protocol-relative and malformed destinations are rejected.
Opening a review or dismissing its notice approves nothing. The task drawer also
answers every pending ask for its task (`src/lib/attention/pendingAsks.ts`), and
Quality run detail shows them in place (`ComposerAskDock`), with a `Review →`
link when the ask has a review route.

A pending `source: "agentic"` `choice` row in Needs you shows its answers as
buttons, one per option id (`attentionQuickChoices` in
`src/lib/attention/model.ts`). A row whose ask carries details (a hint on
several lines or over 200 characters, `isDetailedHint`) offers no buttons: the
row is a button and cannot show them, so it reads "Open to read the details
before answering." and the prompt it opens shows them under "Details". A confirmation gets Approve / Deny only on an
allow-list: explicit `destructive: false`, no `action_type` (an agentic tool
action), not `protected`, no preview. One click posts the same
`/api/magician/v2/hitl/{cid}/respond` body the prompt would
(`chooseAttentionRowOption` in `controller.ts`); the clicked button keeps its
name with `aria-busy` while it posts, the row drops on success and stays, with
the error above the list, on failure. The group is named `Answers: <prompt>`.
`escalation`-source rows (max iterations, cannot proceed, loop detected),
change/other answers, secret and protected asks, preference holds and
confirmations outside the allow-list keep the row click, which opens the
prompt. The compact Attention center does not show these buttons.

**Legacy links.** Rendered internal Markdown links and direct entries for retired
Work hosts are normalized to Work (before the SPA error page), keeping quality
targets, repeated query values and fragments, without rewriting replay-bound
bodies. The destination's authentication gate still applies.

**New work** opens the task form as a focused view: title, outcome and an
eligible Personal owner suffice; references, thread, output mode and scheduling
are optional; schedule, program and quality configuration are not required.
Opening it clears task, execution and plan focus but keeps creation context
(thread, feed filters); Cancel returns to the feed. Creation uses
`POST /api/magician/v3/tasks`.

**Task inspector:** a right-side modal drawer at the page root that overlays
Work without narrowing the feed; full-viewport on phones, with a Work return
control. The Playbooks overview stays mounted behind task and plan drawers (no
shift, no document reload); keyboard traversal stays inside the drawer. The
[Unified task panel](unified-task-panel.md) owns Run, Plan, Output, previous
executions, questions, files and technical details. The Plan Inspector mounts
above it (Escape closes it first) and Back/Forward reconciles it with `selected`
and `plan=open`. Closing or switching tasks clears plan and execution focus.
System work and Crew cycles omit task-plan actions. The status ledger counts
Failed and Cancelled separately (an owner-stopped job is not a failure) and
describes retained tasks, not success rate or Quality coverage.

**Thread moves** keep their original task, need a loaded destination, and stop
if the panel closes or scope or credentials change. The thread store checks
credential revision, principal and workspace, so a stale lookup cannot create a
destination with replacement credentials. A rejected update reports failure.

**Crew** links open the member workspace within Work. Task-to-Crew and
task-to-Quality links carry the original task, filter, run and inspector query
in a validated local `from` return link, which Crew tabs preserve. Editor, rules and memory-tier
inspectors live at `/work/crew/new`, `/work/crew/[id]/edit`,
`/work/crew/[id]/rules` and `/work/crew/[id]/memory/[tier]`; definition writes
and member actions keep their existing contracts. ETag conflicts keep
the editor open without reporting a save; responses from a prior scope, request
or destroyed screen never replace the current record.

**Themes and access.** All 24 themes apply through common tokens and native
components (New work uses `native/Button.svelte`). Work, its core child paths
and the specialist allowlist are available on authenticated remote touch-screen
web sessions; retired route names get no exemption, other console pages stay
local-only, and the root bearer/session gate and scoped API permissions are
unchanged. On narrow screens controls wrap before hiding (no control is
removed); the smallest phone header keeps the brand mark and search but drops
the wordmark and shortcut hint; the More panel scrolls within the viewport;
task menus flip and scroll within viewport and scrollport, recalculate on
resize and focus their first action; the 44px Plan graph control has its own
header column.

## Existing feature paths

Retained web implementations; provider qualification stays with each feature.

| Action or information | Web destination and retained implementation |
| --- | --- |
| Create task; owner, references, dependencies; thread; accumulate/overwrite | New work → `TasksWorkspace` / `TaskCreateForm` / mention editor |
| Recurring cadence and timezone; pause/resume schedule; convert to monitor | Task scheduling controls / `MonitorComposer` |
| Inbox, Today, Overdue, Running, Completed; tags, paging, page size | All work → unchanged task filters, counts and pagers |
| Priority, due date, description, tags, completion, thread move | Task card menu and shared task inspector |
| Plan, direct run, approve/reject, approve and run, reset, cancel | Task lifecycle handlers and server admission |
| Steer, pause, resume, stop, owner questions, stalled work | Execution controls and canonical Attention handlers |
| Plan graph, waterfall, analysis, slots, attempts, clarifications, history, edits | Deep `PlanInspectorSheet` |
| Immutable output per execution; previous runs; preview/open/reveal/export | Shared task panel, output adapters, artifact viewer, export menu |
| Monitor configuration, runs, updates, feedback | All work → Monitors → monitor workspace |
| Internal cycles, synthesis retry, recurring App history | All work → System work → internal workspace |
| Crew roster, filters, leaderboard, hierarchy, pipeline | Crew → `CrewWorkspace` |
| Member overview, health, effective tools, models, execution details | Crew member → `CrewMemberWorkspace` and panels |
| Definition, kind, trust, tools, delegation, rules, primary owner | Definition editor, read-only rules browser, validated mutation handlers |
| Memory, consolidation health, user/shared memory, retention/feedback config | Crew member tabs and memory inspectors; user-memory merge is an explicit write |
| Persisted agent episode history | Separate `/history` inspector; member History shows retention configuration |
| Program metrics/state, cycle evidence, owner/structural requests | Responsible member's harness/operator projection and canonical decision controls |
| Backlog promotion and delivery review | Harness-owned tools and stores; full human UI/API coverage is follow-up |
| Playbook document, managed missions section, edit, version restore | Playbooks → scoped program read/save/revert APIs |
| Goals | More → Goals → `GoalsWorkspace`: New folder and New goal (main/sub, private and proposed, owned by you) and a target-date edit through the notes front-matter API; goal tree from `Workspaces/*/Goals/<Main goal>/*.md` (one folder per main goal) and legacy `Workspaces/*/Goals/*.md` via the notes tree/file APIs, up to 3 folder levels, 1000 folders and 2000 notes per workspace with a not-shown count; Not linked for missing-parent, self-parent and parent-loop; only open goals are drawn and counted (here, on the All work card and in the apps), Mark done sets `status: done` + `closed:` and Open/Closed (`&goals=closed`) lists done and dropped goals with Reopen; fully closed workspaces appear only under Closed, empty ones under Open, and partially read ones remain visible in both lists; iOS `WorkGoalsView` and Android `WorkGoalsScreen` port the same tree, limits and copy ([feature guide](../../features/goal-workspaces.md)) |
| Agent briefs, contracts, task context, references, budgets, model and thinking choices | Quality checks → `RelentlessWorkspace` |
| Attempts, exact-artifact acceptance, resume/cancel, review reports, evidence | Relentless review and handoff controls |
| Guidance, campaigns, capture policy, advanced configuration | Relentless tabs and editors |
| Global loop enable/disable, drain semantics, trigger, anomalies | More → Team routines → operations workspace |
| HITL answers, approvals, escalations, failed-task dismissal | Needs you → canonical `AttentionWorkspace`; `/attention` retained |
| Broader OPC engagements, rooms, commitments, envelopes, receipts | Needs a separate integration audit |

Program listing covers the scoped `programs/*.md` API; default, focus and
provider Playbooks are exposed by Crew Autopilot controls. There is no
general program-authoring endpoint, and Work does not assume every provider is a
writable local file.

Goals can select an existing Playbook during creation through the current notes
content API. The link supplies direction without starting work or changing crew
assignments. Goal rows distinguish a missing document from a catalog lookup
failure; the latter leaves the goal visible and offers Retry. One main goal can
coordinate several Playbooks through its sub-goals and their existing focus-area
identities ([goal-note contract](../../features/goal-workspaces.md#work--goals)).

## Native apps

Both shells put Work in the former Tasks tab position; Today stays the initial
tab. Controls use the installed theme at 44pt (iOS) and 48dp (Android). New work
creates a one-off task (never a hidden monitor). Navigation and connection
changes clear consumed deep-link targets; Back returns through the detail stack,
and task-to-Crew/Quality panels return to the original task. iOS keeps its task
workspace mounted across root views; Android keeps its view model and saved
state; pushed details hide the root Work controls. Legacy `/tasks` and new
`/work` links keep task, execution, monitor update and plan-focus identities;
notification intents use the existing host.

| Native Work surface | Controls and details |
| --- | --- |
| All work and Needs you | Task and Plan Inspector, task fields, validated thread moves, scheduling, references/dependencies; Attention answers and approvals |
| Monitors and System work | Separate server filters, paging and page size, exact updates/executions, output ownership; confirmed cleanup with progress and partial failures; recurring App history, schedule and originating installation |
| Crew | Guided Add/Edit plus advanced definitions with pinned ETags and unknown-field preservation; search/status/type, leaderboard/hierarchy/pipeline, model pins/default clearing, run once, pause/resume, primary owner |
| Member details | Effective tools, runtime cache, health/bootstrap, current cycle; all rules sections, goal/user/shared memory tiers, isolated-user merge; harness projections, owner requests, structural proposals |
| Playbooks | Scoped read, history/confirmed restore, responsible members, program sections, phase/step/open loops, task/execution evidence |
| Quality | Server-paged inbox/runs, owning tasks, guided and advanced contract, provider-backed reference picking, create/eligible-attach, frozen preview, exact review, previous-attempt comparison, bounded recurring policies |
| Run detail | Attempts, artifacts, reports, lessons, cost reconciliation, cancellation, owner questions, resume |
| Capture and learning | Server-paged policies/proposals/captures; preview/version checks; binding/review, qualification, revocation/resume, trial and adopted-run monitoring evidence |
| Team routines | Enable/disable, warnings/anomalies, ordered manual company-sequence dispatch through agent goals |

Guarantees:

- Advanced Crew definitions and policy specs stay editable JSON, preserving
  fields with no dedicated editor (including large integer budgets). Rules and
  memory-tier pages are inspectors, not authoring tools. Android Crew → Quality
  keeps the selected member through contract review and start; task/proposal
  context supplies its own recorded owner.
- Task creation defaults to the enabled primary Personal member (Android also
  shows Create at the top of the sheet). Completed references stay removable
  and merge their dependency IDs with manual ones. Thread moves use the loaded
  active catalog (Android searches and bounds it) and recheck the destination
  before writing. Schedule pause/resume is separate from execution pause/resume;
  schedule edits read the current record and keep pause, limits and unknown
  fields.
- Search, tag and sort cover the whole matching corpus; Internal filters use
  their own server contract; monitor refresh keeps pages beyond the 200-record
  request ceiling. App-managed runs are inspectable (previous executions,
  schedule, output) but excluded from cleanup; their lifecycle belongs to the
  App. Cleanup keeps failed selections for retry.
- Requests capture one immutable origin/header snapshot. A scope change clears
  records, previews and pending confirmations, and an old screen cannot
  reconnect its editor to a new machine; writes and late responses are
  generation-checked; Crew edits and policy confirmations use the version
  opened (iOS editors open an immutable version/scope session); runtime
  credentials do not follow HTTP redirects or appear in URLs. Filter/page
  changes during a write load the latest selection and keep any failure; a
  failed iOS task creation keeps its draft. Android invalidates the connection
  on same-origin re-pairing or credential/workspace change, not display-name
  edits.
- Exact-artifact approval stays disabled until the files are reviewed. Binary
  files preview locally through the frozen authenticated client, with explicit
  open/share. Both clients stream artifacts and enforce the 128 MiB ceiling
  against declared and delivered bytes, stopping before EOF even without a size
  header; failed or cancelled transfers release the response. Capture binding
  uses the reviewed artifact hash and campaign version and records a local
  capture; sealed trial and monitoring evidence shows only when its content hash
  matches the policy.

## Source and compatibility

`lib/work/WorkWorkspace.svelte` composes the workspaces; `WorkFrame.svelte` is
the shared chrome, also used by deep member pages. Attention keeps its direct
route. Quality is the shared `RelentlessWorkspace` (no standalone Relentless
page); its frozen preview shows a collapsed paid-tool contract (server-resolved
skill pins, argument grants, pricing, call limits) before admitting metered
tools. Crew definition transport is `lib/magician/crew/definitionApi.ts`,
imported directly by every editor.

`/tasks`, `/crew` and its former member/editor/rules/memory children,
`/relentless`, `/internal-tasks`, `/debug/tasks`, `/harness` and thread task
route aliases are removed with their redirects; Work and its child routes are
the supported URLs. Shell navigation and both native bottom bars use Work; the
command palette has one Work destination searchable by tasks, Crew, monitors,
System work, Playbooks and Quality checks. In-app links are normalized into Work,
keeping query values, repeated filters, task/run/plan/monitor-update identities
and hash focus; native link parsers still read older payloads. Thread-scoped
lanes use the shared Work workspace; unresolved threads stay read-only until
resolution succeeds. New monitor Today/notification links and monitor-tool URLs
target `/work?type=monitors`; proposal-tool review URLs target
`/work?view=quality`; Quality-to-Quality links keep the original return path.
Task ownership, exact artifact review, advanced editors and
`/api/magician/v2/relentless` endpoints are unchanged; persisted notices and
output records are not rewritten. The legacy route name stays reserved so an
App cannot take over bookmarked review links.

Scope and staleness rules:

- Draft admission retries reuse an ID only for the same reviewed request (owning
  agent, task thread, resolved contract hash); interrupted existing-task
  admissions keep their server-prescribed ID.
- A failed previous-attempt comparison never blocks review of loaded bytes (its
  retry reloads only the comparison); a failed accepted artifact still blocks
  approval. Run actions, reports, lessons and download errors stay bound to
  their original run.
- Run and proposal deep links stay pending if identity resolution changes the
  workspace mid-load; refresh resumes them in the resolved scope and discards
  the interrupted response.
- Program reads are cancelled and, with restores, generation-checked against
  the credential revision after each await (including same-workspace bearer
  replacement); late reads are discarded; a restore confirmed after a scope or
  selection change is not submitted, cannot start from a document loaded under
  earlier credentials, and shows a late error only on its original program.
- An incoming task selection survives the first authenticated scope hydration;
  later workspace changes clear it.
- Company-loop dispatch stops later stages on navigation, scope or credential
  change and discards stale results; a request the server already accepted keeps
  its execution lifecycle. Crew (roster, member, effective tools) and Quality
  (contract, artifacts, policy/campaign state) reload on credential revision
  change even when principal/workspace names are unchanged; Quality starts and
  approvals require current review state.

## Verification

`make test-ui-work` covers task lifecycle, Attention integration,
Crew/operations, Relentless, programs, route compatibility and simple creation.
`make test-ui` is the full unit/component lane; `make check-ui` checks
production types.

`make test-ui-work-browser` (`scripts/qualify-unified-work-browser.py`, driving
`agent-browser`) runs against a dev server you start separately, by default at
`http://127.0.0.1:5175` (start it after dependency-installing Make checks, since
reinstalling packages mid-run can invalidate its modules; flags `--origin`,
`--session`, `--output`, `--route`; pass them through `ARGS=`).
Its fixture intercepts API requests and changes no real data. It visits 18 Work
routes at 320, 375, 430, 768, 959, 960 and 1280px plus 320/390px with 130% text,
across all 24 themes, waiting for required views and loaded fonts and checking
overflow, clipping, inspector bounds/roles, the open More panel, touch targets
and exceptions, and writes `.local/designs/unified-work-browser-results.json`.
It is layout evidence only: it does not qualify live providers, native apps or
every specialist action; theme-picker behavior and real backend journeys are
checked separately.

Native (real builds, app journeys, deep-link/Back and theme/accessibility
checks): `make test-ios-work` (iOS simulator); `make check-magdroid` and
`make test-magdroid-work ANDROID_SERIAL=<device>` (Android). Results are in the
[qualification record](unified-work-qualification.md); physical-device and
provider acceptance are separate.

## Complete migration web checkpoint (2026-10-06)

- Internal filters, page/sort and selected executions hydrate from URLs;
  selected records outside the page load independently and Back preserves the
  list. Bearer changes notify scoped resources without publishing credentials.
  Installed-App links come only from trusted task manifest provenance. Guided
  Crew saves keep nested extensions and integers beyond JavaScript's safe range.
- Specialists carry exact Work return links across subpages; Analytics hydrates
  filters before fetching; Events accepts member/task/run filters; Playbooks link
  exact document paths to configured members, progress, metrics and evidence.
- Quality links with a run, proposal or campaign open that target (a task ID
  only adds context, never the draft composer); a task-only link prepares
  improvement of that task.
- Quality allowance stops appear in Needs you, linked to the item's quality
  review. Continuation shows only reached limits, defaults to $100 / 24 hours /
  25 attempts and needs explicit owner approval; other balances, artifacts and
  paid checkpoints stay. Run details show remaining allowance apart from
  cumulative usage, with approval history. Stop work cancels the waiting run.
  Archived or superseded roots offer no continuation.
- Work Operations has a collapsed, server-paginated Scheduling and delivery
  panel with scoped record inspection. Automatically associated scheduling
  replies appear as ordinary Work items; unverified senders and unclear replies
  go through owner approval and Attention; raw provider bytes stay in the
  backend audit.
