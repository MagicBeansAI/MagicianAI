# App agent capability bindings

App workflows may use a normal scoped agent, an optional scoped personality,
package-private workflow instructions, immutable procedure dependencies, and a
reviewed tool/resource ceiling. Those inputs have different source owners and
update independently, so the workflow task sidecar seals the exact material
selected at launch; a friendly agent or personality name is never sufficient
reconstruction evidence.

## Review

Installation review is the first immutable boundary. It lists, per resolvable
workflow, the canonical agent revision/content/descriptor digests and optional
personality content/descriptor digests, plus one aggregate material digest that
approval must echo. The per-workflow evidence is stored in the consumed
installation approval, whose identity includes the aggregate. Launch recomputes
that identity from the consumed binding list, attempt, approving session and
grant digest, so evidence drift cannot silently keep the old `approval_ref`.

Review and launch share one runner-eligibility rule: the definition must be
enabled and permit the canonical `Task` invocation surface; otherwise review
refuses it rather than showing an unlaunchable workflow.

**Tool resolution at `workflow.uses`.** Declared uses are intersected with the
reviewed, admitted tool set (locked dependency list + live grant). The runner
contributes only blocking rules (`excluded_tools`/`denied_tools`) and the two
deny-all shapes `resolved_tools` carries (strictly tool-free public surface;
untrusted definition with no explicit allowlist); any of those refuse with
`RunnerMissingDeclaredTool`. A tool the runner simply does not list still
resolves, because `tools:` layers named work tools over the auto-injected
universal substrate (`tools: []` = all). Dispatch additionally needs a wired app
effect owner ([`app-tool-bind.md`](app-tool-bind.md)).

**Eager catalog.** Agentic app decisions eagerly expose the admitted flat action
catalog: the shared ToolIndex supplies concrete primitive schemas (e.g.
`meetings_data__list_threads`); execution-local store-query and terminal-commit
tools keep their supplied schemas. Same grant/deny/withheld filters as deferred
projection; no `tool_search` grant or ambient packs. Policy snapshot and exact
locked-action dispatch checks still run. For host-read leaves, dispatch
normalizes `<pack>__<action>` to the granted pack plus an explicit action
selector (conflicts rejected); only the bound host-read owner accepts this, with
no generic/browser/CLI fallback.

## Durable binding

Every newly admitted app task records:

- the full bounded `AgentDefinition`, its revision, canonical definition,
  persona, prompt-pipeline and constraint digests;
- the final tool set after agent, app grant, workflow and every procedure
  ceiling intersect;
- the final resource ceiling (app authority narrowed by the agent's token and
  duration limits; the sealed definition keeps its own loop-iteration ceiling);
- the workflow-instruction digest and each procedure's dependency reference,
  semantic version, content digest and instruction digest;
- the optional personality's canonical structured bytes and content digest;
- one workflow-authority digest joining live capability authority to the
  immutable agent and prompt-material digest tree.

The task-sidecar byte limit is the outer bound; one agent definition is capped
at 256 KiB. Debug output prints only identities and digests. Procedure and tool
identity is anchored by the package lock; procedure source is package-private
and reconstructed through the locked revision owner.

## Launch and resume

Launch traces the installation to its committed lifecycle attempt and uniquely
consumed approval; scope, candidate package, active grant and approval
consumption must agree. The consumed revision must match the generation created
by that transition: initial install = 2, update = source + 2
(`BeginUpdate`, `CommitUpdate`), retained reinstall = source + 1. Later
disable/enable may advance the generation, so it must be ≥ the reviewed commit
generation; package and grant stay exact. Current agent and personality bytes
must reproduce the reviewed evidence before a fresh launch persists task-local
copies. A legacy approval with empty evidence fails recoverably with
`InstallationReReviewRequired`.

Launch then resolves the lock, computes all intersections, and persists one
immutable binding before V3 execution starts. Artifact V2 dispatch and resume
obtain a non-serializable restoration proof after the service verifies sealed
agent, prompt tree, base authority and workflow-authority digest. The
orchestrator's app-only override builder consumes that proof and the sealed
definition directly and never reopens the mutable definition store, so editing
or deleting the definition cannot change or strand an admitted task. Live
installation, grant, provider and revocation checks stay separate.

