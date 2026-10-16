# Engagements — Contextual Authority

**Module:** `magician/src/magician_v2/engagements.rs`
**Design:** `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md`
**Execution plan:** `docs/archive/plans/2026-08-13-opc-workstream-b-authority-carrier.md`

## What an engagement is

An agent's capability is otherwise static: its `definition.agent.yaml` tools
list, the same every day for every counterparty. An **engagement** makes
capability a property of the *(agent, context)* pair: a bounded, expiring,
revocable grant that exists because a specific piece of outward work (a
program, a counterparty) exists, and ends when it does.

## The record

`EngagementAuthority`: scope (principal/workspace), program, counterparty
label, a **tool ceiling** (effective capability is always an intersection; the
ceiling never grants what the agent's definition lacks), a **delegation team**
(`team[]`, the only agents an engagement-scoped execution may delegate to), a
**mandatory expiry**, and an **authority revision** bumped on every mutation so a
policy snapshot resolved before a narrow or revoke is detected as stale.

An execution carries `EngagementAuthorityRef { engagement_id, authority_revision }`.
A child inherits the pair verbatim and can never supply its own; a child that
could name its own engagement could grant itself one.

## The store

`EngagementStore` keeps one roster per data root (`system/engagements.json`),
published whole through the shared durable writer under a write lock. Liveness
(revoked / expired) is computed against the clock at every read, never cached,
because revocation is the event that invalidates cached state. `narrow()` only
intersects: an engagement shrinks after creation, and broader authority is a new
engagement with its own owner act.

### Minting, from the work rather than from the engagement

`POST /api/magician/v2/engagements` calls
`grant_for_work(principal, workspace, work: &WorkContext, counterparty, team,
now_ms, expires_at_ms)`. It takes the generic carrier
([`work-context.md`](./work-context.md)) rather than a program id and tool
names, so a support triage, recruiting loop or vendor review can mint authority
without this module's vocabulary. The ceiling is the work's
`needs_capabilities` plus `needs_playbooks`, trimmed and with blanks dropped but
never case-normalised: dispatch tests membership with exact, case-sensitive
`contains`, so a re-cased name would authorise nothing while reading as a grant. `work_binding_for_dispatch`
is the inverse mapping, applied at every dispatch.

`program_id_for_work` maps the work kind onto the record's one work field with
an **exhaustive match** (not `WorkContextKind::id()`), so a new kind of work stops
the build until someone decides what it means. The `Engagement` arm is refused:
an engagement id written into `program_id` would be listed and filtered as a
program and merge with any program sharing the id. A grant nested under another
engagement would need a parent field the record does not have.

`create` refuses a blank or U+001F-carrying (the derived-id separator) program id or counterparty label,
and a window already over at `now_ms` (expiry is inclusive, so it would
authorise nothing while looking like a grant).

### Binding a root

`root_authority_for_work`, reached through `create_root_execution_under_work`,
gives a **root** execution a carrier (a child only inherits one from its
parent's durable record). Minting takes `WorkContextKind::Program` and refuses
`Engagement`; binding a root takes `Engagement` and refuses `Program`. The
durable column holds every arm as a `work_context::WorkAuthorityRef`, but no
roster owns programs, so there is no liveness or revision to read, and an
invented revision would be the caller-supplied authority §4.2c row 5 forbids.
Neither coerces the arm it cannot serve.

### Renewal

`extend_expiry(engagement_id, new_expires_at_ms, now_ms)` moves the window
forward on the **same** engagement. Re-granting would mint a new ULID, and every
execution, pause blob, envelope, disclosure and retrieval partition bound to the
old id would keep naming a lapsed authority.

It is forward-only and never touches a terminal state: a revoked engagement
refuses; an already-expired one refuses (moving the window would re-authorise
denied acts); an earlier expiry refuses (shortening is a withdrawal, and
`revoke` is the act whose instant the audit records). An identical replay
returns `Ok(false)`, writing nothing and leaving the revision unmoved.

A corrupt roster **refuses to open**, unlike the device policy store's
default-fallback. A defaulted roster under live engagement ids would deny
everything with `UnknownEngagement`: safe but undebuggable. Loud refusal keeps
the evidence.

## The dispatch-boundary core

`authorize_engagement_dispatch()` / `authorize_engagement_delegation()` run
after trust policy and before side effects whenever the execution context
carries a ref. Three properties, each a row of the §4.2c matrix:

- **Unconditional when a ref is present.** Unlike the snapshot gate, which skips
  its ceiling check when its guard slot is `None`, this check never fails open.
- **Fails closed on store absence**: an unavailable store reads as denied.
- **Live, not snapshot**: the store's current ceiling is enforced; the carried
  revision only detects staleness (`revision_changed`), telling the caller to
  re-resolve its policy snapshot before the next provider decision.

The process-wide handle is installed at boot with
`install_global_engagement_store`, like the device bridge hub and device
governance. Matrix tests: `magician/tests/engagement_authority_matrix.rs` (row
12, capture-mode, is waived: no execution flag distinguishes a capture run).

## The carrier

`EngagementAuthorityRef` rides the execution all the way down and cannot be
forged by the model:

- **Where it lives.** A sealed field on `AgenticContext` /
  `AgenticContextOverrides`, set only by server-side spawn code. The durable
  column on `ExecutionRun` is the **generic** carrier,
  `work_context::WorkAuthorityRef { work: WorkContextKind, authority_revision }`,
  so a recruiting, support-triage or vendor-management run is confined through
  the same field. The orchestrator narrows it to an `EngagementAuthorityRef` at
  the two seams that enforce an engagement ceiling (context hydration,
  delegation admission) and **refuses the run** for an arm it cannot enforce.
  It is *not* on `AgentInvocationContext` (cleared on every owner transition)
  nor on `DelegationTargetRequest` (deserialized from model args with
  `deny_unknown_fields`, so a model-supplied engagement field is a parse error).
- **Inheritance.** A delegated child's carrier is copied verbatim from the
  parent `ExecutionRun`, never from the model's request. The stored record is
  compared with what was asked for (`verify_persisted_work_authority`), because
  the storage trait's default authority-carrying creation drops the grant; a
  child created without its parent's ceiling is refused.
- **Persistence.** Carried in `AgenticPauseState` and folded into its
  `authorization_hash`, so a tampered pause blob cannot swap the engagement;
  hydrated from the durable record (not overrides) on restart.
- **Projection (layer 1).** `resolve_effective_tool_policy_snapshot` narrows
  `dispatch_tool_names`/`provider_specs`/`delegation_targets` to the live
  ceiling and team. The snapshot cache key *and* catalog digest carry the
  engagement revision, so a revoke/narrow never serves a stale snapshot. Any
  authority denial fails closed to an empty ceiling.
- **Dispatch (layer 2 — the boundary).** `execute_action_inner` runs the live
  check after the trust gate and before any side effect, so a fabricated tool
  name layer 1 never advertised still dies here. Delegation admission
  (`spawn_delegated_children_from_runtime`) re-checks the live `team[]` before
  any child row exists.

The chat surface carries no engagement ref, so it has no dispatch mirror.

## What the carrier does not bound

Capability, not context. An execution bound to one engagement could still
retrieve every other engagement's material, because agent-scoped memory
separates agents, not an agent's own engagements. That containment is a separate
boundary: [`engagement-scoped-retrieval.md`](./engagement-scoped-retrieval.md) (§5A.2).
