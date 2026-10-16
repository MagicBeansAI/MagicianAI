# Delivery

Module: `magician/src/magician_v2/delivery/`.
Named as a gate in `docs/archive/plans/2026-08-07-opc-readiness-review.md` (Workstream A).

**What the provider did with an act after we dispatched it.** Without
reconciliation every live send stays at `dispatch_unknown` and "sent" is a claim
nothing can check, so it is a precondition for turning capture mode off. The
intake is the door a receipt comes through. The AgentMail bot forwards signed
delivery, bounce and complaint events through it; deployments can also name a
bounce mailbox to pull from. It is generic across any channel with a provider:
email, WhatsApp, SMS, push.

## The order is total, and commutative on purpose

`DeliveryState` is ranked by severity (`Accepted` < soft `Bounced` < `Failed` <
`Delivered` < hard `Bounced` < `Complained`) and the fold takes the maximum. It
is therefore **commutative**: webhook arrival order cannot change the answer. A
pairwise last-wins fold would settle `delivered → complained` and
`complained → delivered` differently, a bug only redelivery shows. Every edge is
pinned by a literal matrix, not computed from the ranking, so the test cannot
agree with itself.

Two rankings are judgement calls:

- **`Delivered` outranks `Failed`.** The ledger answers "did our words reach
  them", and the fail-closed answer is to assume they did; otherwise a
  correction could skip somebody who read the message.
- **A complaint outranks a delivery.** Reporting a message proves it was
  received.

## Shape

`reconcile(act_ref, receipt)` is idempotent by provider message id: the same
receipt twice is one record. A different state under the same id is a
transition and obeys the order. A different identity under the same id is
refused whatever its state; the check keys on the provider message id alone,
because the derived receipt id includes the state.

`state_of(act_ref)` folds the log; unknown is `dispatch_unknown`, which is
**not** success. `unreconciled(older_than)` is the operator query that exposes
a silently broken provider integration. `suppression_signals(since)` exposes
hard bounces and complaints as plain data for `magician_v2::suppression`;
neither module imports the other.

## Callers

- `suppression_signals(since)` is swept into the suppression register by
  `magician_v2::delivery_hygiene`, which owns the `SuppressionCause ->
  SuppressionReason` mapping and builds the evidence ref.
- `unreconciled(older_than)` is read by `delivery_hygiene::silence`, from the
  worker's tick and from `GET /api/magician/v2/delivery/unacknowledged`.
- `reconcile` is reached only through `delivery::intake::ReceiptIntake::admit`,
  from `POST /api/magician/v2/delivery/receipts` (normalised JSON) and
  `POST /api/magician/v2/delivery/receipts/mail` (raw RFC 3464 mail).

## The intake: one door, any provider

Module: `magician/src/magician_v2/delivery/intake.rs`.

The AgentMail bot subscribes to `message.delivered`, `message.bounced` and
`message.complained` with incoming mail. It verifies Svix over the raw body
before decoding, then calls the normalized intake with the SDK's
workspace-bound bearer. It acknowledges the provider only after every recipient
is durably admitted; an unavailable backend or unmatched send returns 503 so the
provider retries. Unknown bounce classes are refused. Kapso does not yet forward
delivery statuses.

`ReceiptIntake::admit` validates, records the attempt and passes the receipt to
`reconcile` **untouched**, so every invariant above holds: terminal states never
resurrect, one provider message is one identity's story, an identical replay
resumes and a changed payload under one id is an error. It adds exactly two
things:

- **The act must have left.** `admit` gets this scope's dispatch log and refuses
  a receipt whose act is not on it; otherwise any string named as an act ref
  could record a complaint, which hygiene turns into a register entry only an
  owner act with evidence can lift. The list is *supplied, never discovered*, as
  `unreconciled` takes it, so a second rail stays reconcilable.
