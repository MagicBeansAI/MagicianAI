# Consequence classes — what an act costs if it is wrong

OPC phase 3. Plan: `docs/archive/plans/2026-08-07-opc-approval-envelopes.md` §3.

Every `requires_approval` rule and every outward act is tagged with a class.
Consumers: the outward dispatch gate, the assertions store, the envelope
resolver, and the approval waiver path.

## Why a class rather than a judgement

Approval today matches on `(tool, action)` — a **mechanism**. What an owner
consents to is an **outcome**: *"approach accelerators and correspond with
them."* Nobody wants to be asked about `agentmail-send` seventeen times.

"Important" is a judgement and cannot be computed. **Consequence class is a
property of the act**, which is what makes the model generic rather than a
fundraising special case.

## What this axis deliberately is not

**Reversibility.** A sent email is *correctable*, not reversible: once it has
been read, the disclosure has happened and no follow-up undoes it. A taxonomy
that grouped disclosure with things that can genuinely be taken back would
license exactly the acts most worth thinking about.

## The five classes

| class | examples | gate? | standing envelope | reviewed batch |
|---|---|---|---|---|
| `private_local` | draft written, research done, deck built but unsent | **no** | — | — |
| `bounded_communication` | a message to an identity already in the engagement | yes | **yes** | yes |
| `confidential_disclosure` | financials, a data room, non-public material | yes | never | yes |
| `submission_or_publication` | application submitted, demo published | yes | never | yes |
| `commitment_or_transaction` | terms agreed, money moved, anything binding | yes | **never** | **never** |

The line that matters is between *bounded communication* — talking to someone you
already agreed to talk to — and *disclosure*, a one-way transfer of information
however correctable the wording was.

A **reviewed batch** goes wider than a standing envelope because the owner saw
the actual instances. *"Submit to these ten, here is the template, here is the
list"* is not pre-authorising an unknown act. A batch is exhausted by its own
list and nothing can be added to it, which is the only reason it may carry
disclosure and submission at all.

**Commitment is never covered, batch or otherwise.** That rule has no exception
anywhere in the plan, and it has its own test.

## How an act is classified

`consequence_class_for(capability, action)`, in three cases:

1. a **known commitment** → `commitment_or_transaction`;
2. an act known to be **outward** with no class assigned → `commitment_or_transaction`,
   **failing closed**. Being wrong here costs a prompt; being wrong the other way
   costs a send nobody approved;
3. anything else → `private_local`, needing no gate.

Case 3 is what keeps this a classification rather than a behaviour change: an
ordinary local capability is untouched, exactly as today.

### And the case where case 3 is wrong

`consequence_class_for_approval_rule` is the same function with one difference:
it can never answer `private_local`, because the act in front of it **already
has a gate** — somebody wrote `requires_approval` for it.

That answer is correct for a capability nobody gated and actively harmful for a
rule an author deliberately wrote: `private_local` needs no gate, so the moment
the envelope resolver decides from the class, an approval the author asked for
stops being asked. An unclassified gated act therefore fails closed to
`commitment_or_transaction`, the same direction case 2 already takes.

The intended response to an over-strict answer is to classify the action
deliberately, not to soften the fallback. There is a test that
`websearch`, `jq`, `office-word`, `duckdb` and `gmail/list_messages` stay
ungated, because tagging an inventory must not quietly become gating it.

Commitment actions are listed **explicitly**, not pattern-matched on words like
"order" or "pay". A capability whose name merely resembles a transaction would be
gated by accident, and one that does not resemble it at all — `checkout`,
`book_table` — would be missed. Both errors are worse than a list somebody has to
maintain deliberately.

## Shipped rules are checked, not listed

`every_shipped_approval_rule_has_a_consequence_class` walks the shipped
definitions (a hardcoded list would agree with itself and miss the next rule),
and asserts the **invariant** as well as the snapshot: every rule behind
`requires_approval` must classify as something that `requires_gate()`. A
snapshot alone invites updating the expected list until it matches. Example
outcomes: `executive-assistant` · `swiggy-mcp` / `zepto-mcp` (payment orders,
checkout, table booking) are **commitments**, so no envelope may ever cover them;
`ceo` · `propose_program_missions` is unclassified and fails closed to
commitment; `agentmail-send`, `gmail`, `calendar`, `imessage_send` rules are
bounded communication.

