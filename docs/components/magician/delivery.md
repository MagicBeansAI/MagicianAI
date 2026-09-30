# Delivery

Module: `magician/src/magician_v2/delivery/`.
Named as a gate in `docs/archive/plans/2026-08-07-opc-readiness-review.md` (Workstream A).

**What the provider did with an act after we dispatched it.** Without
reconciliation every live send stays at `dispatch_unknown` and "sent" is a claim
nothing can check, so it is a precondition for turning capture mode off. The
intake is the door a receipt comes through; no wired provider pushes one
unprompted, so a deployment that wants automatic reconciliation names a bounce
mailbox for the pull.

Generic across any channel with a provider: email, WhatsApp, SMS, push.

## The order is total, and commutative on purpose

`DeliveryState` is ranked by severity — `Accepted` < soft `Bounced` < `Failed` <
`Delivered` < hard `Bounced` < `Complained` — and the fold takes the maximum.
That makes it **commutative**, so webhook arrival order cannot decide the answer.
An explicit pairwise transition with a last-wins fold would settle
`delivered → complained` and `complained → delivered` differently, a bug that
reproduces only under redelivery. All edges are pinned by a literal matrix
written out rather than computed from the ranking, so the test cannot pass by
agreeing with itself.

Two rankings are judgement rather than mechanics:

- **`Delivered` outranks `Failed`.** The question this ledger answers is "did our
  words reach them", and the fail-closed answer is to assume they did — refusing
  a delivery after a transport failure would let a correction skip somebody who
  read the message.
- **A complaint outranks a delivery.** Somebody who reports a message received it
  first, so a complaint proves receipt as well as recording harm.

## Shape

`reconcile(act_ref, receipt)` is idempotent by provider message id: the same
receipt twice is one record. A *different* state under the same id is a
transition, not a duplicate, and obeys the order rather than overwriting. A
different *identity* under the same id is neither: one provider message is one
identity's story, and the second is refused whatever state it claims — the check
is keyed on the provider message id alone, because the derived receipt id folds
the state in and would only fire while the state held still.

`state_of(act_ref)` folds the log; unknown is `dispatch_unknown`, which is
explicitly **not** success. `unreconciled(older_than)` is the operator query that
makes a silently broken provider integration visible.

`suppression_signals(since)` exposes hard bounces and complaints as plain data
for `magician_v2::suppression` to ingest — neither module imports the other.

## Callers

- `suppression_signals(since)` is swept into the suppression register by
  `magician_v2::delivery_hygiene`, which owns the `SuppressionCause ->
  SuppressionReason` mapping and builds the evidence ref — keeping "neither
  module imports the other" intact.
- `unreconciled(older_than)` is read by `delivery_hygiene::silence`, from the
  worker's tick and from `GET /api/magician/v2/delivery/unacknowledged`.
- `reconcile` is reached only through `delivery::intake::ReceiptIntake::admit`,
  from `POST /api/magician/v2/delivery/receipts` (normalised JSON) and
  `POST /api/magician/v2/delivery/receipts/mail` (raw RFC 3464 mail).

## The intake: one door, any provider

Module: `magician/src/magician_v2/delivery/intake.rs`.

No wired provider emits delivery events (the AgentMail bot ignores everything but
`message.received`; Kapso exposes no bounce/complaint/status surface). The intake
is deliberately **not** provider-specific: it is the normalised door any
provider, adapter or operator pushes a receipt through.

`ReceiptIntake::admit` validates, records the attempt, and hands the receipt to
`reconcile` **untouched**. Every invariant above is unchanged: terminal states
never resurrect, one provider message is one identity's story, an identical
replay resumes and a changed payload under one id is an error, the severity
order decides.

Two things it adds, and only two:

- **The act must have left.** `admit` is handed this scope's dispatch log and
  refuses a receipt whose act is not on it. Without that check, one call naming
  an arbitrary string as an act ref could record a complaint — which the hygiene
  sweep turns into a register entry only an explicit owner act with evidence can
  lift. The candidate list is *supplied, never discovered*, exactly as
  `unreconciled` takes it, so a second rail stays reconcilable.
- **An attempt log.** The ledger records what a provider said and has no field
  for who carried it, and should not grow one. The attribution — actor,
  authentication class, and `provider` vs `operator` — lives beside it under the
  same `delivery/` root. It is written **before** the reconcile, so a refused
  attempt still leaves a row: the attempt somebody made and the store rejected is
  exactly what a review of *"who tried to mark this address complained"* is made
  of. It holds no disposition; the outcome is the ledger's, in one place.

`IntakeAttribution::authentication` is a plain string. This module does not
import the HTTP boundary's identity types, because a generic ledger that
imported an authentication enum could never serve a rail that authenticated
differently.

## Which act a receipt is about

`ReceiptIntake::admit` takes the act ref from its caller
(`RecordDeliveryReceiptBody.act_ref`, required). A real provider event carries
only the provider's **message id**, so the door is reachable by an operator who
knows the act, not by a webhook. The join lives in the outward store — a generic
ledger must not learn what an outward act is:

