# Work context — capability that flows from the work

Composable work modules §2.1. Module: `magician/src/magician_v2/work_context/`.

## What was blocking composition

Capability is granted **per-agent, statically**, and there is no skill-discovery
tool — an agent cannot find a capability it was not given. So "compose for a
specific purpose" meant editing agent YAML, which does not scale to purposes
nobody has thought of yet.

## The generalisation

The engagements plan defines `effective = agent ∩ engagement ∩ channel`. Widen
the middle term from *engagement* to **work context** — a program **or** an
engagement — and grants flow from the work as well as from the actor.

Both kinds narrow identically. The generalisation is that the middle term
widened, not that programs got special treatment.

## The rule that makes it safe

**A work context identifies capabilities; it never attaches them.**

Intersection can only *narrow*. A capability the work names and the agent does
not hold is not granted by naming it — it becomes a pointer to delegate to
someone who does, and the work context travels with the delegation.

| agent holds | work needs | standing |
|---|---|---|
| yes | yes | **usable** — the only permitting arm |
| no | yes | requires delegation |
| yes | no | outside this work |
| no | no | unavailable |

`permits_direct_use()` is true for exactly one variant. A test walks every
combination to prove no other path reaches it: a single path that granted by
naming would turn a program into a way to hand any agent any tool.

**"Outside this work" is not a revocation.** It says only that using a held
capability *here* is outside what the work is for. A caller deciding what to
**offer** should exclude it; a caller deciding what to **forbid** should not,
because the agent's own grant still stands outside this work.

## Discovery is the other half

§2.1: *"the acting agent sees those capability summaries."* `delegation_needs`
returns what the work needs and the agent cannot do. `capability_summary`
renders both halves into the decision prompt when the dispatch binding is
`Bound`, so the model sees the shape of the work before it plans.

That visibility is load-bearing. An agent that cannot see what the work needs
would improvise with what it has — which turns a narrow grant into a **wrong
answer** rather than a handoff.

## Decoupled on purpose

`agent_capabilities` is passed in, not read. Keeping this free of the
agent-definition layer is what lets one function serve a dispatch gate, a
discovery surface and a test — the same decoupling the envelope gate uses.

Names are compared case- and whitespace-insensitively: a program is authored by a
human in YAML and a grant is authored elsewhere, and requiring them to agree on
capitalisation would make composition fail for a reason nobody could see.

Reads are sorted and deduplicated, so two callers asking the same question get
the same answer in the same order.

## The durable carrier lives here too

`WorkAuthorityRef { work: WorkContextKind, authority_revision }` is the
authority a persisted `ExecutionRun` carries
(`magician/src/magician_v2/storage/models.rs` `work_authority`). It is
deliberately generic rather than engagement-specific so a run for any kind of
work (including a program-scoped root) has a slot it can honestly fill.

The direction of the dependency is the whole point. `engagements.rs` names
`work_context`; `work_context` never names `engagements`. The two conversions
live on the OPC side:

- `From<&EngagementAuthorityRef> for WorkAuthorityRef` — what gets persisted.
- `TryFrom<&WorkAuthorityRef> for EngagementAuthorityRef` — narrowing back to
  the shape the §4.2c dispatch checks take. Every non-engagement arm is an
  **error**, never `None`: "no engagement" and "an engagement ceiling that
  cannot be enforced here" are indistinguishable downstream, and the second one
  would run.

Fail-closed properties the carrier keeps:

- `#[serde(default)]` on the record, so an absent value reads as *no
  authority*, never as unrestricted.
- Ids are guarded by `WorkContextKind::guard_id` — blank refused, U+001F
  refused — at construction and again when a carrier is rebuilt from the
  storage boundary's wire form (`runtime_core::WorkAuthorityGrant`), where an
  unknown kind token is refused rather than coerced into the nearest arm.