### Classifying a dispatch, not a pair

`consequence_class_for_dispatch(capability, action, params)` is what a **write
point** should use. `consequence_class_for` asks the token-only outward
classifier, which misses the `raw` escape action that `gmail` and `presto-gmail`
ship: `gmail action=raw args=["+send", …]` sends an email while its token says
nothing, so the pair form answers `private_local` — **the one class needing no
gate at all** — for an act that reached a person.

The dispatch form asks `outward_dispatch_class` instead
(`docs/components/magician/outward-actions.md`), which treats an unbounded argv
passthrough on an outward-capable capability as outward whatever the token says.

`consequence_class_for` stays correct where only a `(capability, action)` pair
exists — a static approval rule has no arguments to inspect.

### A browser form submission is `submission_or_publication`

`browser`'s **explicit submission** tokens — `submit` and `submit_form`, and
only those — classify as `form_submission`
(`docs/components/magician/outward-actions.md`), which maps here to
**`submission_or_publication`** and not to bounded communication.

Classifying `click`, `press`, `key`, `eval`, `find` or `batch` (any of which
*can* submit a form) would break the browser: a classified act meets §4A's
bindability refusal, `browser` has no restricted form, so every click —
including following a link — would be refused, and a gate that refuses every
call gets switched off. The residue: **the browser has no `submit` token**, so a
form submitted by clicking is not seen as a submission. Closing it needs an
explicit submit action or a restricted form for `browser`; capture mode stands in
front of it meanwhile.

The difference is load-bearing rather than cosmetic. Bounded communication is the
one class a **standing** envelope may cover; filing a submission there would let
a blanket *"you may talk to people in this engagement"* consent also authorise
pressing Submit on an application the owner never saw. Reading a page, clicking through
one and navigating stay `private_local`, so the classification did not turn the
browser into a permanently gated capability.

## Where the class is consumed today

The outward dispatch gate classifies each act and the assertions store records
it (`docs/components/magician/outward-assertions.md`). The **caller** classifies
and the store records: passing the class in rather than deriving it keeps the
evidence subsystem free of a dependency on the classifier's internals, and lets a
caller with its own scheme use the store unchanged.

The envelope resolver decides from it
(`docs/components/magician/approval-envelopes.md`), in shadow — and the
**approval** path decides from it too, through `resolve_approval_waiver`, which
derives the class with the strict `consequence_class_for_approval_rule` rather
than the dispatch classifier.

Related built pieces: the envelope, its limits and consumption ledger
(`approval-envelopes.md`); shadow mode (`log_envelope_shadow` logs
`[ENVELOPE-SHADOW]` in the outward dispatch gate, observation only); restricted
outward actions (§4A, gate 1 of the outward gate, `restricted-actions.md`); the
owner surface `/api/magician/v2/approval-envelopes`
(`magician-api/src/approval_envelopes_api.rs`); and the approval path —
`run_standing_consent` builds the gate, `ApprovalGate::check_against` takes it,
and `resolve_approval_waiver` derives the class from the step's own capability
and action, so any act behind `requires_approval`, outward or not, is classified.

## What is still inert, and why

`run_standing_consent` returns `None` when `envelope_mode()` is `Off`, and `Off`
is the default and is installed once at config load. Unless configuration says
otherwise, the waiver path is never entered and no `[ENVELOPE-WAIVED]` line is
ever emitted; the classification runs at the outward dispatch gate (which is
unconditional) and in shadow logging (which is also mode-gated).

Two guarantees hold whatever the mode, and both are the reason the strict
classifier exists: it never answers `private_local`, and an unclassified act
falls closed to `commitment`, which no envelope of any kind may waive.

Nothing outside approval and outward dispatch consumes the class (no reporting
or aggregation).
