# Outward assertions — what we told whom, and where

OPC phase 2. Plan: `docs/archive/plans/2026-08-07-opc-outward-assertions.md`.

Built: the schema and indexes with fail-closed preparation, the write point, and
the reverse lookup with correction obligations. Observed-channel writers exist;
remaining gaps are listed at the end.

## Why a new store

`EvidenceRecord` records evidence, `ClaimRecord` reads claims (ephemeral by
design), and Artifact V2 owns immutable output revisions. None records the
**disclosure**: that a claim was asserted, to these people, through this exact
artifact revision, on this channel, at this time. Without it a corrected figure
cannot find the emails, decks or drafts that stated it, and correction
propagation is impossible.

## Two records

Not every outward act makes a claim (*"Tuesday at 3 works"*, a calendar invite),
so act and claim are separate:

- **`OutwardActDisclosure`** — one per controlled outward act: immutable payload
  revision, resolved sender, audience, channel, consequence class, status.
- **`OutwardAssertionUse`** — zero or more: one claim, asserted by one act, to one
  audience, with its evidence.

## Statuses

```
prepared → dispatching → provider_accepted → delivered
                      ↘ dispatch_unknown            ↘ corrected | retracted
                      ↘ failed
```

A provider receipt proves acceptance, not delivery; delivery, bounce and
complaint update the record later. `dispatch_unknown` is a question held open:
a `prepared` record with no receipt proves neither send nor non-send, and a
stored idempotency key proves nothing about what the provider did.
`is_active_disclosure()` is **true** for it, because an act that may have reached
someone must still attract correction obligations.

## Ordering and fail-closed

```
1. prepare the record          (before anything leaves)
2. if preparation fails        -> the send FAILS CLOSED
3. perform the outward act     -> dispatching
4. provider receipt            -> provider_accepted
5. an idempotent retry RESUMES the same record
6. a stranded record is reconciled, or held at dispatch_unknown
```

`prepare()` returning `Err` means **do not send**: an act that succeeded while its
record failed is a disclosure nobody can find later.

## Invariants by path

Both uniqueness rules are path-derived, not bookkept (a path cannot drift from
itself):

- the act ref derives from `(scope, idempotency_key)`, so a retry resumes the
  same record even for a caller that never checks;
- the assertion-use id derives from `(act, claim, audience)`, so one record per
  triple cannot be violated by writing twice.

## Append-only

An act's file is its log: first line the disclosure, each later line a
transition, current state the fold. Corrections are successors, so history
survives and a retraction raises obligations rather than rewriting the past.

**Attempt identity.** A transition carries the dispatch attempt that drove it
(`effect_id`, see `docs/components/magician/effect-identity.md`) — on the
transition, not the act. The act is keyed by what it discloses (capability,
action, exact payload ref) so a replay resolves to the same record; an attempt id
in that key would make two attempts two disclosures. The store handle carries it
(`with_effect_id`) because `append_transition_with_binding` is the one place a
transition row is built; a handle is opened per dispatch and must not outlive it.

## Indexes

Written with the row, not derived by a later scan. Axes: `claim`, `evidence`,
`artifact`, `engagement`, `program`, `recipient`, plus `PROVIDER_MESSAGE_AXIS`.

- Work axes use `WorkContextKind`'s wire tokens, so a new kind of work gets an
  axis by existing. (`run_state::submit` files programme acts under `program`,
  not `engagement`.)
- Acts whose relationship is an account, panel or person are not reachable by a
  work axis — the record carries only `program_id` and `engagement_id`. Every act
  is filed under `artifact`, so `act_refs_under_axis(scope, ARTIFACT_AXIS)` minus
  the work axes counts the acts no work names.
- `act_refs_under_axis(scope, axis)` unions every index file under one axis. It
  **refuses** `recipient`, which mixes assertion-use ids with act refs. A missing
  axis directory reads as empty; other listing failures propagate.
- `index_entries(scope, axis, value)` dedupes as a **set** preserving first-seen
  order (index files interleave, so `Vec::dedup` is not enough).
- **The index is written before the row.** "Row exists" then implies "index
  exists"; the reverse order can lose the pointer permanently, because
  `record_assertion_use` early-returns on a visible row. A dangling entry from a
  failed row write is inert.

## Reverse lookup and correction obligations

- `disclosures_carrying_claim(scope, claim_ref)` — one row per `(act, audience)`
  with the act's **status** and the **exact payload revision** that carried it
  (what must be corrected is that revision, not the artifact now).
- `disclosures_resting_on_evidence(scope, evidence_ref)` — what we said on the
  strength of a source later found wrong.

`raise_correction_obligations(scope, claim_ref, correction_ref, now)` raises one
obligation per **active** affected disclosure:

- **It alters no prior record** — the obligation points at act and assertion
  use; neither learns about it (asserted by comparing records before and after).
