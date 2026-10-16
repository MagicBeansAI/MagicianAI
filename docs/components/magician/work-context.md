# Work context — capability that flows from the work

Composable work modules §2.1. Module: `magician/src/magician_v2/work_context/`.

## What was blocking composition

Capability is granted **per-agent, statically**, and there is no
skill-discovery tool, so an agent cannot find a capability it was not given.
Composing for a new purpose meant editing agent YAML.

## The generalisation

The engagements plan defines `effective = agent ∩ engagement ∩ channel`. The
middle term widens from *engagement* to **work context** — a program **or** an
engagement — so grants flow from the work as well as from the actor. Both kinds
narrow identically.

## The rule that makes it safe

**A work context identifies capabilities; it never attaches them.**

Intersection can only *narrow*. A capability the work names and the agent does
not hold is not granted by naming it; it becomes a pointer to delegate to someone
who does.

| agent holds | work needs | standing |
|---|---|---|
| yes | yes | **usable** — the only permitting arm |
| no | yes | requires delegation |
| yes | no | outside this work |
| no | no | unavailable |

`permits_direct_use()` is true for exactly one variant. A test walks every
combination to prove no other path reaches it; one path that granted by naming
would let a program hand any agent any tool.

**"Outside this work" is not a revocation.** A caller deciding what to **offer**
should exclude it; a caller deciding what to **forbid** should not, because the
agent's own grant still stands outside this work.

## Discovery is the other half

`delegation_needs` returns what the work needs and the agent cannot do.
`capability_summary` renders both halves into the decision prompt when the
dispatch binding is `Bound`, so the model sees the shape of the work before it
plans. An agent that cannot see what the work needs would improvise with what it
has, turning a narrow grant into a **wrong answer** rather than a handoff.

## Decoupled on purpose

`agent_capabilities` is passed in, not read, so one function serves a dispatch
gate, a discovery surface and a test (the same decoupling the envelope gate
uses). Names compare case- and whitespace-insensitively, since programs and
grants are authored in different places. Reads are sorted and deduplicated.

## The durable carrier lives here too

`WorkAuthorityRef { work: WorkContextKind, authority_revision }` is the
authority a persisted `ExecutionRun` carries
(`magician/src/magician_v2/storage/models.rs` `work_authority`). It is generic, so
a run for any kind of work (including a program-scoped root) has a slot to fill.

`engagements.rs` names `work_context`; `work_context` never names
`engagements`. The two conversions live on the OPC side:

- `From<&EngagementAuthorityRef> for WorkAuthorityRef` — what gets persisted.
- `TryFrom<&WorkAuthorityRef> for EngagementAuthorityRef` — narrowing back for
  the §4.2c dispatch checks. Every non-engagement arm is an **error**, never
  `None`, because downstream "no engagement" and "an engagement ceiling that
  cannot be enforced here" would look the same, and the second would run.

Fail-closed properties:

- `#[serde(default)]` on the record, so an absent value reads as *no
  authority*, never as unrestricted.
- Ids are guarded by `WorkContextKind::guard_id` (blank and U+001F refused) at
  construction and when a carrier is rebuilt from the storage wire form
  (`runtime_core::WorkAuthorityGrant`), where an unknown kind token is refused,
  not coerced.
- A root's revision comes from the roster that owns the work; a child's carrier
  is copied verbatim from the parent's durable record. Neither is
  caller-settable (§4.2c row 5).

Proof: `magician/tests/work_authority_carrier.rs`. Engagement-scoped behaviour
is still proved by `magician/tests/engagement_authority_matrix.rs`.

## Where it is enforced

**Gate 0 of the outward gate.** `execute_action_inner`, which every action
passes through, calls `outward_gate::work_context_refusal` inside
`if let Some(..) = classify_outward_dispatch(action)`. That calls
`work_context::resolve` with the acting agent's own grant
(`agent_capability_grant` = `ctx.merged_agent_tools` minus the whole-tool deny
set, resolved at dispatch). Only `CapabilityStanding::permits_direct_use()`
proceeds; the other three refuse, and the refusal is written onto the act's
disclosure record as `failed`. It runs **ahead of the capture/live branch**, so
refusal applies under capture as well as live.

`work_binding_for_dispatch` reads the generic
`ctx.work_authority: Option<WorkAuthorityRef>`, not an agent-authored id:

| carrier | binding | what gate 0 does |
|---|---|---|
| absent | `WorkBinding::Unbound` | **no opinion** — declines to answer; every other gate still decides |
| `Engagement` | `Bound`, needs = live `tool_ceiling` | narrows; store miss / revoke / expiry is `Unresolvable` (refuse) |
| `Program` | `Bound`, needs = the agent's own grant | **named, not narrowed** — intersection is the grant unchanged |
| unreadable id / no store (engagement) | `Unresolvable` | refuse |

An **engagement** ceiling is read live from the roster at each dispatch, so
revocation, expiry and narrowing apply to the next act.
`POST /api/magician/v2/engagements` is the production caller of
`EngagementStore::create` (via `grant_for_work`);
`PATCH /api/magician/v2/engagements/{id}` renews the window without moving the
id. See [`engagements.md`](./engagements.md) and
[`opc-owner-and-reader-surfaces.md`](../magician-api/opc-owner-and-reader-surfaces.md).

A **program** binding files the act under `program_id`, scoped to
`EnvelopeScope::Program` (`envelope_scope_for_work`, via
`From<&WorkContextKind> for EnvelopeScope`). Its need list is the agent's own
grant because no roster owns programs, and inventing a ceiling would be
caller-supplied authority. See [`approval-envelopes.md`](./approval-envelopes.md).
`root_authority_for_work_with_store` **refuses** `WorkContextKind::Program` until
a roster can answer with a live revision.

What the gate does not reach:

- **Inward acts.** File reads, memory searches and shell commands never reach
  gate 0. Retrieval containment
  (`docs/components/magician/engagement-scoped-retrieval.md`) is a separate gate;
  `work_retrieval_refusal` also confines a Program carrier.
- **Tool resolution.** `execution::restricted_toolset` does **not** consult a
  work context, so the model is still offered capabilities the work does not
  name; the prompt summary and the dispatch refusal tell it.
- **Programs, in production.** Until a program roster exists, production roots
  never carry Program, and a Program binding that arrives (tests, inherited
  carriers) names the work without shrinking the grant.
- **Delegation.** §2.1 step 4 (the work context travels with the delegation)
  needs a carrier that does not exist yet; `delegation_needs` only names the
  handoff inside a refusal message.

`Unbound` is a distinct arm, not a permissive default. Once an execution carries
a live engagement, the ceiling applies to every outward act of that execution.
