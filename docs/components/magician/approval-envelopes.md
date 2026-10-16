# Approval envelopes — consent per outcome, not per act

Design (archived): `docs/archive/plans/2026-08-07-opc-approval-envelopes.md`.
Module: `magician/src/magician_v2/approval_envelopes/`. HTTP:
`magician-api/src/approval_envelopes_api.rs`. Classification:
[consequence-classes](consequence-classes.md).

**Default-off.** `approval_envelopes.mode` (`off` / `shadow` / `enforcing`) is
installed once at config load; an unreadable value is `off`, and the first
install wins so a reload cannot promote shadow to enforcing.

## A primitive, not an OPC feature

Nothing in the model is specific to the one-person company, so this is a
**sibling of `resource_authority`**, not something under `agents/`:

| | governs | grant | ledger |
|---|---|---|---|
| `resource_authority` | how much of a **commodity** an act may spend | budget row | token ledger |
| `approval_envelopes` | what class of **outcome** an act may cause | envelope | consumption ledger |

Both resolve at dispatch, on the same tuple, from durable grants.

## The one inversion

**Resource Authority fails open:** an action with no budget row runs uncounted
(right for a tool a user explicitly invoked). **Envelopes fail closed:** no
envelope means ask. These acts are autonomous, self-initiated and outward, so a
missing envelope reads as silence, not permission. Hence `envelope_mode()`
defaults to `Off`: a process that failed to wire config must not start
authorising sends.

## What may decide coverage

**Only facts the agent does not author.** Agent self-declaration and LLM
classification are both injection surfaces; only deterministic predicates on the
act's shape decide. This is enforced by type: `ActFacts` is the only decision
input and has no field for the agent's account. `effective` comes from
`resolve_effective_action`, `consequence_class` from the classifier,
`engagement_identities` from the engagement store, `now` from the clock.

Boundary predicates: `recipient_in_engagement`, `capability_in_set`,
`no_attachment_outside_ledger`, `exact_content`. Counts, windows and value
ceilings are **limits**, checked on every resolution regardless of grant, never
predicates.

## The order of the gates, and why

```
1. is the act BINDABLE?            §4A — absolute
2. may this KIND carry the class?  commitment: never, by anything
3. was the class GRANTED?
4. revoked? expired?
5. limits
6. batch instances, then boundary predicates
```

Steps 1–4 are absolute; no envelope configuration overrides them. Limits and
predicates come after because they are what an owner tunes.

**Step 1.** `is_bindable()` is false whenever an action accepts an escape hatch
([effective-action](effective-action.md)), which is true of every **raw** sending
skill, so no envelope covers those. An act through `restrict`
([restricted-actions](restricted-actions.md)) is bindable by construction.
Checking first means no envelope can cover an act whose arguments could still
widen after inspection.

**Which envelope pays.** `resolve_any` evaluates candidates soonest-expiring
first (never-expiring last, id breaks ties), so lapsing headroom is spent first
and grant order does not matter. It answers bindability once, before iterating,
so an unbindable act next to an expired envelope reports the unbindable reason,
not `Expired`.

## Degradation is always toward asking

Expiry, revocation and exhaustion all produce `NotCovered`; no state lets an
envelope silently stop applying while the act proceeds. Expiry is inclusive.
Re-granting after expiry mints a **new** envelope (the id folds in the grant
instant), so re-issuing the same words cannot undo expiry.

`NotCoveredReason` distinguishes eleven "ask" cases, and `resolve_any` reports the
**most specific** refusal across candidates, so an owner is not told "no
envelope" when theirs is merely exhausted.

**Audit.** A `Covered` verdict names the envelope and every matched predicate.
Revocation takes effect on the next act with nothing to clean up, because nothing
is copied out of the envelope.

## Storage

An envelope's file is its log: first line the grant, later lines consumptions
and a revocation; current state is the fold. Nothing is edited in place.
Grant, revocation, resolution and reservation share a hardened per-store-scope
AgentStorage lock. Enforcing resolution rereads and debits under that lock, so
parallel callers cannot spend the same remaining slot; shadow stays read-only.
Grant ids include the complete authority and grant instant. Exact retries repair
an interrupted scope-index publication.

- Debits are **idempotent on the act ref**: retries cannot double-consume, and a
  replaying act is not refused by the cap its own earlier attempt filled. The
  fold deduplicates via a set, so a ledger with no `max_acts` does not go
  quadratic on the hot path.
- A per-scope index is written with the envelope, never derived by scanning,
  because resolution runs on every outward dispatch.

**Refused at the door.** The store will not mint an envelope that could never
authorise anything:

