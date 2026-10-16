# Critical-request delivery

How a critical human-in-the-loop request (a credential or verification-code ask,
or a decision with a deadline) reaches the owner's verified private
destinations: registered mobile push and the channel bots the owner enabled
(Kapso WhatsApp, the Telegram bot). Design: secure HITL plan §6.1
(`docs/archive/plans/2026-09-15-secure-hitl-credentials-and-otp.md`;
implementation `docs/archive/plans/2026-09-19-secure-hitl-otp-implementation.md`).

Attention stays the authoritative pending request whatever the providers do: a
delivery is a projection of `hitl.requested`, never a second request, and the
owner answers only in the authenticated UI.

## The coordinator

`magician/src/magician_v2/hitl_delivery/` (`DeliveryCoordinator`) owns one
subscription to the canonical `HitlRequested` / `HitlResolved` lifecycle. The
composition root starts it for every runtime (bots may be enabled without push)
and publishes it process-wide (`hitl_delivery::global()`) for the API.

**Criticality is structural** (`alert::Criticality::of`): a request is critical
when its `input_schema` carries the value-free sensitivity spec (`sensitive`) or
names a `collection_deadline_ms`. An urgency word in a prompt is not enough.
Other requests keep the plain attention push to registered devices, with no
channel fan-out and no record.

**Destinations** (`policy::DeliveryPolicy`): the channel types in
`hitl.critical_delivery.enabled_channels`, in preference order, resolved to
owner addresses through `EnvoyConfig::owner_identities_for`, the same authority
`chat::envoy::is_owner` uses. Registration is eligibility, not proof of the
recipient: the last inbound sender, an envoy contact, a display name, group
membership or a model-provided id is never a destination. A channel without an
owner identity is shown on the settings surface and recorded `unavailable`.
Group channels are outside this contract.

**One deduplication decision.** When the request belongs to a chat session
whose origin channel equals a destination (same type and address), that
destination is recorded `relayed_by_origin` and not sent, but only while a relay
actually injects the ask into that chat (`coordinator::origin_relay_is_live`,
which follows the chat escalation injector's taxonomy gate
`chat_surface_renders_agent_event("hitl.requested")`). That gate is off today,
so the origin channel gets the alert as an ordinary destination. Push is never
deduplicated against a channel; the push dispatcher deduplicates it per
registration with its own watermark.

**Policy.** `simultaneous` offers every destination at once; `staged` offers the
first and moves on only if the provider has not accepted within
`staged_fallback_secs` (a destination that accepts marks the rest `skipped`).

**Retrieval grace** (§6.2): a critical `otp` ask in a scope where automatic
code retrieval is expected (`RetrievalOracle`) waits
`hitl.verification_codes.retrieval_grace_secs` (20 s) before its *channel*
alerts go out. Push is never delayed; a deadline under 90 s skips the grace; a
code arriving inside it resolves the ask with no alert; and a
`VerificationRetrievalStatus` of `ambiguous` or `unavailable` ends the grace at
once.

**Quiet hours** hold an alert (`held`) until the window ends, unless the request
is time-bound and `interrupt_for_time_bound` is on; the owner's own test is
never held. An unparseable window holds nothing, since misconfiguration must
never silence alerts. When the window ends the request is rechecked before
sending.

## The alert

`alert::AlertCard` is value-free by construction:

| Field | Source |
| --- | --- |
| `service_alias` | the host of the destination a challenge bound the request to (`expected_destination`), else "a service"; never a producer- or model-authored label |
| `reason` | the kind: "a verification code", "your password", "your sign-in details", "a private value", "a decision" |
| `deadline_ms` | the spec's collection window when it has one |
| `open_url` | `{public_origin}/attention?attention=1&attention_item={id}` when an owner-link origin is configured (`hitl.critical_delivery.owner_ui_origin` first, then `mobile_access.public_origin` or its env fallback), else absent and the text says to open Magician → Attention. `attention_item` opens the exact item and `attention=1` keeps the list behind it, so a resolved request shows "Item no longer available … may have been resolved" rather than a form or a blank page |
| `text` | the one sentence every channel sends, with a relative expiry and "Don't reply with the value here." |

