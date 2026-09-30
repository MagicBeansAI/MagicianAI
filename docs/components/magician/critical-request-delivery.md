# Critical-request delivery

How a critical human-in-the-loop request — a credential or verification-code
ask, or a decision with a deadline — reaches the owner's verified private
destinations: registered mobile push, and the channel bots the owner enabled
(Kapso WhatsApp, the Telegram bot). Secure HITL plan §6.1, phase P5
(`docs/plans/2026-09-15-secure-hitl-credentials-and-otp.md`;
implementation `docs/plans/2026-09-19-secure-hitl-otp-implementation.md`).

Attention remains the authoritative pending request whatever the providers
do: a delivery is a projection of `hitl.requested`, never a second request,
and the owner answers only in the authenticated UI.

## The coordinator

`magician/src/magician_v2/hitl_delivery/` (`DeliveryCoordinator`) owns one
subscription to the canonical `HitlRequested` / `HitlResolved` lifecycle and
is started by the composition root for every runtime (bots may be enabled
without push). It is published process-wide (`hitl_delivery::global()`) for
the API.

**Criticality is structural** (`alert::Criticality::of`): a request whose
`input_schema` carries the value-free sensitivity spec (P3, `sensitive`), or
that names a `collection_deadline_ms`, is critical. An urgency word in a
prompt is not. Every other request keeps today's behaviour — the attention
push to registered devices — and gets no channel fan-out and no record.

**Destinations** (`policy::DeliveryPolicy`): the channel types in
`hitl.critical_delivery.enabled_channels`, in preference order, resolved to
owner addresses through `EnvoyConfig::owner_identities_for` — the same
authority `chat::envoy::is_owner` uses. Registration is eligibility, not
proof of the recipient: the last inbound sender, an envoy contact, a display
name, group membership or a model-provided id is never a destination. A
channel without an owner identity is reported on the settings surface and
recorded `unavailable`. Group channels are outside this contract.

**One deduplication decision.** When the request belongs to a chat session
whose origin channel equals a destination (same channel type, same
address), that destination is recorded `relayed_by_origin` and not sent:
the origin relay already turned the ask into a notice (P3,
`sensitiveAskNotice`). Push is never deduplicated against a channel — a
different device and surface — and is deduplicated per registration by the
push dispatcher's own watermark.

**Policy** `simultaneous` offers every destination at once; `staged` offers
the first and moves to the next only when the provider has not accepted
within `staged_fallback_secs` (a preferred destination that accepts first
marks the rest `skipped`).

**Retrieval grace** (P6, §6.2): a critical `otp` ask in a scope where
automatic verification-code retrieval is expected (`RetrievalOracle`) waits
`hitl.verification_codes.retrieval_grace_secs` (20 s) before its *channel*
alerts are offered — push is never delayed, a deadline under 90 s skips the
grace, a code that arrives inside it resolves the ask with no alert sent,
and a `VerificationRetrievalStatus` of `ambiguous` or `unavailable` ends the
grace at once so the alert goes out the moment retrieval says it will not
deliver.

**Quiet hours** hold an alert (`held`) until the window ends, unless the
request is time-bound and `interrupt_for_time_bound` is on; the owner's own
test is never held. An unparseable window holds nothing — misconfiguration
must never silence alerts. When the window ends the request is rechecked
before anything is sent.

## The alert

`alert::AlertCard` — value-free by construction:

| Field | Source |
| --- | --- |
| `service_alias` | the host of the destination a challenge bound the request to (P4 `expected_destination`), else "a service"; never a producer- or model-authored label |
| `reason` | the kind: "a verification code", "your password", "your sign-in details", "a private value", "a decision" |
| `deadline_ms` | the spec's collection window when it has one |
| `open_url` | `{public_origin}/attention?attention=1&attention_item={id}` when an owner-link origin is configured — `hitl.critical_delivery.owner_ui_origin` first, `mobile_access.public_origin` (or its env fallback) as the single-origin default — else absent and the text says to open Magician → Attention. `attention_item` is what the web UI's Attention center opens as an exact item (a correlation id is one of a row's aliases) and `attention=1` keeps the list behind it, so a request that already resolved shows "Item no longer available … may have been resolved" rather than a form or an empty page |
| `text` | the one sentence every channel sends, with a relative expiry and "Don't reply with the value here." |

