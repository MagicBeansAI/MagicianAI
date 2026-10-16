# One-time credential HITL responses

Built-in browser JIT prompts use the scoped
`POST /api/magician/v2/hitl/{id}/respond` route with `source: "user_request"`.
Secure input uses `{ "type": "password", "value": "..." }`; cancellation uses
`{ "type": "aborted" }`. The final choice is `allow_once` or `cancel`.

The pending request selects the private service path; the request body cannot
designate another prompt as secure. Secure material moves directly into the
service without the generic relay copy. The service checks scope, deadline,
receiver liveness and first-response ownership, then strips input before
durable history and event publication. A restored/orphan secure request is
cancelled and discards material. Public response bodies contain acceptance
status only.

See [the custody and browser contract](../magician/jit-browser-credentials.md)
for the supported boundary and rollout requirements.

## Spec-driven gate

The private path is selected by the pending request's **sensitivity spec**
(`UserRequest.sensitive`), not its type name; the two browser request types
(`secure_browser_input`, `secure_browser_confirm`) stay secure as a backstop
for entries restored from before the spec existed.
`sensitive_transport_for_pending` in `web_api.rs` decides the route:

- **Secure** — a `password`/`text` value goes into `UserResponse.input` for
  `respond_scoped` to move into custody. So does a `guidance` value: a masked
  answer through the advice box still answers this ask. A **form** ships every
  answered field as one JSON object, which the service splits: spec-flagged
  fields into one custody deposit each, the rest back into `input` as ordinary
  text. Nothing is rendered for relays (`input_text` is `None`).
- **Ordinary** — the value is rendered by `agentic_value_input_text`, which
  never renders a `Password` and, for forms, withholds every spec-flagged
  field plus any field whose id reads as a secret (`[sensitive; withheld]`).
  The clarification arm has no pending request and so only the wording
  heuristic, which can only withhold more.

Tests: `web_api::tests` (`p0_*` and the sensitivity-spec routing cases).

## Response shape

`UserResponse` carries `sensitive: [{ reference, kind, status, field? }]`
(value-free), and pending requests carry a `sensitive` sensitivity spec; see
the [HITL / Attention contract](../magician/hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1).

## One-time code answers

`UserInputType::Otp` is a first-class agentic ask. Its answer rides
`UserInputValue::Password` on the wire (`{ "type": "password", "value": "..." }`
against a pending `otp` ask), so `user_input_value_matches_type` accepts the
pair and the value takes the password's secure path: never rendered, moved
into custody or the vault as one-time material, exact string (a leading zero
is part of the code). The agentic resume route accepts `"input_type": "otp"`
in `requested_input_type` and refuses a mismatch against the pause's recorded
type. Every announcement of an `otp` or `password` ask carries the value-free
spec in `input_schema.sensitive`, which is what a client masks by.

## Critical-request delivery

Three route groups under `/api/magician/v2` back the delivery coordinator
(`docs/components/magician/critical-request-delivery.md`); handlers are in
`magician-api/src/hitl_delivery_api.rs`:

- `POST /hitl/deliveries/{delivery_id}/claim` and `POST …/report` — for the
  channel bots. Both require a runtime-minted `mag_bot_` bearer
  (`BearerKind::Bot { bot_name }`); a person's session or API token is refused
  (`bot_token_required`). A claim names its `channel_type`, which must equal
  the bot's name, and its `connection_generation`; the record must be `queued`
  in the bearer's scope. The grant returns the owner address and the
  value-free card. A report is accepted only from the claimant, and, when it
  names a `connection_generation`, only from the connection that claimed, so
  an orphaned process of the same bot cannot finish its replacement's send
  (`403 not_the_claimant`); a report naming none (an older SDK) is admitted.
  `status` is `provider_accepted` (+ `provider_message_id`),
  `confirmed_delivered` or `failed` (+ a bounded `reason`). An unknown id and
  another scope's id answer alike (`404 delivery_not_found`).
- `GET /hitl/deliveries?correlation_id=` — the owner's status: rows with
  masked addresses, latency percentiles, last claim per channel.
- `GET`/`PUT /settings/critical-delivery`, `POST
  /settings/critical-delivery/test` — the `hitl.critical_delivery` section
  and the owner's explicit test. `PUT` validates, writes durably, reloads the
  live config (`reload_applied` / `reload_error` in the envelope) and hands
  the coordinator the new policy.

## The ask an agentic answer names

An agentic `hitl.requested` carries `AgenticPauseState::hitl_correlation_id()`:
the pause's storage key plus the ask's own identity (`<key>~<12 hex>`), so a
step that asks twice (an OTP re-ask, a confirmation after a password)
publishes two requests the one-shot lifecycle journal accepts
(`docs/components/magician/hitl-attention.md`, "A correlation id is one ask").
The respond route and the legacy `agentic-resume` / `agentic-
continue` / `agentic-cancel` bodies accept that id verbatim as
`pause_state_id`: `split_ask_from_pause_selector` (`web_api.rs`) runs every
lookup on the key and, once the pause is taken, checks the ask against
`pause_state.ask_id` beside the plane lane's `expected_input_revision` check.
An answer to an earlier ask is refused with **409**
(`reason: superseded_ask`) and the pause is restored untouched; the canonical
`hitl.resolved` always carries the taken pause's own id. A bare key (an older
client, or a pause from before asks had identities) still selects whatever the
key holds now.