- **Active only**: `prepared` never left, `failed` told nobody; `dispatch_unknown`
  does raise one.
- **Idempotent**: id derives from `(correction_ref, assertion_use_id)`.

`resolve_obligation` appends an outcome; **"nothing" is a legitimate
resolution** — the store finds what is affected and does not decide (plan §10).

## The write point

There is no Rust email adapter (`agentmail-send` is a skill), so the write point
is `execute_action_inner`, the single function every action passes through —
covering every outward capability at once. Order (plan §4):

1. classify the dispatch;
2. **write the disclosure** — `prepare_dispatch`, before anything leaves;
3. on failure the whole dispatch fails. **The send fails closed.**
4. under capture the record stays `prepared` (recorded, nothing left; not active,
   so owes no corrections);
5. a live send is marked `dispatching` then immediately `dispatch_unknown` —
   before the send, so a process that dies mid-act leaves "we do not know";
6. `agentic::outward_settle` binds the act to the provider message the result
   named, or appends why it could not. Only this moves an act past step 5.

### Binding the provider message

The binding is taken at `execute_action`, the one funnel every result passes
through (a settle wired into some of `execute_action_inner`'s many return routes
would miss the rest). `agents::outward_receipts` (`RECEIPT_SOURCES`) reads the id
from the tool's result using a table keyed by capability **and sending action
token** — a mail capability is also classified outward when it merely carries an
argv escape hatch, and a Gmail read's top-level `id` must not be bound as a send.

`record_provider_message` refuses:

- a blank id or one with a control character (U+001F is the index-key separator);
- an act still at `prepared` (a captured rehearsal must not acquire a real id);
- a terminal act (`delivered`, `failed`, `retracted`, `corrected`);
- a provider message already bound to a different act (one message is one act).

An identical binding resumes; a different id on a bound act is an error. The act
moves to `provider_accepted`. Nothing is written to the delivery ledger: an
`accepted` observation per send would empty `DeliveryLedger::unreconciled`, the
sweep that names sends no provider event came back about.

`act_for_provider_message(scope, provider, id)` is the reverse join a receipt
needs (a bounce carries an id and nothing else) before
`delivery::intake::ReceiptIntake::admit`. Unbound id → `None`; an id filed
against more than one act **refuses**. `record_provider_message` also uses it to
enforce one-message-one-act.

Two identifiers are in play: `delivery_receipts::SentMessageIndex` records the
RFC 5322 `Message-ID` / RFC 3461 envelope id a mail bounce quotes; a provider API
id may differ. Whether AgentMail's `message_id` equals the RFC 5322 header is not
established, so the settle records the API id and registers nothing with the mail
correlator.

### What a send returns, per channel

| Capability | Result shape | Id recoverable |
|---|---|---|
| `agentmail-send` | `{"message_id","thread_id"}` (`SendMessageResponse`) | **Yes**, `message_id` |
| `kapso-whatsapp-send` | `{"messaging_product","contacts":[…],"messages":[{"id","message_status"}]}` | **Yes**, `messages[0].id` (Meta `wamid`) |
| `gmail`, `presto-gmail` | Gmail API `Message` resource from `gws gmail +send` | **Yes**, `id` — inferred from the API contract, not confirmed live; shape-strict, so a different envelope leaves the act unknown |
| `imessage_send` | `{"status","to","service","stdout","stderr","exit_code"}` | **No** — AppleScript `send` returns no message identity |
| `whatsapp` `send` | `{"id","timestamp"}` (`wu messages send … --json`) | **Yes**, `id` (Baileys key, a different namespace from `wamid`) |
| `telegram` `send` | `{"ok","method","chat_id","message_id","result":{…}}` | **Yes**, `chat_id` + `message_id` pair (unique only within a chat) |
| `telegram-self` `send` | `{"channelId","messageId"}` (`tgcli send text … --json`) | **Yes**, pair, under its own provider name |
| `whatsapp` / `telegram` / `telegram-self` `run` | whatever the command prints | **Never** — free-text escape hatch; the token cannot say it was a send |
| `calendar`, `presto-calendar` | Calendar `Event` resource | **No** — not a per-recipient message id |
| `browser`, `agent-browser` | page snapshot | **No** |

### Chat channels: `send` vs `run`

`telegram`, `telegram-self` and `whatsapp` each declare an explicit `send` action
(in `SENDING_ACTIONS`) with a typed recipient (`jid`, `chat_id`, `to`), so a send
classifies and reaches the whole outward block, `resolve_effective_action` can
name who it reaches, and it returns a provider id. Each has a
`RESTRICTED_ACTIONS` entry whose `allowed_parameters` are exactly the skill's own
(`jid`/`text`/`reply_to`, `chat_id`/`text`, `to`/`message`/`reply_to`) with no
sender parameter (the transports fix the sender), and a `RECEIPT_SOURCES` entry
on `send`.