Never the prompt, the hint, an option label, a mailbox preview, a token or an
answer. The link carries no authority: the page is behind login and scope,
a GET is read-only, and possession of the id claims nothing.

## Bots claim, never receive addresses

`RuntimeTransportEvent::CriticalRequestAlert { delivery_id, correlation_id,
channel_type, kind, alert, revision, deadline_ms, … }` is scoped
(`event_visible_to_scope`) and carries the card and **no address**. The
channel's bot (bot SDK `ChannelRuntime`, which opens the realtime feed at
start and reconnects it with backoff) claims it:

- `POST /api/magician/v2/hitl/deliveries/{id}/claim` `{channel_type,
  connection_generation}` — a `mag_bot_` bearer only; the bot's name must
  equal the channel type; the record must be `queued` in the bearer's scope.
  The claim returns the owner address and the card, binds the record to
  `bot:<name>` and the connection generation, and moves it to `claimed`. The
  winner is whoever performed that transition, never whoever matches the
  binding afterwards; the SDK's generation carries a per-process token, so two
  processes of one channel cannot collide on a shared counter.
- `POST …/{id}/report` `{status: provider_accepted | confirmed_delivered |
  failed, provider_message_id?, reason?, connection_generation?}` — only the
  claimant, and only the CONNECTION that claimed: one bot name can be two
  processes (an orphan beside its replacement), so a report naming a different
  generation than the claim is refused `not_the_claimant` rather than taking
  the delivery terminal for a send the replacement is still making. The SDK
  echoes the generation it claimed with; a report that names none (an older
  SDK) is admitted, since a field the bot does not send cannot be checked.
- `CriticalRequestRetired { delivery_id, outcome }` tells the bot that
  claimed to edit (Telegram) or annotate (Kapso) what it sent — a card sent
  within the last 24 hours; an older one is left alone, its link resolves
  to the completed state. A bot whose `provider_accepted` report is refused
  because the request closed between its claim and its send retires the
  card itself (`superseded`) — the resolution can race ahead of the send.