**Idempotency.** Direct user, personal-agent, brokered-transfer and background
retries derive the deterministic task id before touching the authored catalog.
With a complete binding, the service validates caller input identity and lane or
receipt, reconstructs the sealed proof, and reopens only live
installation/grant/schema/package/source authority. Only a missing task or a
proven pristine pending shell with no disk or registry binding may re-enter
fresh admission; one-sided publication and identity drift fail closed.

Legacy sidecars stay deserializable (new fields optional) but are not
executable: missing sealed material returns `UnpinnedWorkflowMaterial`. Resume
decodes the sealed personality and verifies its content address, never
substituting the current file; the processing boundary renders it and includes
the workflow-authority digest in disclosure identity.

## Authority and resource rules

The effective tool set is an intersection, never a union. Procedure text cannot
add a tool. Ambient app delegation is empty and the app runner's spawned-task
budget is zero.

`max_tokens_per_cycle` is split conservatively between input and output so the
sum cannot exceed the agent limit. Active and lifetime seconds clamp to the
agent duration. The loop-iteration ceiling is not translated into a paid-tool
count (one iteration may contain several calls). App-level cost,
tool-invocation, browser/network, data, attachment, monthly and concurrency
ceilings are enforced independently and cannot be widened by the agent.

## `agent_as_tool` boundary

An agent may declare a closed `app_tool` contract: object input/result schemas
plus non-zero byte ceilings capped at 256 KiB. Changing it changes the agent
definition digest and the primitive action/implementation identities. The V2
contract binds schemas and ceilings to the sealed definition, narrowed
tools/resources, effective data-handling policy, a fixed
`app_agent_tool_result.json` identity and fixed child prompt digest. Only a
non-default, Task-callable definition with one sealed `agent_as_tool` action is
`Ready` and lockable; implicit default runners, missing/invalid contracts,
other actions/modes, and disabled or non-Task definitions stay blocked.

**Child binding and adoption.** The durable same-task child binding fixes app
scope, parent/child execution refs, invocation, typed input digest and labels,
retry generation, absolute launch expiry, workflow authority and the full
primitive/action/agent contract. The V3 sidecar is reserved before any child
row, parent link or `WaitingChildren` mutation; the delegation runtime then
creates and links the deterministic shell, advances the sidecar to `Attached`,
and only then schedules it. Runtime code sees only a non-Clone, non-Serde child
handle; restart reopens the persisted binding.

- Startup adopts a pre-shell reservation or an `Attached` pre-start shell through
  the same dispatcher, first repairing an idempotent missing parent link; an
  unscheduled shell is never cancelled into an ownerless row.
- An `Attached` child is adoptable only while `pending`, `ready` or `queued`.
  From `running` (or unknown nonterminal) provider I/O may have begun, so
  startup exact-fails the leaf through the payload-free uncertain carrier and
  never redispatches. The sweep remembers children it adopted itself so a newly
  scheduled child is not mistaken for a stale crash.
- Expired or unauthorized never-created reservations are tombstoned before the
  parent fails; an `Attached` shell gets a durable cancellation intent first.

**Child authority.** Local capacity admission is not launch authority: after it,
and again before the child model loop, the runtime revalidates installation,
grant, lock, source definition, sealed callable definition and
workflow-authority digest; drift leaves the child terminally unavailable. Child
input crosses the same reviewed model-processing boundary as the parent, which
records a content-free admission receipt and installs an opaque physical-attempt
disclosure guard (a direct child without it is rejected). The sealed absolute
expiry is a non-renewable ceiling on provider I/O, capacity waits and restart
adoption. Labels derive from the sealed contract and admitted value; upstream
supplies only a provenance digest and cannot relabel input.

**Results and settlement.**

- Cancellation is a separate durable non-terminal intent (request ref, reason,
  timestamp, pre-start posture) and never consumes the terminal outbox slot.
