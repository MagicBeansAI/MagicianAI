# Engagements — Contextual Authority

**Module:** `magician/src/magician_v2/engagements.rs`
**Design:** `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md`
**Execution plan:** `docs/archive/plans/2026-08-13-opc-workstream-b-authority-carrier.md`

## What an engagement is

Capability in this system is a static property of an agent: its
`definition.agent.yaml` tools list, the same on every day for every
counterparty. An **engagement** makes capability a property of the
*(agent, context)* pair — a bounded, expiring, revocable grant that exists
because a specific piece of outward work (a program, a counterparty) exists,
and stops existing when it does.

## The record

`EngagementAuthority`: scope (principal/workspace), program, counterparty
label, a **tool ceiling** (effective capability is always an intersection —
the ceiling never grants what the agent's own definition lacks), a
**delegation team** (`team[]` — the only agents an engagement-scoped
execution may delegate to), a **mandatory expiry**, and an
**authority revision** bumped on every mutation. The revision is how a policy
snapshot resolved under one state of the engagement is detected as stale
after a narrow or a revoke.

`EngagementAuthorityRef { engagement_id, authority_revision }` is what an
execution carries. Two fields by design: a child inherits the pair verbatim
and can never supply its own — a child that could name its own engagement
could grant itself one.

## The store

`EngagementStore` — one roster per data root (`system/engagements.json`),
published whole through the shared durable writer under a write lock.
Liveness (revoked / expired) is computed against the clock at every read,
never cached, because revocation is exactly the event that invalidates
cached state. `narrow()` only intersects — an engagement shrinks after
creation; broader authority is a new engagement with its own owner act.

### Minting, from the work rather than from the engagement

`POST /api/magician/v2/engagements` calls
`grant_for_work(principal, workspace, work: &WorkContext, counterparty, team,
now_ms, expires_at_ms)`. It takes the generic carrier —
[`work-context.md`](./work-context.md) — rather than a program id and a bag of
tool names, so a support triage, a recruiting loop or a vendor review mints
authority without learning this module's vocabulary. The ceiling is the work's
`needs_capabilities` plus `needs_playbooks`, recorded **verbatim**: the
dispatch check tests membership with exact, case-sensitive `contains`, so a
name normalised on the way in would authorise nothing while reading as a grant.
`work_binding_for_dispatch` maps a live ceiling back into a `WorkContext` at
every dispatch; this is that mapping inverted.

`program_id_for_work` maps the work kind onto the record's one work field with an
**exhaustive match**, not through `WorkContextKind::id()`. That accessor exists
so guards need not match on the arms; a mapping wants the opposite shape, so a
third kind of work stops the build until somebody decides what it means. The
`Engagement` arm is refused rather than mapped: the record has one work field,
`program_id`, and an engagement id written into it would be listed and filtered
as a program and would merge with any program sharing the id. A grant nested
under another engagement needs a parent field this record does not have.

`create` refuses a blank or separator-carrying program id or
counterparty label, and a window already over at `now_ms` — expiry is inclusive,
so such an engagement would authorise nothing from birth while sitting in the
roster looking like a grant.

### Binding a root

`root_authority_for_work`, reached through `create_root_execution_under_work`,
gives a **root** execution a carrier (a child only ever inherits one from its
parent's durable record). The two halves refuse opposite arms of the same
generic carrier: minting takes `WorkContextKind::Program` and refuses
`Engagement`, because the record's one work field is a program id; binding a
root takes `Engagement` and refuses `Program`. The durable column holds every
arm as a `work_context::WorkAuthorityRef`, but no roster owns programs, so
there is no liveness answer and no authority revision to read, and a revision
this code invented would be the caller-supplied authority §4.2c row 5 forbids.
Neither coerces the arm it cannot serve.

### Renewal

`extend_expiry(engagement_id, new_expires_at_ms, now_ms)` moves the window
forward on the **same** engagement. Re-granting instead mints a new ULID, and
every execution, pause blob, envelope, disclosure and retrieval partition already
bound to the old id goes on naming a lapsed authority — nothing repoints them,
because nothing was ever copied anywhere.

Forward-only, and never onto a terminal state. A revoked engagement refuses. An
engagement the clock has already closed refuses, because moving that window would
re-authorise acts that were already being denied. An earlier expiry refuses too:
shortening a window is a withdrawal, and `revoke` is the one act whose instant
the audit records. An identical replay resumes — `Ok(false)`, nothing written,
the revision unmoved.

A corrupt roster **refuses to open** — deliberately the opposite call from
the device policy store's default-fallback. A defaulted policy is the safe
default; a defaulted engagement roster under live engagement ids would deny
everything with `UnknownEngagement`, which is safe but undebuggable. Loud
refusal preserves the evidence.

## The dispatch-boundary core

`authorize_engagement_dispatch()` / `authorize_engagement_delegation()` —
called after trust policy and before side effects, whenever the execution
context carries a ref. Three properties, each a row of the §4.2c matrix:

- **Unconditional when a ref is present.** The pre-existing snapshot gate
  skips its ceiling check when its guard slot is `None`; this check never
  inherits that fail-open shape.
- **Fails closed on store absence** — an unavailable authority store reads
  as denied, never as unrestricted.
- **Live, not snapshot** — the ceiling enforced is always the store's
  current one; the carried revision is compared to observe staleness
  (`revision_changed`), which tells the caller to re-resolve its policy
  snapshot before the next provider decision.

Process-wide handle via `install_global_engagement_store` at boot, the same
pattern as the device bridge hub and device governance.
Matrix tests: `magician/tests/engagement_authority_matrix.rs` (row 12,
capture-mode, is waived: no execution flag distinguishes a capture run).

## The carrier

`EngagementAuthorityRef` rides the execution the whole way down and is
model-unforgeable by construction:

- **Where it lives.** A sealed field on `AgenticContext` /
  `AgenticContextOverrides` (set only by server-side spawn code). The durable
  column on `ExecutionRun` is the **generic** carrier —
  `work_context::WorkAuthorityRef { work: WorkContextKind, authority_revision }`
  — so a recruiting, support-triage or vendor-management run is confined
  through the same field instead of fabricating an engagement id. The
  orchestrator narrows it to an `EngagementAuthorityRef` at the two seams that
  enforce an engagement ceiling (context hydration, delegation admission) and
  **refuses the run** for an arm it cannot enforce rather than reading it as no
  authority. Deliberately *not* on `AgentInvocationContext`
  (cleared on every owner transition) and *not* on `DelegationTargetRequest`
  (deserialized raw from model args; `deny_unknown_fields`, so a
  model-supplied engagement field is a parse error, not a silent grant).
- **Inheritance.** A delegated child's carrier is copied verbatim from the
  parent `ExecutionRun`, never from the model's delegation request; a child
  cannot name or widen its own work. The record the store returns is compared
  with what was asked for (`verify_persisted_work_authority`), because the
  storage trait's authority-carrying creation is a provided method whose
  default drops the grant — a child created without its parent's ceiling is
  refused rather than run.
- **Persistence.** Carried in `AgenticPauseState` and folded into its
  `authorization_hash`, so a tampered pause blob cannot swap the engagement;
  hydrated from the durable record (not overrides) on restart.
- **Projection (layer 1).** `resolve_effective_tool_policy_snapshot` narrows
  `dispatch_tool_names`/`provider_specs`/`delegation_targets` to the live
  ceiling and team; the snapshot cache key *and* catalog digest carry the
  engagement revision, so a revoke/narrow can never serve a stale pre-change
  snapshot. Any authority denial fails closed to an empty ceiling.
- **Dispatch (layer 2 — the boundary).** `execute_action_inner` calls the
  live check after the trust gate and before any side effect, unconditional
  when a ref is present. This is provable independently of projection: a
  fabricated tool name layer 1 never advertised still dies here. Delegation
  admission (`spawn_delegated_children_from_runtime`) re-checks the live
  `team[]` before any child row exists.

The chat surface carries no engagement ref, so it has no dispatch mirror.

## What the carrier does not bound

Capability, not context. An execution bound to one engagement could still
retrieve every other engagement's material, because agent-scoped memory
separates agents from each other and not an agent's own engagements from one
another. That containment is a separate boundary:
[`engagement-scoped-retrieval.md`](./engagement-scoped-retrieval.md) (§5A.2).