Classifying `run` is **rejected** (see `BROWSER_SUBMITTING_ACTIONS`): `run` also
reads, its free-text parameter can never be reduced to a bindable form, and
classifying it would refuse every read. Send-shaped `run` commands are instead
caught by the structural deny rule in `outward-actions.md` (classified, then
refused at §4A); `run` commands outside those heads stay invisible here. The
skill-side fix is for `run` to refuse sends itself, so the only sending door
names itself.

### The audience is resolved, not read

`intended_audience` comes from `execution::resolve_effective_action`, never the
payload's typed fields: a passthrough (e.g. `agentmail-send` `extra_args` with
extra `--to`) can widen recipients, so typed fields name a subset. See
`docs/components/magician/effective-action.md`. A dispatch that accepts an escape
hatch logs `[OUTWARD-UNBINDABLE]` and is recorded, not refused — failing closed
on unbindable acts is the envelope resolver's authority decision.

### Payload ref (a deliberate deviation)

Artifact V2's write path needs a `task_id` an outward send may not have, so
`store_payload` content-addresses the exact dispatched bytes as
`payload://blake3:<hash>`. Content-addressing is immutability, which is what the
plan needs. This deviates from plan §2's ownership table; when Artifact V2 gains
an outward-payload revision the ref migrates, with the stored bytes as input.

`consequence_class` comes from `agents::consequence_class_for`
(`docs/components/magician/consequence-classes.md`).

## Observed channels

A live meeting cannot be prepared. `record_observed_act` writes the record with
`observed: true`, and `prepare()` refuses an observed channel. The mark matters:
being transcribed afterwards is not having been cleared beforehand, and the
record must never read as pre-authorisation. With no prepare step, nothing at the
record layer can prevent the disclosure — which is why terms conversations stay
founder-attended.

## Observed statements

`observed_statements::record_observed_statement` is the observed-channel writer:

```
spoken → transcript captured → statement extracted → owner-confirmed where
needed → appended
```

It covers any channel where words leave before the runtime can record them
(room, phone call, later transcription). It uses only
`OutwardChannel::is_controlled`; a controlled channel is refused with a message
naming `prepare_dispatch`. The **consequence class is caller-supplied**
(`ObservedStatement::spoken` defaults it to bounded communication); understating
is the direction that matters.

- **Extraction is not assertion.** The **act** is always recorded; the **claim**
  only once owner-confirmed. An unconfirmed extraction returns as a
  `PendingClaim` — neither written nor dropped — so a correction never chases
  people over something nobody said.
- **One assertion per person, not per room**, so a retraction can raise one
  obligation per listener. An unconfirmed extraction and an empty audience both
  surface rather than silently write zero rows.
- Re-processing a transcript **resumes**: the act ref derives from the statement
  key.

## Ownership

The evidence subsystem owns this store. Presentation Maker, Demo Maker and
outward answering **consume; never own** — a second authoritative copy is how a
claim ends up verified in one place and stale in another.

## Write points and gaps

| Write point | State |
|---|---|
| Room visibility | Wired — the data-room grant path builds `GrantDisclosure` and calls `record_room_disclosures` before minting or rotating a link, and when a document is added to a live room (`magician-api/src/data_room_api.rs`). |
| Form submission | `work_modules_api` → `RunStateStore::submit` records the covering act, then seals the run. `record_submission` remains a primitive for callers holding an act ref; nothing records a Form-channel act on its own. |
| Transcript / commitment acceptance | Owner HTTP surface: `POST /api/magician/v2/transcripts/ingest` (`transcript_claims_api.rs`, `TranscriptIngestion`). Meeting sinks do not auto-ingest: which rooms are outward is a relationship judgement. |

Not built:

- **Provider event stream.** No shipped skill exposes a delivery event (the
  pinned AgentMail SDK declares only `message.received`). Receipts enter through
  `ReceiptIntake::admit` (which calls `DeliveryLedger::reconcile`) behind
  `POST /api/magician/v2/delivery/receipts` and `POST /delivery/receipts/mail`,
  from an operator or a parsed DSN.
- **Automatic reconciliation.** Every live send is written to
  `outward_gate::DispatchLog`; `delivery_hygiene::silence` reads
  `DispatchLog::unreconciled` on the delivery-hygiene tick and at
  `GET /api/magician/v2/delivery/unacknowledged`. With no automatic receipts,
  live sends stay `dispatch_unknown` until someone posts one; the counts report
  how many, on which rail, for how long. See `docs/components/magician/delivery.md`.
- **Envelope resolution.** Nothing here decides from the consequence class; the
  envelope resolver runs in **shadow** at the dispatch gate
  (`envelope_mode()` is `Off` unless configured;
  `docs/components/magician/approval-envelopes.md`).
