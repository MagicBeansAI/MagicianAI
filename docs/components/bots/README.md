# Bots Docs

Landing page for the `skillshub/bots/` workspace documentation.

## Canonical References

- [Bots Workspace README](../../../skillshub/bots/README.md)
- [Bots Workspace Changelog](../../../skillshub/bots/CHANGELOG.md)
- [Consumer Channels Design](../../consumer_channels.md)
- [Magician Docs](../magician/README.md)

## Current Scope

Each platform is a supervised per-scope daemon over `@magician/bot-sdk`
(`ChannelAdapter` + `ChannelRuntime`). The seven packages:

- `@magician/bot-sdk` — Magician HTTP/WebSocket client, channel runtime, adapter contract
- `@magician/bot-telegram` — official Telegram bot (Telegraf)
- `@magician/bot-telegram-self` — Telegram user-bot (tgcli)
- `@magician/bot-whatsapp` — consumer-account bridge with QR pairing and local reads
- `@magician/bot-kapso` — public WhatsApp number-webhook bridge. Requires
  `KAPSO_WEBHOOK_SECRET` and authenticates `X-Webhook-Signature` against the
  exact raw body before parsing or dispatching an inbound message
- `@magician/bot-gmail` — Gmail via `gws` OAuth, multi-account
- `@magician/bot-agentmail` — AgentMail inbound webhook

`/channels` and `/api/magician/v2/bots/*` are the operational control plane
for configured bot processes.

## Deferred Channels

Discord remains intentionally deferred. Slack and iMessage are still future
expansion paths rather than active implementation work.
