# Consequence classes — what an act costs if it is wrong

Plan: `docs/archive/plans/2026-08-07-opc-approval-envelopes.md` §3.

Every `requires_approval` rule and every outward act is tagged with a class.
Consumers: the outward dispatch gate, the assertions store, the envelope
resolver, the approval waiver path and the preference-alignment check.

## Why a class rather than a judgement

Approval matches on `(tool, action)`, a **mechanism**. What an owner consents to
is an **outcome**: *"approach accelerators and correspond with them."* Nobody
wants to be asked about `agentmail-send` seventeen times. "Important" is a
judgement and cannot be computed; **consequence class is a property of the
act**, which keeps the model generic.

## What this axis deliberately is not

**Reversibility.** A sent email is *correctable*, not reversible: once read, the
disclosure has happened. Grouping disclosure with acts that can truly be undone
would license exactly the acts most worth thinking about.

## The five classes

| class | examples | gate? | standing envelope | reviewed batch |
|---|---|---|---|---|
| `private_local` | draft written, research done, deck built but unsent | **no** | — | — |
| `bounded_communication` | a message to an identity already in the engagement | yes | **yes** | yes |
| `confidential_disclosure` | financials, a data room, non-public material | yes | never | yes |
| `submission_or_publication` | application submitted, demo published | yes | never | yes |
| `commitment_or_transaction` | terms agreed, money moved, anything binding | yes | **never** | **never** |

The line that matters is between *bounded communication* (talking to someone you
already agreed to talk to) and *disclosure*, a one-way transfer however
correctable the wording.

A **reviewed batch** may go wider than a standing envelope because the owner saw
the actual instances ("submit to these ten, here is the template"). A batch is
exhausted by its own list and nothing can be added, which is the only reason it
may carry disclosure and submission.

**Commitment is never covered, batch or otherwise.** The rule has no exception
and its own test.

## How an act is classified

`consequence_class_for(capability, action)`:

1. a **known commitment** → `commitment_or_transaction`;
2. an act known to be **outward** with no class → `commitment_or_transaction`,
   **failing closed** (a wrong answer costs a prompt, not an unapproved send);
3. anything else → `private_local`, no gate.

Case 3 keeps this a classification, not a behaviour change: ordinary local
capabilities are untouched.

### And the case where case 3 is wrong

`consequence_class_for_approval_rule` is the same function except it never
answers `private_local`, because the act in front of it **already has a gate**:
somebody wrote `requires_approval` for it. Answering `private_local` there would
mean that once the envelope resolver decides from the class, an approval the
author asked for stops being asked. An unclassified gated act therefore fails
closed to `commitment_or_transaction`, like case 2.

The fix for an over-strict answer is to classify the action, not to soften the
fallback. A test keeps `websearch`, `jq`, `office-word`, `duckdb` and
`gmail/list_messages` ungated, because tagging an inventory must not quietly
become gating it.

Commitment actions are listed **explicitly**, not matched on words like "order"
or "pay": a lookalike name would be gated by accident and an unlike one
(`checkout`, `book_table`) missed.

## Shipped rules are checked, not listed

`every_shipped_approval_rule_has_a_consequence_class` walks the shipped
definitions (a hardcoded list would agree with itself) and asserts the
**invariant** as well as the snapshot: every rule behind `requires_approval` must
classify as something that `requires_gate()`. Examples: `executive-assistant` ·
`swiggy-mcp` / `zepto-mcp` (payment orders, checkout, table booking) are
**commitments**, so no envelope may cover them; `ceo` ·
`propose_program_missions` is unclassified and fails closed to commitment;
`agentmail-send`, `gmail`, `calendar`, `imessage_send` rules are bounded
communication.

### Classifying a dispatch, not a pair

`consequence_class_for_dispatch(capability, action, params)` is what a **write
point** should use. `consequence_class_for` asks the token-only outward
classifier, which misses the `raw` escape action `gmail` and `presto-gmail`
ship: `gmail action=raw args=["+send", …]` sends mail while its token says
nothing, so the pair form would answer `private_local`, **the one class with no
gate**. The dispatch form asks `outward_dispatch_class`
(`docs/components/magician/outward-actions.md`), which treats an unbounded argv
passthrough on an outward-capable capability as outward. `consequence_class_for`
stays right where only a pair exists, such as a static approval rule.

### A browser form submission is `submission_or_publication`

`browser`'s **explicit submission** tokens, `submit` and `submit_form` only,
classify as `form_submission` (`docs/components/magician/outward-actions.md`),
which maps to **`submission_or_publication`**, not bounded communication.

Classifying `click`, `press`, `key`, `eval`, `find` or `batch` (any of which can
submit a form) would break the browser: a classified act meets §4A's
bindability refusal, `browser` has no restricted form, so every click would be
refused and the gate would get switched off. The residue: **the browser has no
`submit` token**, so a form submitted by clicking is not seen as a submission.
Closing that needs an explicit submit action or a restricted form for `browser`;
capture mode covers it meanwhile.

The difference is load-bearing. Bounded communication is the one class a
**standing** envelope may cover, so filing a submission there would let "you may
talk to people in this engagement" also authorise pressing Submit on an
application the owner never saw. Reading, clicking through and navigating stay
`private_local`.

## Where the class is consumed today

The outward dispatch gate classifies each act and the assertions store records
it (`docs/components/magician/outward-assertions.md`). The data room's
`disclosure_bridge` (`magician-learning`) records every grant as
`confidential_disclosure`; no classified act produces that class through
`consequence_class_for` today, since every outward capability it knows is mail,
a message or a calendar invite (bounded communication) or a browser `submit`.
`preference_alignment::classify_dispatch` checks an act against owner
preferences only when it is outward or its `consequence_class_for_dispatch` is
not `private_local`. The **caller** classifies
and passes the class in, so the evidence subsystem does not depend on the
classifier and a caller with its own scheme can use the store unchanged.

The envelope resolver decides from it
(`docs/components/magician/approval-envelopes.md`), in shadow, and so does the
**approval** path, through `resolve_approval_waiver`, which uses the strict
`consequence_class_for_approval_rule`, not the dispatch classifier.

Related pieces: the envelope, its limits and consumption ledger
(`approval-envelopes.md`); shadow mode (`log_envelope_shadow` logs
`[ENVELOPE-SHADOW]` in the outward dispatch gate, observation only); restricted
outward actions (§4A, gate 1 of the outward gate, `restricted-actions.md`); the
owner surface `/api/magician/v2/approval-envelopes`
(`magician-api/src/approval_envelopes_api.rs`); and the approval path:
`run_standing_consent` builds the gate, `ApprovalGate::check_against` takes it,
and `resolve_approval_waiver` classifies from the step's own capability and
action, so any act behind `requires_approval`, outward or not, is classified.

## What is still inert, and why

`run_standing_consent` (`execution/agentic/executor.rs`) returns `None` when
`envelope_mode()` is `Off`: the default when `approval_envelopes.mode` is unset
or unrecognised (seed: `"off"`), installed once at config load (first install
wins). Unless config says otherwise, the waiver
path is never entered and no `[ENVELOPE-WAIVED]` line is emitted; classification
runs at the outward dispatch gate (unconditional) and in shadow logging
(mode-gated).

Two guarantees hold in every mode, and are why the strict classifier exists: it
never answers `private_local`, and an unclassified act falls closed to
`commitment`, which no envelope may waive.