- a commitment in `covers[]`, by any kind;
- confidential disclosure, submission or publication in a **standing** envelope;
- `private_local` (needs no gate);
- an **empty** reviewed batch ("empty means everything" is the wrong reading);
- a **standing envelope with no expiry** (a blanket yes). Reviewed batches are
  exempt: they exhaust their own list.

**Reviewed batches** require an exact `parameters://blake3:<hash>` content ref
per instance. `resolve_effective_action` computes it from canonical capability,
action and complete arguments; HTTP preview exposes the ref. Every recipient
must match a reviewed instance with that same ref and known immutable attachment
provenance. An instance can reserve one act; a retry must keep the same exact
binding. Consumption rows record content refs and refuse changed same-act
metadata. Legacy rows without a content binding cannot authorise a replay, and
legacy recipient-only batches ask for review. The optional `exact_content`
standing predicate accepts a finite owner-reviewed set of these refs. This pins
arguments; it does not fix the bytes of a mutable referenced attachment.

### Campaign capture policies

Owner campaign capture policies live in this module. They bind existing task
context and counterparty identity ids, reviewed contract hashes, sender,
channel, message kind and content-contract identity.

- Activation and revocation use exact version checks. Capture reservations have
  finite total, UTC-day and recipient ceilings; pending or rejected slots are
  not refunded. The accepted run's artifact and every file hash are verified
  before binding.
- Automatic capture needs an owner-approved `content_template`: exact literal
  parts plus named bounded slots, filled from finite choices or complete,
  hash-pinned UTF-8 sources (optionally an exact JSON string pointer). A sealed
  JSON slots artifact must declare every slot once; the payload must equal the
  full rendering; every extra file must match an approved attachment hash. Any
  template, source or rendering failure requires exact review.
  `POST /relentless/campaigns/preview` computes the template/content hash for
  owner inspection and activates nothing.
- Clean approvals are counted per identity under the current campaign version,
  and only exact owner-approved validated templates earn them. Novel-content
  approval or rejection resets that identity's count. Each identity's first
  capture asks, even if another recipient has earned simulated coverage.
- Automatic policy pins normalized recipient addresses and refuses address
  drift; per-recipient caps count normalized addresses across identity aliases.
  Counts follow review-event order, policy changes reset simulated coverage,
  and a rejected audit suspends automatic capture until a successor owner
  policy.
- The owner API/UI is under `/relentless/campaigns`. Capture is local: no live
  dispatch or production trust is granted, and production envelope minting and
  typed receipt-fed trust are unfinished. Counterparty addresses may be
  unverified for local capture; that does not make them safe to contact.
- Pending exact review projects a stable notice through the scoped attention
  service. Dismissal or expiry grants nothing; owner review closes it, and boot
  recovery retries publication without duplicating the capture or its slots.
- An optional owner-selected task `capture_handoff` pins campaign, version and
  identity in its admission snapshot and root owner key, so terminal projection
  can attach the accepted artifact automatically. Historical receipt lookup
  survives expiry and successors, refuses ambiguous recipient bindings and
  grants no fresh coverage. A current-policy or capacity failure leaves the
  accepted local artifact intact, with an open item and one durable owner
  notice.

The resolver re-checks the class rules anyway, so a store written by an older
binary cannot become enforceable by a newer one.

## The dispatch gate

`EnvelopeGate::evaluate(mode, ctx)` is the only place resolver, store and mode
meet, which keeps the decision function testable without a store.
`DispatchContext` supplies engagement identities, value and attachment count
rather than looking them up, so the gate does not depend on the engagement
store, billing model or asset ledger.

| mode | resolves | debits | authorises |
|---|---|---|---|
| `Off` (default) | no | no | no |
| `Shadow` | yes | **no** | no |
| `Enforcing` | yes | yes | when covered |

- **Shadow never debits.** An act that was still asked about consumed nothing;
  debiting would spend caps on hand-approved acts and falsify the comparison
  shadow exists for.
- **Debit before the act.** A crash between debit and act costs one act of
  headroom; the reverse order would lose the record and leave phantom headroom.
  The idempotent debit makes the retry resume.
- **No stable id, no authorisation.** A blank `act_ref` would put every
  unidentified act in one debit slot, so `UnidentifiedAct` refuses before the
  store is read.
- A resolution that fails to read is logged and dropped; it never reads as
  coverage.