- **The send records which message it became.**
  `agentic::outward_settle::settle_outward_dispatch` runs at `execute_action`
  (the funnel every action result passes through), reads the provider's id out
  of the sending tool's result, and calls
  `evidence::OutwardAssertionStore::record_provider_message`.
- **The id resolves back to the act.**
  `OutwardAssertionStore::act_for_provider_message(scope, provider, id)`, filed
  under `PROVIDER_MESSAGE_AXIS`. An unbound id answers `None`; an id filed
  against more than one act **refuses** rather than picking (a wrong pick lands a
  complaint on a disclosure that never sent it).

`POST /delivery/receipts` does not yet resolve an absent `act_ref` through that
join.

Two identifiers are in play. `delivery_receipts::SentMessageStore` (default
adapter `SentMessageIndex`) records the RFC 5322 `Message-ID` or RFC 3461
envelope id a **mail bounce** quotes back; the outward binding records the
provider's **API** id (`DeliveryReceipt::provider_message_id`). Whether these
coincide for AgentMail is not established, so the settle registers nothing with
the mail correlator rather than guess. The runtime does not call
`POST /delivery/sent`.

The settle writes nothing to this ledger: a send response is not receipt, and an
`Accepted` per send would empty `unreconciled`. The act moves to
`provider_accepted` on its own disclosure and stays in the silence watch.