- **An attempt log.** The ledger records what a provider said, not who carried
  it. Attribution (actor, authentication class, `provider` vs `operator`) lives
  beside it under the same `delivery/` root and is written **before** the
  reconcile, so a refused attempt still leaves a row; that is what a review of
  "who tried to mark this address complained" needs. It holds no outcome; the
  ledger does.

`IntakeAttribution::authentication` is a plain string: a generic ledger that
imported the HTTP layer's identity types could not serve a rail that
authenticates differently.

## Which act a receipt is about

`ReceiptIntake::admit` takes the act ref from its caller. The HTTP door accepts
an optional `act_ref`; when absent, it resolves the provider message id through
this scope's outward bindings. An explicit act that contradicts a binding is
refused. Automatic correlation also needs the committed binding, a dispatched
act and a reported identity in that act's intended audience; an index pointer
published before its binding transition is inert. The join stays in the outward
store:

- **The send records its message.**
  `agentic::outward_settle::settle_outward_dispatch` runs at `execute_action`
  (which every action result passes), reads the provider id from the tool
  result and calls `evidence::OutwardAssertionStore::record_provider_message`.
- **The id resolves back.**
  `OutwardAssertionStore::act_for_provider_message(scope, provider, id)`, filed
  under `PROVIDER_MESSAGE_AXIS`. An unbound id answers `None`; an id filed
  against more than one act is **refused**, since a wrong pick would land a
  complaint on a disclosure that never sent it.

For a multi-recipient adapter, optional `sent_message_id` names the original
send and `provider_message_id` the stable per-recipient key, keeping one message
key per identity without an operator-supplied act ref.

Two identifiers are in play. `delivery_receipts::SentMessageStore` (default
`SentMessageIndex`) records the RFC 5322 `Message-ID` or RFC 3461 envelope id a
**mail bounce** quotes; the outward binding records the provider's **API** id
(`DeliveryReceipt::provider_message_id`). Whether they match is never guessed.
Adapters may return explicit `delivery_identifiers` metadata (`message_id`,
`envelope_id`), which settlement registers in the sent index with the original
dispatch instant and audience; replay changes neither. The AgentMail send
adapter reads the sent message's `Message-ID` header through the CLI and returns
it apart from the API id. Missing or inconsistent metadata keeps provider
acceptance and reports a correlation gap. Manual `POST /delivery/sent` remains.

The settle writes nothing to this ledger: a send response is not a receipt, and
an `Accepted` per send would empty `unreconciled`. The act moves to
`provider_accepted` on its own disclosure and stays in the silence watch.

A send with no recoverable id stays at `dispatch_unknown`, with the reason
appended (capability, where the id was sought). **Unreconcilable is a failure to
know, not a failure to send.** `imessage_send` returns no id (AppleScript's
`send` returns nothing), so iMessage cannot be reconciled.

`whatsapp`, `telegram` and `telegram-self` declare a `send` action beside their
free-text `run`. `send` classifies outward, types its recipient and returns the
provider id as JSON: `id` for `whatsapp`, and a pair for Telegram, which numbers
messages per chat (`chat_id + message_id` for `telegram`,
`channelId + messageId` for `telegram-self`). Each has a `RECEIPT_SOURCES`
entry; the Telegram channels use different provider names (`telegram-bot`,
`telegram-user`) because their chat ids are separate namespaces. A send typed
into `run` is not classified and never reaches the disclosure write (see
`outward-assertions.md`).

## Authentication, and what a provider-direct door would still need

The route is **owner-authenticated only**. Every `/api/magician/v2` route sits
behind `cloudflare_access::verify_access_middleware`, whose only
unauthenticated paths are the mobile and Android-apps enrollment exchanges
(`MOBILE_ENROLLMENT_EXCHANGE_PATH`, `ANDROID_APPS_ENROLLMENT_EXCHANGE_PATH`,
each gated by a single-use capability in its handler), and no `magician-api`
route verifies a provider signature over a request body. The AgentMail Node receiver
verifies Svix and forwards with its workspace-bound bearer; Kapso's
HMAC-verified receiver handles inbound messages, not receipts. Neither opens an
unauthenticated route in Magician.