Never the prompt, hint, option labels, a mailbox preview, a token or an answer.
The link carries no authority: the page is behind login and scope, a GET is
read-only, and knowing the id claims nothing.

## Bots claim, never receive addresses

`RuntimeTransportEvent::CriticalRequestAlert { delivery_id, correlation_id,
channel_type, kind, alert, revision, deadline_ms, … }` is scoped
(`event_visible_to_scope`) and carries the card and **no address**. The
channel's bot (bot SDK `ChannelRuntime`, which opens the realtime feed at start
and reconnects with backoff) claims it:

- `POST /api/magician/v2/hitl/deliveries/{id}/claim` `{channel_type,
  connection_generation}`: `mag_bot_` bearer only; the bot's name must equal the
  channel type; the record must be `queued` in the bearer's scope. It returns the
  owner address and the card, binds the record to `bot:<name>` and the
  connection generation, and moves it to `claimed`. The winner is whoever made
  that transition. The SDK's generation includes a per-process token, so two
  processes of one channel cannot collide on a counter.
- `POST …/{id}/report` `{status: provider_accepted | confirmed_delivered |
  failed, provider_message_id?, reason?, connection_generation?}`: only the
  claiming connection. One bot name can be two processes (an orphan beside its
  replacement), so a report naming a different generation is refused
  `not_the_claimant`. A report naming none (older SDK) is admitted.