**Shadow wiring.** The outward dispatch gate logs `[ENVELOPE-SHADOW]` with the
act ref, resolved scope (`envelope_scope`) and `work_identities` count.
`envelope_work_facts` (agentic executor) derives both from the work the act is
bound to: the scope via `From<&WorkContextKind> for EnvelopeScope` (program →
`Program`, engagement → `Engagement`) behind `envelope_scope_from_id`, which
refuses a blank id or one containing the field separator; the identities are
the work's verified counterparty addresses (`counterparties::audience_for_label`,
stamped with the engagement's expiry). Unbound or unresolvable work logs
`<unscoped>` and resolves `no_envelope`; with envelopes `off` nothing is
read.

## The owner surface

`OwnerView` is a projection plus two commands, with no HTTP or rendering, so
CLI, API and tests read what the resolver decided. It answers: what did I
authorise, how much is left, what was done under it, how do I stop it.

Routes under `/api/magician/v2` (registered by
`configure_approval_envelope_routes` from `magician-bin/src/main.rs`), all
reading `OwnerView`:

- `GET /approval-envelopes`, `POST /approval-envelopes`
- `POST /approval-envelopes/preview`
- `POST /approval-envelopes/{id}/revoke`, `GET /approval-envelopes/{id}`

Rules:

- **Standing is derived, never stored** (a stored status drifts when an envelope
  expires unwritten). With several applicable: revoked over expired over
  exhausted.
- **An unset cap is `None`, not `0`** (`Headroom::remaining()`); over-consumption
  saturates.
- **A reviewed batch is exhausted** once every named instance is reached.
- **Revoked and expired envelopes stay listed.** Revocation is idempotent.

## Approval waiver

`resolve_approval_waiver` answers: **may this approval prompt be waived?**
Every approval ask converges on `requires_approval`, so this generalises without
enumeration; envelopes sit in front of it, and it stays the fallback.

- **Strict classifier.** Waiver candidates are classified with
  `consequence_class_for_approval_rule`, which never answers `private_local` and
  fails closed to commitment for anything unclassified, so unclassified is never
  waived.
- **Shadow never waives**; its reason is `not_enforcing`.
- **A commitment is never waived**, in any mode, by any envelope (guaranteed by
  the classifier and again by the resolver).
- The context type has no consequence-class field, so a caller cannot pass
  `bounded_communication` for a payment.
- **Resolving consumes; previewing does not.** A waiver is the act's
  authorisation, so it debits. Surfaces asking "would this need a prompt?" must
  use `preview_approval_waiver` (shadow semantics, no debit); calling the
  committing path from a render loop would spend an act per render. Previews are
  advisory; with envelopes off they resolve nothing. Every outcome yields an
  `audit_line()`.

**Where it is called.** `ApprovalGate::check_against` (the interpreter of
`constraints.requires_approval`) takes an optional `StandingConsent`: a pending
approval covered by a live envelope is dropped **and debited**; no consent is
byte-for-byte `check`. The executor supplies consent only at the per-action
confirmation gate. Handover, delegation and sub-goal gates do not (their acts
classify as `orchestrator` → commitment → always ask). The in-turn follow-up
probe stays on plain `check` on purpose: a candidate it lets through executes
without reaching the deciding gate, so an advisory answer there would bypass the
debit.

**The act key.** `act_ref_for_step` hashes execution scope, capability, action
and ordered arguments. It excludes the step id (a resume would double-spend),
the step goal (model free text), and raw `HashMap` order (differs per process).
An absent or blank execution scope yields an empty key, refused as
`unidentified_act`.

**Preview over HTTP.** `POST /api/magician/v2/approval-envelopes/preview` takes
scope, capability, action, arguments and `act_ref` (required; an invented key
answers about a different act) and returns a **reason**, not a verdict. Because
preview is shadow, `not_enforcing` means "an envelope covers it; only the
posture stands between it and a waiver"; every other reason names why no
envelope covered it.

## Known limits

- **Physical dispatch revocation.** Atomic reservation orders debits with
  revocation. Channels still need a final live-policy check at physical I/O and
  typed receipt reconciliation before standing campaign consent can be enabled.
- **Engagement identities** are empty whenever the counterparty register,
  principal, workspace or engagement roster is unavailable, or the label has no
  verified address; `recipient_in_engagement` then refuses every recipient.
- **Attachments.** `attachments_outside_ledger` is `None` (no caller computes
  it); `None` means unknown, and the predicate fails closed on it.
- **Goal scope.** `EnvelopeScope::Goal` can be granted over HTTP
  (`parse_envelope_scope`), but `WorkContextKind` has only program and
  engagement work, so a goal-scoped envelope is never found at dispatch.
- No UI in `ui/` calls the HTTP routes.
