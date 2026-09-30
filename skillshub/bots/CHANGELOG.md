# Changelog - Bots Workspace

All notable changes to the bots workspace will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this workspace follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
for its internal package versions.

---
## [Unreleased]

- Validate tracked Telegram message receipts and apply complete-request deadlines to Telegram and AgentMail sends.

- Require a provider receipt for every tracked Kapso chunk and bound stalled provider requests without resending messages.

- Bound stalled WhatsApp sends and Envoy receipt HTTP requests so unknown outcomes can recover without repeating outbound messages.

- Share strict mailbox parsing across Gmail and AgentMail so quoted display names cannot substitute another sender or reply address.

- Wait for WhatsApp server acknowledgement on tracked replies and refuse AgentMail webhooks without a verified signing secret.

- Prevent duplicate tracked AgentMail sends from SDK retries and forbid Kapso template substitution for exact-text replies.

- Recover Envoy receipt reports after longer service outages without repeating the external send.

- Recover lost Envoy Begin responses safely and bind Gmail replies to the actual recipient and exact MIME body; constrain AgentMail tracked replies to the recorded recipient.

- Report explicit Envoy adapter acceptance to Claims Review and prevent duplicate sends when delivery or receipt outcomes are uncertain.

### Added — critical-request alerts reach the owner's verified channels (`@magician/bot-sdk` 0.3.6, `@magician/bot-telegram` 0.2.2, `@magician/bot-kapso` 0.2.3; secure HITL P5)

- `ChannelRuntime` opens the realtime feed at `start()` (with the bearer's
  own scope, before any inbound message), reconnects it with backoff when
  the socket drops, and counts connection generations. A
  `CriticalRequestAlert` for the runtime's channel type is **claimed** over
  the authenticated API (`claimCriticalDelivery`) — the event carries no
  address; the claim hands over the owner address for exactly that delivery
  and binds it to this bot and connection generation — then the value-free
  card is sent (`adapter.sendCriticalAlert` when the adapter has one, else
  `sendText` with the card's `text`) and the provider's answer is
  **reported** (`reportCriticalDelivery`: `provider_accepted` with the
  provider message id, or `failed` with a bounded reason). A claim the
  backend refuses (another bot was first, the request resolved) sends
  nothing. `CriticalRequestRetired` edits or annotates a card this process
  sent (`adapter.retireCriticalAlert`). Nothing the runtime sends names the
  request's prompt, options or answer; a reply typed on the channel is still
  refused while a secret ask is open (P3).
- Telegram: the card carries an inline **Open secure request** URL button;
  retirement edits the message. Kapso: inside the 24-hour window the card is
  an interactive CTA-URL message; outside it the configured fallback
  template carries the text as its body variable, and without a template the
  send fails honestly (recorded, never dropped). Retirement is a one-line
  follow-up reply to the sent card.

### Fixed — the realtime feed a bot never opened (`@magician/bot-sdk` 0.3.6, secure HITL P5)

- `connectRealtime` now offers `magician-events-v2` alongside the bearer. The
  runtime negotiates against its own protocol list and echoes the one it
  selected; the bearer alone left it nothing it recognised, so it selected none
  and omitted `Sec-WebSocket-Protocol` from the `101` — which a strict client
  (Node's built-in `WebSocket`) must reject. The bearer still rides second,
  because a WebSocket cannot carry an `Authorization` header, and the runtime
  never echoes it back.
- `ChannelRuntime.start()` opens the feed **before** awaiting the adapter. A
  long-polling adapter's `start` does not return until the bot stops —
  Telegraf's `launch()` awaits its polling loop — so the connect sequenced
  after it was unreachable for exactly the bots that needed it.
- Together these meant a bot opened no realtime feed at all: it received no
  critical-request alert and claimed no delivery, silently, and every alert
  recorded `unavailable — no bot claimed the alert`. Verified on a live
  Telegram bot, which now reaches `provider_accepted` with a provider message
  id.

### Security — a channel never solicits a secret (`@magician/bot-sdk`, secure HITL P3)

- `ChannelRuntime` relays a notice instead of the question for an escalation
  whose `input_schema.sensitive` is set, whose `input_type` is `password` or
  `otp`, or whose `request_type` is a built-in secure browser ask: it names
  what is needed (a verification code, your password, your sign-in details),
  says to enter it in the Magician app or web UI, and says not to send it
  here. While that ask is open on the session, an inbound reply is answered
  with `SENSITIVE_REPLY_REFUSAL` and never forwarded (it is not the answer,
  and must not become transcript); the hold clears on `escalation_resolved`,
  at the spec's `collection_deadline_ms`, or after `SENSITIVE_ASK_REPLY_HOLD_MS`
  (15 min) when the spec names no window; `/stop` still cancels the run.
  `sensitiveAskNotice`, `sensitiveAskExpiry` and the constants are exported
  for the SDK tests. Secure links and owner-channel delivery follow in P5.

### Security — Kapso webhooks fail closed (`@magician/bot-kapso` 0.2.2)

- `@magician/bot-kapso` now requires `KAPSO_WEBHOOK_SECRET` and verifies
  Kapso's `X-Webhook-Signature` HMAC-SHA256 over the exact raw request bytes
  with a timing-safe comparison before acknowledging or processing a webhook.
  Missing, malformed, or incorrect signatures receive HTTP 401.
- Bot-env setup now carries the secret into the scoped Kapso runtime env and
  warns when the value is empty.

### Changed — runtime-derived bot credentials

- Runtime-managed bots no longer require a pasted `MAGICIAN_BEARER_TOKEN` in
  their environment files. Magician mints a scope-qualified, process-local
  bearer at spawn, injects it after file-based values, and revokes it on stop.
- The AgentMail inbox guidance and defaults now use
  `magican@agentmail.to`; third-party provider credentials remain file-based.

---

Older entries: `docs/archive/changelogs/skillshub-bots.md`
