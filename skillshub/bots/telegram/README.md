# `@magician/bot-telegram`

Telegram bot package for Magician consumer channels.

It currently supports:

- private-chat `/start` welcome/connect; enrollment is also implicit on any
  normal inbound message
- plain text inbound messages as ordinary public envoy chat
- `/magic <request>` control messages for Magician work/control flow
- `/new` to start a fresh Telegram conversation session
- plain text delivery for runtime summaries and task updates

The Magician API credential (`MAGICIAN_BEARER_TOKEN`) is **not** listed
below. Injected by the runtime at spawn, not configured: a scoped in-memory
token minted for this bot's `(principal, workspace)` and revoked when it
stops. Nothing is stored on disk. A value set in the env file is
overridden by the injected one.

Required environment variables:

- `TELEGRAM_TOKEN`

Optional environment variables:

- `MAGICIAN_URL` (default `http://127.0.0.1:3002`)
- `MAGICIAN_REALTIME_ENABLED` (default `true`)
- `MAGICIAN_CONTROL_PREFIX` (default `/magic`; legacy `@magic` is also
  accepted)
- `TELEGRAM_DROP_PENDING_UPDATES` (default `false`)

Ordinary non-prefixed messages are sent with
`source_surface=telegram-envoy-chat` and `control_intent=false`, so they use the
same public-chat admission, first-contact, queue, and budget policy as Kapso
ordinary chat. Send `/magic <request>` to enter the Magician control/work rail.

## Telegram chat id for owner control

The adapter uses `String(ctx.chat.id)` as the channel address. For a private
Telegram chat this is the numeric chat id to place in
`MAGICIAN_OWNER_TELEGRAM_IDENTITIES` when enabling owner-control `/magic`
routing.

To find it locally, send any message to the bot and inspect the resulting chat
session file:

```bash
find ~/MagicianNotes/scopes -path '*/ui/chat_sessions/*/session.json' -exec jq -r 'select(.session.origin_channel.channel_type == "telegram") | [.session.origin_channel.address, (.session.title // ""), .session.ui_thread_id, input_filename] | @tsv' {} +
```

The same value appears as `session.origin_channel.address` in the session JSON.