- `CriticalRequestRetired { delivery_id, outcome }` tells the claiming bot to
  edit (Telegram) or annotate (Kapso) a card sent within the last 24 hours
  (`MAX_RETIRE_AGE_MS` in the bot SDK's `channel-runtime.ts`);
  older cards are left alone and their link shows the completed state. A bot
  whose `provider_accepted` report is refused because the request closed between
  claim and send retires the card itself (`superseded`).

Timeouts and terminal outcomes:

- A push wave whose every registration names a platform with no provider on
  this runtime is `unavailable` once ("no push provider is configured on this
  runtime"); retrying cannot help.
- An alert nobody claims within `CLAIM_WINDOW` (10 s) is `unavailable` (no bot
  connected), and the policy's fallback runs.
- A claimed delivery with no report within `REPORT_WINDOW` (30 s) is
  `ambiguous`. The send may have happened and Telegram and Kapso offer no
  idempotency key, so it is **not retried** on that destination: the documented
  duplicate-alert boundary.

Providers: Telegram sends the card with an inline **Open secure request** URL
button and edits it on retirement. WhatsApp through Kapso sends an interactive
CTA-URL message inside the 24-hour customer-service window and, outside it, only
the configured fallback template (`KAPSO_FALLBACK_TEMPLATE_NAME`, the text as its
body variable). Without a template the send fails honestly and the fallback
destination runs.

## Records

`records::DeliveryStore` is `system/hitl-deliveries.json` under the data root:
private (`0600`), durable, bounded (`MAX_RECORDS` 2048, oldest terminal rows
evicted first). It holds routing and status only: scope, correlation id,
revision, destination (channel type + address), state, attempts,
`requested_at_ms`, `enqueued_at_ms`, `claimed_at_ms`, `accepted_at_ms`, provider
message id, a bounded value-free reason, the claim binding, and the ask's lane
(`source`, `execution_id`) and `deadline_ms`. Never the card, a prompt or an
answer. States: `queued`, `held`, `claimed`, `provider_accepted`,
`confirmed_delivered`, `failed`, `unavailable`, `ambiguous`, `expired`,
`resolved`, `relayed_by_origin`, `skipped`. Provider acceptance does not prove
the owner saw the alert.

**A restart re-drives, it does not retire.** No task survives the process, but
the ask may still be pending and nothing else re-announces it (the realtime feed
replays nothing; a republished `hitl.requested` for a pending key is not
re-broadcast). `DeliveryCoordinator::recover_after_restart` reads the
coordinator's rows at boot, awaited **before** it subscribes, so fresh rows from
a mid-recovery announcement cannot join a recovery group. For each live
correlation it asks the same `RequestOracle` every send asks, then either retires
the stale rows and offers a fresh set (with the generic card; the prompt is never
persisted) or retires them with the reason (`the request closed while the
runtime was down`). A row with no `source` can only be retired.

**One writer.** The store rewrites its whole row set on every commit under an
in-process lock, so it assumes one runtime per data root, as the rest of the
runtime does. Two processes on one root would overwrite each other's rows; the
durable user-request store takes a cross-process writer lease for this reason
and the delivery log does not. Accepted limit: the log is routing and status
metadata, never the credential path.

Every send is preceded by a recheck (`RequestOracle`): the request is still
pending in its scope (`UserRequestService::pending_request_snapshot`, or the
agentic pause store for `source: agentic`), its deadline has not passed, and it
has not resolved. Retries: at most 3 attempts per destination with 5 s and 30 s
backoff, each rechecked. The re-queue resumes a row only from `failed`,
`queued` or `held`, so a resolution that landed meanwhile wins while a transient
provider failure still retries. A bot's report is checked inside the store's own
write, so a resolution between read and write is not overwritten. Live cards are
held in memory per delivery and dropped when it ends; a full map
(`MAX_LIVE_CARDS`, 4,096) drops the oldest cards. `HitlResolved` cancels queued retries, marks live rows `resolved`,
retires sent cards and remembers the correlation so a replayed `hitl.requested`
alerts nobody.

## Status and settings

- `GET /api/magician/v2/hitl/deliveries?correlation_id=`: the owner's value-free
  status: rows with masked addresses (`telegram:…01`), request → enqueue and
  enqueue → provider-acceptance latency p50/p95, and when each channel's bot last
  claimed (liveness is inferred from claims; the test action proves a channel end
  to end).
- `GET`/`PUT /api/magician/v2/settings/critical-delivery`: the
  `hitl.critical_delivery` section (`critical_delivery_settings.rs`, a light view
  plus locked load-edit-save of the runtime `magician-config.yaml`, validated
  before the write and live after the reload; the coordinator takes the new
  policy on every reload). It shows enabled channels with masked owner addresses
  and whether each has an owner, channels with an owner not yet enabled, whether
  the secure link can be built, and warnings. Owner addresses live in `envoy` and
  are never edited here.
- `POST /api/magician/v2/settings/critical-delivery/test`: the owner's explicit
  test, a real `test` delivery to every enabled destination, never held by quiet
  hours. Opening or saving the page sends nothing.
- Unified UI: Settings → **Critical alerts** (`CriticalDeliveryPanel`).

Config (the repo-root seed `magician-config.yaml` carries the section with
defaults; the live runtime YAML is written only by the settings surface):

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

`owner_ui_origin` is the origin the alert link is built on. Unset, it falls back
to `mobile_access.public_origin`, which suits a deployment where one origin
serves both the API and the owner UI. Set it when they are split: an API-only
device origin cannot serve `/attention`, and the link would land on a bodyless
404 the browser offers as a download. In dev that is the UI dev server's origin;
once the runtime serves the built UI (`magician.frontend.directory`) it can go
back to unset.

## Out of scope, on purpose

Group channels; actionable provider buttons for non-secret decisions (credential
asks never resolve through a button, and the signed-callback contract is a
separate design); other channel adapters until they pass this contract.

## Tests

`hitl_delivery::*::tests`, `critical_delivery_settings::tests`,
`hitl_delivery_api::tests`; bot SDK `channel-runtime.test.mjs`; unified-ui
`CriticalDeliveryPanel.component.test.ts`.
