# Changelog - Bots Workspace

All notable changes to the bots workspace will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this workspace follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
for its internal package versions.

---
## [Unreleased]

- Bot SDK `0.3.7`: carry real transport receipts and scoped delivery/reply acknowledgements; refuse dispatching arbitrary text for unknown cards.
- AgentMail `0.1.1`: admit signed delivery events durably, route parent replies to Work, bind recipient-only sends and keep existing webhook subscriptions.
- Fixed: the Telegram bot crashed at startup due to bundler renaming; bundles now keep names.
- Tracked Telegram and Kapso sends require a provider receipt and tracked WhatsApp replies a server acknowledgement; Telegram, AgentMail, WhatsApp, Kapso and Envoy requests have deadlines, so stalled or uncertain sends recover without sending twice.
- **Security:** strict shared mailbox parsing for Gmail and AgentMail prevents sender or reply-address spoofing via display names.
- **Security:** AgentMail webhooks are refused without a verified signing secret.
- Tracked replies are bound to the recorded recipient and exact body; Kapso cannot substitute templates for exact-text replies.
- Envoy outcomes are reported explicitly to Claims Review and recover after outages without duplicate sends.

### Added — critical-request alerts reach the owner's verified channels (`@magician/bot-sdk` 0.3.6, `@magician/bot-telegram` 0.2.2, `@magician/bot-kapso` 0.2.3; secure HITL P5)

- Bots open the realtime feed at start and deliver critical-request alerts to the owner's verified channel: the delivery is claimed, a value-free card is sent and the provider's result is reported. Retired requests edit or annotate the card. Nothing sent reveals the request's prompt, options or answer.
- Telegram cards carry an "Open secure request" button; Kapso uses an interactive message inside the 24-hour window and a configured template outside it, failing visibly without one.

### Fixed — the realtime feed a bot never opened (`@magician/bot-sdk` 0.3.6, secure HITL P5)

- Fixed: bots never opened the realtime feed (WebSocket protocol negotiation, and the connect ran after a long-polling adapter that never returns), so no critical alerts were delivered. Verified on a live Telegram bot.

### Security — a channel never solicits a secret (`@magician/bot-sdk`, secure HITL P3)

- **Security:** for sensitive asks (passwords, codes, sign-in details) the bot tells the owner to answer in the Magician app instead, and refuses and never forwards a reply typed in the channel while the ask is open (up to 15 minutes by default). `/stop` still works.

### Security — Kapso webhooks fail closed (`@magician/bot-kapso` 0.2.2)

- **Security, breaking:** `KAPSO_WEBHOOK_SECRET` is required; webhooks with a missing or bad signature get HTTP 401. Setup carries the secret into the bot env and warns when it is empty.

### Changed — runtime-derived bot credentials

- Runtime-managed bots no longer need a pasted `MAGICIAN_BEARER_TOKEN`; Magician mints a scoped bearer at spawn and revokes it on stop. Third-party provider credentials stay file-based.
- AgentMail inbox guidance and defaults use `magican@agentmail.to`.

---

Older entries: `docs/archive/changelogs/skillshub-bots.md`
