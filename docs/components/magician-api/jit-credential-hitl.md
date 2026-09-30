# One-time credential HITL responses

Built-in browser JIT prompts use the existing scoped
`POST /api/magician/v2/hitl/{id}/respond` route with `source: "user_request"`.
Secure input uses `{ "type": "password", "value": "..." }`; cancellation uses
`{ "type": "aborted" }`. The final choice is `allow_once` or `cancel`.

The authoritative pending request's type (`secure_browser_input` or
`secure_browser_confirm`) selects the private service path. The request body
cannot designate another prompt as secure. Secure material is moved directly
into the service without the generic relay copy. The service checks scope,
deadline, receiver liveness, and first-response ownership, then strips input
before durable history and event publication. A restored/orphan secure request
is cancelled and discards material. Public response bodies contain acceptance
status only.

See [the custody and browser contract](../magician/jit-browser-credentials.md)
for the precise supported boundary and rollout requirements.

## Spec-driven gate (P1)

The private path is selected by the pending request's **sensitivity spec**
(`UserRequest.sensitive`), not by its type name; the two browser request types
stay secure as a belt-and-braces for entries restored from before the spec
existed. `sensitive_transport_for_pending` in `web_api.rs` decides the route:

- **Secure** — a `password`/`text` value goes into `UserResponse.input` for
  `respond_scoped` to move into custody. So does a `guidance` value: a masked
  answer submitted through the advice box still answers THIS ask, and dropping
  it would resolve the ask with no answer. A **form** ships every answered field
  as one JSON object, which the service splits — spec-flagged fields into one
  custody deposit each, the rest back into `input` as ordinary text. Nothing
  is rendered for relays (`input_text` is `None`).
- **Ordinary** — the value is rendered by `agentic_value_input_text`, which
  never renders a `Password` and, for forms, withholds every spec-flagged
  field plus — with or without a spec — any field whose id reads as a secret
  (`[sensitive; withheld]`). The clarification arm has no pending request and
  therefore only the wording heuristic; that is compatibility detection and
  can only withhold more.

Tests: `web_api::tests` (`p0_*` and the sensitivity-spec routing cases).

## Response shape (P1)

`UserResponse` carries `sensitive: [{ reference, kind, status, field? }]`
(value-free) and pending requests carry a `sensitive` sensitivity spec; see
the [HITL / Attention contract](../magician/hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1).
The responder gate that consumes it is described above.

## One-time code answers (P3)

`UserInputType::Otp` is a first-class agentic ask. Its answer rides
`UserInputValue::Password` on the wire (`{ "type": "password", "value": "..." }`
against a pending `otp` ask), so `user_input_value_matches_type` accepts the
pair and the value takes the same secure path a password does — never
rendered, moved into custody or the vault as one-time material, exact string
(a leading zero is part of the code). The agentic resume route accepts
`"input_type": "otp"` in `requested_input_type` and refuses a mismatch against
the pause's recorded type as it does for every other kind. Every announcement
of an `otp` or `password` ask carries the value-free spec in
`input_schema.sensitive` (see
[HITL / Attention](../magician/hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1)),
which is what a client masks by.

## Critical-request delivery (P5)

Three route groups under `/api/magician/v2` back the delivery coordinator
(`docs/components/magician/critical-request-delivery.md`):

- `POST /hitl/deliveries/{delivery_id}/claim` and `POST …/report` — for the
  channel bots. Both require a runtime-minted `mag_bot_` bearer
  (`BearerKind::Bot { bot_name }`); a person's session or API token is
  refused (`bot_token_required`). A claim names its `channel_type`, which
  must equal the bot's name, and its `connection_generation`; the record must
  be `queued` in the bearer's scope. The grant returns the owner address and
  the value-free card. A report is accepted only from the claimant — and only
  from the CONNECTION that claimed, when it names one: the optional
  `connection_generation` on the report is compared with the claim's, so an
  orphaned process of the same bot name cannot take a delivery terminal for
  its replacement's send (`403 not_the_claimant`); a report that names none
  (an older SDK) is admitted, since a field the bot does not send cannot be
  checked. `status` is `provider_accepted` (+ `provider_message_id`),
  `confirmed_delivered` or `failed` (+ a bounded `reason`). An unknown id
  and another scope's id answer alike (`404 delivery_not_found`).
- `GET /hitl/deliveries?correlation_id=` — the owner's status: rows with
  masked addresses, latency percentiles, last claim per channel.
- `GET`/`PUT /settings/critical-delivery`, `POST
  /settings/critical-delivery/test` — the `hitl.critical_delivery` section
  and the owner's explicit test. `PUT` validates, writes durably, reloads the
  live config (`reload_applied` / `reload_error` in the envelope) and hands
  the coordinator the new policy. Handlers: `magician-api/src/hitl_delivery_api.rs`.

## The ask an agentic answer names (P7)

An agentic `hitl.requested` carries `AgenticPauseState::hitl_correlation_id()`
— the pause's storage key plus the ask's own identity (`<key>~<12 hex>`), so
a step that asks twice (an OTP re-ask after a rejected code, a confirmation
after a password) publishes two requests the one-shot lifecycle journal
accepts (`docs/components/magician/hitl-attention.md`, "A correlation id is
one ask"). The respond route and the legacy `agentic-resume` / `agentic-
continue` / `agentic-cancel` bodies accept that id verbatim as
`pause_state_id`: `split_ask_from_pause_selector` (`web_api.rs`) runs every
lookup on the key and, once the pause is taken, checks the ask against
`pause_state.ask_id` beside the plane lane's `expected_input_revision`
check. An answer to an earlier ask of the step is refused with **409**
(`reason: superseded_ask`) and the pause is restored untouched; the canonical
`hitl.resolved` always carries the taken pause's own id, whichever selector
found it. A bare key — an older client, or a pause written before asks had
identities — still selects whatever the key holds now.