- A root's revision comes from the roster that owns the work; a child's carrier
  is copied verbatim from the parent's durable record. Neither is
  caller-settable (§4.2c row 5).

Proof: `magician/tests/work_authority_carrier.rs`. Engagement-scoped behaviour
is unchanged and still proved by `magician/tests/engagement_authority_matrix.rs`.

## Where it is enforced

**Gate 0 of the outward gate.** `execute_action_inner` — the one function every
action passes through — calls `outward_gate::work_context_refusal` inside
`if let Some(..) = classify_outward_dispatch(action)`. That calls
`work_context::resolve` with the acting agent's own grant.
`agent_capability_grant` is `ctx.merged_agent_tools` minus the whole-tool deny
set, resolved at dispatch rather than read from a definition.
`CapabilityStanding::permits_direct_use()` is the only arm that proceeds; the
other three refuse, and the refusal is written onto the act's disclosure record
as `failed`.

It runs **ahead of the capture/live branch**, so a capability the work does not
narrow to is refused under capture as well as live.

`work_binding_for_dispatch` reads the generic
`ctx.work_authority: Option<WorkAuthorityRef>` — not an agent-authored id, and
not a leftover `engagement_authority` field. The arms:

| carrier | binding | what gate 0 does |
|---|---|---|
| absent | `WorkBinding::Unbound` | **no opinion** — declines to answer; every other gate still decides |
| `Engagement` | `Bound`, needs = live `tool_ceiling` | narrows; store miss / revoke / expiry is `Unresolvable` (refuse) |
| `Program` | `Bound`, needs = the agent's own grant | **named, not narrowed** — intersection is the grant unchanged |
| unreadable id / no store (engagement) | `Unresolvable` | refuse |

An **engagement** ceiling is read live from the roster at each dispatch, so
revocation, expiry and narrowing take effect on the next act rather than the
next re-plan. `POST /api/magician/v2/engagements` is the production caller of
`EngagementStore::create` (via `grant_for_work`); `PATCH /api/magician/v2/engagements/{id}`
renews the window without moving the id. See
[`engagements.md`](./engagements.md) and
[`opc-owner-and-reader-surfaces.md`](../magician-api/opc-owner-and-reader-surfaces.md).

A **program** binding exists so the act is filed under `program_id` and scoped
to `EnvelopeScope::Program`. `envelope_scope_for_work` derives that through
`From<&WorkContextKind> for EnvelopeScope`. The need list is the agent's own
grant because no roster owns programs, and inventing a ceiling would be a
caller-supplied authority. See [`approval-envelopes.md`](./approval-envelopes.md).

`root_authority_for_work_with_store` **refuses** `WorkContextKind::Program`
by name: a root cannot carry a program authority until a roster can answer with
a live revision.

Four things bound what the gate reaches:

- **Outward acts only.** Every inward act — a file read, a memory search, a
  shell command — never reaches gate 0. Retrieval containment
  (`docs/components/magician/engagement-scoped-retrieval.md`) is a separate
  gate; `work_retrieval_refusal` also confines a Program carrier, by the same
  unlabeled-corpus argument, without needing a program retrieval-scope form.
- **Not at tool-resolution time.** `execution::restricted_toolset` narrows the
  catalog before the model sees it and does **not** consult a work context. The
  model is still *offered* capabilities this work does not name; the prompt
  summary and the dispatch refusal are how it finds out.
- **No program roster.** Until one exists, production roots never carry
  Program, and a Program binding that does arrive (tests, inherited carriers)
  names the work without shrinking the grant.
- **No delegation carrier.** §2.1 step 4 (the work context travels with the
  delegation) needs a carrier that does not exist yet. This module says *that*
  delegation is required; `delegation_needs` only names the handoff inside a
  refusal message.

`Unbound` is the gate declining to answer, which is why it is a distinct arm
rather than a permissive default. The moment an execution carries a live
engagement, the ceiling applies to every outward act of that execution, with no
further wiring.