- Only a canonical terminal observation plus an opaque clean-settlement proof
  from the common effect ledger mints a completed carrier. Uncertain settlement,
  unavailable authority fence, or failure without clean proof yields a bounded
  `outcome_uncertain` carrier; an open resource reservation stays pending.
  `cancelled` requires an opaque scheduler ack that cancellation won the start
  race.
- A completed child must have exactly one bounded JSON tool-output artifact
  under its execution whose Artifact record carries the fixed result name,
  contract digest and child-launch digest, and which matches the result schema.
  The schema reaches the orchestrator only via an opaque permit from the sealed
  binding: protected-app prompt seeding deletes every ambient declaration, then
  restores only this name, JSON content type and schema. Missing, duplicate,
  non-JSON, oversized or invalid output settles once as payload-free
  `outcome_uncertain` (terminal bytes are immutable; never retried forever).
- The carrier holds only the typed value, opaque Artifact ref/content digest,
  labels and child/binding identity — never transcript or handoff text. Result
  creation and every replay require a current installation/grant/scope/policy
  fence.
- The notification outbox rebuilds the carrier from the sealed binding under one
  deterministic ref. Delivery is marked only with an Artifact-owned ack minted
  after the parent event commits, so a pre-commit crash replays and a post-ack
  crash cannot double-wake the parent. The acked carrier is retained as the
  parent's labeled continuation (also on startup replay, in the same sweep).
- Cancellation persists intent before the specialized leaf-tree stop, on both
  scheduled-child cancellation checks; the generic delegation helper never
  reports an app child stopped. If intent or stop cannot be persisted, the leaf
  is terminally failed and the uncertain carrier projected; if neither owner is
  writable, a bounded-backoff retry loop holds and never returns an adoptable
  child. An execution-row reload error takes the same path.

The implementation-plan digest frames all load-bearing agent definition,
schema/policy/effect, workflow, Artifact V3, scheduler/reducer/service,
storage-lock, runtime, orchestrator and executor sources, so old reviewed
bindings fail closed when the child owner changes. No generic handoff, remote
control, transcript transfer or second executor exists.

## Terminal commit and no-change results

`may_mutate` limits writable entities. An empty terminal batch is valid and
yields the owner's no-change result (null receipt, no revisions, no change
sequence); caller output cannot claim a write, and empty batches reject
record-revision preconditions. Read-only workflows still need typed output.
Stored-result validation treats the owner-generated no-change value as
receipt-free; a no-change result with a receipt is rejected.

A successful `app_commit_mutations` ack ends the protected agentic run at once;
no further model turn runs and no generic artifact or learning sink receives the
private result.

## Host-read providers and data policy

Scoped capability snapshots keep the built-in providers for the five Apps
host-read binders (agent roster, evidence, meetings, thinking maps, internal
learning data). Each gets an Apps provider witness only when the scope selected
the embedded fallback and its parsed definition matches exactly; scope or
extra-path overrides stay unwitnessed even with identical bytes.

These five binders label results with the installation's reviewed processing
ceiling (content stays Secret, joined with input/session labels and policy), so
an owner-approved `remote_allowed` is not silently forced local. Other file,
browser, device, network and MCP results keep conservative Secret/LocalOnly.
This contract is part of the locked tool descriptor.

Fresh workflow input policy honors the reviewed processing ceiling after joining
manifest, present-field and trusted source policies; normalization and resume
preserve the joins. The Sensitive floor remains; a caller RemoteAllowed label
cannot override reviewed LocalOnly/None, and sealed local-only runs stay local
after a package update.

## Task lifecycle and background launch

App action tasks use the Internal lifecycle (existing `task_app_` runs are
projected as internal without moving bindings); index rebuild and reconciliation
share the classification, and generic lifecycle/delete routes stay closed to app
runs.

Background launch validates server token, action, caller, source digests, scope
and expiry before minting a short-lived `ReviewedBackgroundLaunch` scope bound to
one installation. Ordinary SystemWorker scope cannot execute an app; the launch
scope cannot approve installations or create interactive meeting-capture
intent; grant, pause, lease and resource checks still run.