A push wave whose every registration names a platform with no provider
configured on this runtime is `unavailable` once ("no push provider is
configured on this runtime") — retrying cannot change it, and three attempts of
backoff would end `failed` on the owner's status for every critical ask on such
a host. An alert nobody claims within `CLAIM_WINDOW` (10 s) is `unavailable` — no
bot is connected — and the policy's fallback runs. A claimed delivery with no
report within `REPORT_WINDOW` (30 s) is `ambiguous`: the send may have
happened, Telegram and Kapso offer no idempotency key for a plain send, so
it is **not retried** on that destination — the documented duplicate-alert
boundary.

Provider behaviour: Telegram sends the card with an
inline **Open secure request** URL button and edits it on retirement;
WhatsApp through Kapso sends an interactive CTA-URL message inside the
24-hour customer-service window and, outside it, only the configured
fallback template (`KAPSO_FALLBACK_TEMPLATE_NAME`, the text as its body
variable) — without a template the send fails honestly instead of being
dropped, and the fallback destination runs.

## Records

`records::DeliveryStore` — `system/hitl-deliveries.json` under the data
root, private (`0600`), durable, bounded (`MAX_RECORDS` 2048, oldest
terminal rows evicted first). Routing and status metadata only: scope,
correlation id, revision, destination (channel type + address), state,
attempts, `requested_at_ms`, `enqueued_at_ms`, `claimed_at_ms`,
`accepted_at_ms`, provider message id, a bounded value-free reason, the
claim binding, and the ask's own lane (`source`, `execution_id`) and
`deadline_ms`. Never the card, never a prompt, never an answer. States:
`queued`, `held`, `claimed`, `provider_accepted`, `confirmed_delivered`,
`failed`, `unavailable`, `ambiguous`, `expired`, `resolved`,
`relayed_by_origin`, `skipped`. Provider acceptance is not proof the owner
saw the alert.

**A restart re-drives, it does not retire.** No task survives the process, but
the ask may still be pending, and nothing else re-announces it (the realtime
feed replays nothing; a republished `hitl.requested` for a key already pending
is not re-broadcast). `DeliveryCoordinator::recover_after_restart` reads the
coordinator's own rows at boot — awaited, BEFORE the coordinator subscribes, so
fresh rows from a mid-recovery announcement cannot be swept into a recovery
group (which would also overwrite the live delivery task's cancellation token).
Per correlation still live it asks the same `RequestOracle` every send asks,
then either retires the stale rows and offers a FRESH set (with the generic
card — the prompt is never persisted) or retires them with the reason (`the
request closed while the runtime was down`). A row that names no `source` can
only be retired.

**One writer.** The store serialises its whole row set on every commit under an
in-process lock, so it assumes ONE runtime per data root — the posture the rest
of the runtime already assumes (the orphan-holds-the-lease hazard). Two
processes on one root would last-writer-wins over each other's rows; the
durable user-request store takes a cross-process writer lease for exactly this
reason and the delivery log does not. Accepted limit: the log is routing and
status metadata, never the credential path.

Every send is preceded by a recheck (`RequestOracle`): the request is still
pending in its scope (`UserRequestService::pending_request_snapshot`, or the
agentic pause store for `source: agentic`), its deadline has not passed, and
it has not resolved meanwhile. Retries: at most 3 attempts per destination
with 5 s and 30 s backoff, each rechecked — the re-queue between attempts
resumes a row from `failed` (the report this loop is retrying) and from
`queued`/`held`, and refuses every other state, so a resolution that landed
meanwhile keeps the row while a transient provider failure still retries.
A bot's report decides reportability INSIDE the store's own write, so a
resolution that lands between the read and the write is not overwritten. Live
cards are held per delivery in memory and reclaimed when the delivery ends;
a full map drops the OLDEST cards, never every request's. `HitlResolved` cancels queued
retries, marks every live row `resolved`, retires sent cards, and remembers
the correlation so a replayed `hitl.requested` alerts nobody.

## Status and settings

- `GET /api/magician/v2/hitl/deliveries?correlation_id=` — the owner's
  value-free status: rows with addresses masked (`telegram:…01`), request →
  enqueue and enqueue → provider-acceptance latency p50/p95, and when each
  channel's bot last claimed (liveness is inferred from claims; the test
  action is the way to prove a channel end to end).
- `GET`/`PUT /api/magician/v2/settings/critical-delivery` — the
  `hitl.critical_delivery` section (`critical_delivery_settings.rs`, a
  light view + locked load-edit-save of the runtime `magician-config.yaml`,
  validated before the write, live after the existing reload; the
  coordinator takes the new policy on every reload). The envelope shows
  enabled channels with masked owner addresses and whether each has an
  owner, channels with an owner that are not yet enabled, whether the
  secure link can be built, and warnings. Owner addresses are never edited
  here — they live in `envoy`.
- `POST /api/magician/v2/settings/critical-delivery/test` — the owner's
  explicit test: a real `test` delivery through every enabled destination,
  never held by quiet hours. Opening or saving the settings page sends
  nothing.
- Unified UI: Settings → **Critical alerts** (`CriticalDeliveryPanel`).

Config (repo seed and template carry the section with its defaults; the
live runtime YAML is written only by the settings surface):

```yaml
hitl:
  critical_delivery:
    enabled_channels: []          # e.g. [telegram, kapso], in preference order
    policy: simultaneous          # or staged
    staged_fallback_secs: 45
    push_enabled: true
    # owner_ui_origin: https://ui.example.com
    # quiet_hours: {start: "22:00", end: "07:00", timezone: Asia/Kolkata, interrupt_for_time_bound: true}
```

`owner_ui_origin` is the origin the alert's link is built on. Unset, the link
falls back to `mobile_access.public_origin`, which is what a deployment wants
when one origin serves both the API and the owner-facing UI. Set it when they
are split: a device origin routed to the API cannot serve `/attention`, so the
link lands on a bodyless 404 the browser offers as a download. In a dev setup
that is the UI dev server's origin; once the runtime serves the built UI
itself (`magician.frontend.directory`) one origin serves both again and the
setting can go back to unset.

## Out of scope, on purpose

Group channels; actionable provider buttons for non-secret decisions (the
spec's "may" — credential asks never resolve through a button, and the
signed-callback contract is a separate design); other channel adapters until
they pass this contract (P7).

## Tests

`hitl_delivery::*::tests`, `critical_delivery_settings::tests`,
`hitl_delivery_api::tests`; bot SDK `channel-runtime.test.mjs`; unified-ui
`CriticalDeliveryPanel.component.test.ts`.