A send whose result carries no recoverable id stays at `dispatch_unknown`, with
the reason appended (capability, where the id was looked for).
**Unreconcilable is a failure to KNOW, not a failure to send.**
`imessage_send` returns no message id (AppleScript's `send` returns nothing), so
that channel cannot be reconciled.

`whatsapp`, `telegram` and `telegram-self` declare an explicit `send` action
beside their free-text `run` escape hatch. `send` classifies outward, names its
recipient in a typed parameter and returns the provider's id as JSON — `id` for
`whatsapp`; for Telegram a PAIR, because Telegram numbers messages per chat
(`chat_id + message_id` for `telegram`, `channelId + messageId` for
`telegram-self`). Each has a `RECEIPT_SOURCES` entry; the Telegram channels file
under different provider names (`telegram-bot`, `telegram-user`) because their
chat ids are not one namespace. A send typed into `run` is not classified and
never reaches the disclosure write (see `outward-assertions.md`).

## Authentication, and what a provider-direct door would still need

The route is **owner-authenticated only**: the Magician API has no
provider-signature middleware for an unattended inbound POST. Every `/api/magician/v2` route sits behind
`cloudflare_access::verify_access_middleware`, whose only bypass is the
device-enrollment exchange (still gated by a single-use capability in its
handler), and nothing in `magician-api` verifies a signature over a request
body. The separate Node webhook receivers do not expose delivery events:
AgentMail ignores them, while Kapso's fail-closed HMAC-verified receiver handles
inbound messages rather than delivery receipts.

A receipt endpoint anyone could POST to is a **remote suppression primitive** —
`complained` silences a real recipient permanently, `delivered` silences a real
failure — so the door requires the `VerifiedRequestIdentity` the middleware
attaches, and takes the scope from that identity rather than from headers.

Turning it into a provider-direct webhook needs four things this deployment does
not have: a per-provider signing key, a path exemption in the access middleware,
raw-body capture before JSON parsing (a signature covers the exact bytes), and a
replay window keyed on the provider's own event id.

## What the silence read reports

`delivery_hygiene::silence` splits into a scan and a pure fold, so a rail whose
acts are not outward disclosures supplies its own attribution and nothing in the
module changes.

- **Counts, never rates.** `scanned` is carried beside every count: six silences
  over eight acts and six over eight thousand are different facts.
- **`any_dispatch_recorded: false` is not a clean bill of health.** A scope with
  no dispatches has an `overdue` of zero, exactly like a scope where every send
  was confirmed, and only one of them is evidence that sending works. The flag is
  what tells them apart, and the worker reports that case as `no_dispatches`
  rather than `idle`.
- **An act nothing can attribute is counted under `unattributed`**, never folded
  into a named rail — a rail that has gone quiet must not be able to hide inside
  another rail's number.
- **An act dispatched ahead of the clock** is counted apart from both
  acknowledged and unacknowledged. Clock skew must not erase a send.
- **Nothing is written.** Every number is derived from the clock on the read, so
  the sweep is idempotent and no decision can move because a window was widened.

## The pull: nobody has to be holding the receipt

The intake is a **push** surface; alone, an act is reconciled only when
somebody walks a receipt through.

`delivery_hygiene::receipts` is the pull. Once per hygiene tick, per scope, it
asks a source what it is holding, walks each receipt through **the same door**,
and moves the outward disclosure off `dispatch_unknown` where the receipts prove
it. Named entry point:
`delivery_hygiene::worker::SuppressionSweepWorker::spawn_with_receipts`.

- **One port.** `ReceiptPuller` is *"what have you got that I have not
  recorded?"*. Nothing in `delivery_hygiene` imports a rail; the DSN
  implementation lives in `delivery_receipts::pull`, so the dependency runs
  specific → generic and a second rail is a second implementor rather than an
  edit to the sweep.
- **One door, one route into suppression.** The pass never calls
  `DeliveryLedger::reconcile` and never touches the suppression register. Every
  pulled receipt goes through `intake::ReceiptIntake::admit`, so the
  act-must-have-left check, the attempt log and the severity order apply exactly
  as they do to a posted one; the hard bounces it records reach the register
  through `sweep_delivery_into_suppression` and nowhere else.
- **It runs before the sweep, on the same tick.** A bounce pulled in at 09:00
  suppresses at 09:00. Pulling after the sweep would leave the address sendable
  for a whole interval — fifteen minutes, by default.
- **Nothing is dropped.** A report nobody can correlate to a send, one that
  announces itself and cannot be read, and one the door refuses are all counted
  and **left with their source**, never settled and never guessed at. A settle
  happens only after the door returns, and only for a handle every one of whose
  receipts was admitted.
- **A disclosure moves only where the receipts prove it.** The pass asks
  `DeliveryLedger::reach` over the act's own recorded audience, not the strongest
  receipt: an act where one recipient bounced and another was delivered is
  `delivered`, because marking it `failed` would erase an active disclosure and
  every correction obligation resting on it. One unheard-of recipient holds the
  whole record at `dispatch_unknown`.

### What `GET /delivery/watch` tells an operator

Three independent state fields, because collapsing them is how a subsystem stays
broken behind a green dashboard:

| field | question it answers |
| --- | --- |
| `state` | is the suppression register being written at all? |
| `silence_state`, `overdue` | how many dispatched acts are still unacknowledged past the grace window? |
| `receipts_state`, `acts_left_dispatch_unknown`, `acts_left_dispatch_unknown_total` | is reconciliation actually happening — on this tick, and ever? |

`receipts_state: "no_source"` is what a process with no attached source reports,
and it is deliberately **not** `idle`: it means nothing pulls a receipt back, so
every live send stays at `dispatch_unknown` and the register is only as full as
whatever somebody posted by hand. `receipts_last_error` carries the sentence
that says so. Counts, never rates: `receipts_examined` sits beside
`receipts_offered`, and `receipts_uncorrelated` is its own number rather than a
shortfall somebody has to compute.

### Bounce mailbox (config + maildir)

`delivery_hygiene.bounce_mailbox` is `None` by default. With no mailbox,
`magician-bin` still calls `spawn_with_receipts` and the watch says
`no_source` on every tick — a sweep with no source asked nobody, and that
must never render as idle.

When an operator names a mailbox, `magician-bin` builds a `DsnPuller` over
`delivery_receipts::MaildirBounceMailbox` and passes it to
`spawn_with_receipts`:

```yaml
delivery_hygiene:
  bounce_mailbox:
    provider: agentmail          # must match the send-index provider name
    maildir_path: /var/spool/magician/bounces
```

`provider` is stated, never derived: the send index is keyed on it, and a
lookup under the wrong name refuses a correlatable bounce as `uncorrelated`.
`maildir_path` is a directory of `.eml` files. Every way a bounce reaches a
host ends in something that can write a file, so this commits to no
provider API.

`MaildirBounceMailbox` implements `BounceMailbox`. Unread offers `.eml`
files only; an absent directory reads as empty, an unreadable directory is
an error (never an empty list), and a pass caps at 1000 messages (oldest
half plus newest half). Settle renames to `<handle>.settled` beside itself
and never deletes. Handles that could leave the directory are refused. A
provider-native reader is a second implementor of the same three methods
when a deployment cannot reflect its mailbox onto a filesystem.

The `ReceiptPuller` trait lives in `delivery_hygiene` rather than in
`delivery_receipts`: the ledger must not learn what a mailbox is, and the
mailbox reader must not learn what a suppression is. A puller that finds
nothing and a puller that cannot reach the mailbox are different answers;
an unreachable mailbox must never read as a clean run.

### Mail door: `POST /delivery/receipts/mail`

`magician-api/src/delivery_receipts_api.rs` also mounts, on the same
`/api/magician/v2` scope:

- `POST /delivery/sent` — register what left, under the identifier a bounce
  will quote back
- `POST /delivery/receipts/mail` — hand in one raw message

The mail route is owner-authenticated (same `VerifiedRequestIdentity` as
`/delivery/receipts`). The body is parsed as RFC 3464; each correlated
receipt goes through `ReceiptIntake::admit`. Mail that is not a delivery
report answers `200` with `recognised: false`. Mail that announces itself as
a delivery report and cannot be read answers **422**. A store fault is
**503**. Nothing here is AgentMail-shaped: `provider` is a parameter and
`raw_mail` is bytes.