An open receipt endpoint would be a **remote suppression primitive**
(`complained` silences a real recipient for good, `delivered` hides a real
failure), so the door requires the `VerifiedRequestIdentity` the middleware
attaches and takes scope from it, not from headers. Optional bounded
`raw_payload` keeps the exact event beside its attribution in the private
attempt log, readable through the authenticated receipt read.

Provider shape references: [AgentMail delivery events](https://www.agentmail.to/docs/events)
and [sent-message headers](https://www.agentmail.to/docs/api-reference/inboxes/messages/get).

A provider-direct webhook would need four things this deployment lacks: a
per-provider signing key, a path exemption in the access middleware, raw-body
capture before JSON parsing, and a replay window keyed on the provider's event
id.

## What the silence read reports

`delivery_hygiene::silence` is a scan plus a pure fold, so a rail whose acts are
not outward disclosures can supply its own attribution unchanged.

- **Counts, never rates.** `scanned` sits beside every count.
- **`any_dispatch_recorded: false` is not a clean bill of health.** A scope with
  no dispatches has `overdue` zero, like one where every send was confirmed. The
  worker reports it as `no_dispatches`, not `idle`.
- **Unattributable acts count under `unattributed`**, never inside a named rail,
  so a quiet rail cannot hide in another's number.
- **Acts dispatched ahead of the clock** are counted apart from acknowledged and
  unacknowledged; clock skew must not erase a send.
- **Nothing is written.** Every number derives from the clock at read time, so
  the sweep is idempotent.

## The pull: nobody has to be holding the receipt

The intake is a push surface; alone, an act reconciles only when somebody brings
a receipt. `delivery_hygiene::receipts` is the pull: once per hygiene tick per
scope it asks a source what it holds, walks each receipt through **the same
door**, and moves the outward disclosure off `dispatch_unknown` where receipts
prove it. Entry point:
`delivery_hygiene::worker::SuppressionSweepWorker::spawn_with_receipts`.

- **One port.** `ReceiptPuller` asks "what have you got that I have not
  recorded?". `delivery_hygiene` imports no rail; the DSN implementation lives in
  `delivery_receipts::pull`, so a second rail is a second implementor.
- **One door, one route into suppression.** The pass never calls
  `DeliveryLedger::reconcile` or touches the register. Every pulled receipt goes
  through `intake::ReceiptIntake::admit`, and hard bounces reach the register
  only through `sweep_delivery_into_suppression`.
- **It runs before the sweep, on the same tick.** A bounce pulled at 09:00
  suppresses at 09:00, not one interval later (`delivery_hygiene.tick_interval_secs`,
  default 900, floored at 60).
- **Nothing is dropped.** An uncorrelatable report, an unreadable one that
  claims to be a report, and one the door refuses are counted and **left with
  their source**. A settle happens only after the door returns, and only for a
  handle whose receipts were all admitted.
- **A disclosure moves only where receipts prove it.** The pass asks
  `DeliveryLedger::reach` over the act's recorded audience, not the strongest
  receipt: one bounce plus one delivery is `delivered`, because `failed` would
  erase an active disclosure and its correction obligations. One unheard-of
  recipient holds the record at `dispatch_unknown`.

### What `GET /delivery/watch` tells an operator

Three independent state fields, because collapsing them hides a broken
subsystem behind a green dashboard:

| field | question it answers |
| --- | --- |
| `state` | is the suppression register being written at all? |
| `silence_state`, `overdue` | how many dispatched acts are still unacknowledged past the grace window? |
| `receipts_state`, `acts_left_dispatch_unknown`, `acts_left_dispatch_unknown_total` | is reconciliation actually happening — on this tick, and ever? |

`receipts_state: "no_source"` means no source is attached and is deliberately
**not** `idle`: nothing pulls receipts, so every live send stays at
`dispatch_unknown`. `receipts_last_error` says so. `receipts_examined` sits
beside `receipts_offered`, and `receipts_uncorrelated` is its own count.

### Bounce mailbox (config + maildir)

`delivery_hygiene.bounce_mailbox` is `None` by default. `magician-bin` still
calls `spawn_with_receipts`, and the watch says `no_source` on every tick.

When an operator names a mailbox, `magician-bin` builds a `DsnPuller` over
`delivery_receipts::MaildirBounceMailbox`:

```yaml
delivery_hygiene:
  bounce_mailbox:
    provider: agentmail          # must match the send-index provider name
    maildir_path: /var/spool/magician/bounces
```

`provider` is stated, never derived: the send index is keyed on it, and a wrong
name refuses a correlatable bounce as `uncorrelated`. `maildir_path` is a
directory of `.eml` files, which ties the design to no provider API.

`MaildirBounceMailbox` implements `BounceMailbox`. Unread offers `.eml` files
only; a missing directory reads as empty, an unreadable one is an error (never
an empty list), and a pass caps at 1000 messages (oldest half plus newest half).
Settle renames to `<handle>.settled` and never deletes. Handles that could leave
the directory are refused. A provider-native reader would be a second
implementor of the same three methods.

`ReceiptPuller` lives in `delivery_hygiene`, not `delivery_receipts`: the ledger
must not know what a mailbox is, and the reader must not know what a
suppression is. An unreachable mailbox must never read as a clean run.

### Mail door: `POST /delivery/receipts/mail`

`magician-api/src/delivery_receipts_api.rs` also mounts on `/api/magician/v2`:

- `POST /delivery/sent`: register what left, under the identifier a bounce will
  quote back;
- `POST /delivery/receipts/mail`: hand in one raw message.

The mail route uses the same `VerifiedRequestIdentity` as `/delivery/receipts`.
The body is parsed as RFC 3464 and each correlated receipt goes through
`ReceiptIntake::admit`. Mail that is not a delivery report answers `200` with
`recognised: false`; a report that cannot be read answers **422**; a store
fault is **503**. `provider` is a parameter and `raw_mail` is bytes.

Direct tracked AgentMail replies carry their provider ID through the
attempt-bound Envoy acknowledgment. The Delivery dispatch log is written before
dispatch, and acceptance registers the verified sent header when available. A
failed header read keeps acceptance without licensing another send. Older
acknowledgments stay readable with their correlation gap visible.

Calendar adapters return a provider operation ID and an immutable
`resource_effect` on the same outward act; create, update and delete are
separate receipts even for one event. Conditional changes use the provider ETag
to refuse intervening edits ([Google's conditional-write contract](https://developers.google.com/calendar/api/guides/version-resources));
credentials stay in the selected gws profile and are exported or refreshed only
in memory for that request.

AgentMail bot replies always pin the explicit target and clear inherited CC/BCC,
system and owner replies included. Unknown SDK message variants are never sent
as fallback text. Calendar holds admit confirmed create or update effects that
match the accepted slot; release effects must postdate the current hold.

Reply bindings keep an originating task reference and brief digest. Matched Work
inherits that approved brief and goal, plus the completed task reference when
available; a changed or missing source brief needs review. The brief stays in
the task store, not the negotiation book.

When an AgentMail webhook lacks status subscriptions, the adapter updates its
event list and keeps other subscribed types; creating with a reused client ID is
not enough. See the [provider update contract](https://www.agentmail.to/docs/api-reference/webhooks/update).

A reply stays tied to its recorded offer after the original Work is cancelled or
suspended, but that does not authorize a new run. Stopped, archived, uncertain,
user-waiting and unknown source states need owner review; a completed
authorized source can still get follow-through.
