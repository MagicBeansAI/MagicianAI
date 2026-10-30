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
- `@magician/bot-kapso` — public WhatsApp number-webhook bridge. On start it
  registers `https://${MAGICIAN_WEBHOOK_HOST}/webhook` (or `KAPSO_WEBHOOK_URL`)
  on the number (active, unbuffered), deactivates other Magician machines'
  `/webhook` hooks for that number, and verifies `X-Webhook-Signature` over the
  exact raw body with that registration's secret (`KAPSO_WEBHOOK_SECRET` only
  without a webhook host, or as the fallback when Kapso is unreachable)
- `@magician/bot-gmail` — Gmail via `gws` OAuth, multi-account
- `@magician/bot-agentmail` — AgentMail inbound webhook

`/channels` and `/api/magician/v2/bots/*` are the operational control plane
for configured bot processes.

## Deferred Channels

Discord remains intentionally deferred. Slack and iMessage are still future
expansion paths rather than active implementation work.
