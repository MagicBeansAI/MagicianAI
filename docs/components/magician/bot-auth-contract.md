# Bot Auth Contract — `EX_NEEDS_AUTH` + Sidecar

## Purpose

A provider-agnostic way for a channel-bot adapter to tell the magician
supervisor "this bot cannot start or stay running because its account needs
authentication". The operator sees one uniform escalation in the UI attention
bar instead of each bot inventing its own flow (browser popup on startup, QR
codes in stdout).

- **Adapter half.** `@magician/bot-sdk` (`skillshub/bots/sdk`, 0.3.7)
  exports the primitives every adapter calls to publish needs-auth state.
- **Supervisor half.** Reads the sidecar, parks the bot in `Failed`,
  exposes `BotAuthSnapshot` via `GET /bots/auth` and `GET /bots/{name}/auth`,
  and dispatches the right login flow on operator "Authenticate"
  (`POST /bots/{name}/auth/start`). `BotManager::auth_state` reports a
  `BotAuthFlowState` of `Idle`, `Active` or `Queued`.

`BotManager::start_auth_with` adds sign-in methods for Service accounts on a
phone ([browser-signin.md](browser-signin.md#managed-bot-accounts)): headless
Google login whose URL goes only to the API's private sign-in session
(`auth_connect_material`); Telegram code login whose prompts become
`pending_input` (`phone`/`code`/`password`); WhatsApp pairing code via one-shot
`WHATSAPP_PHONE` and `WHATSAPP_PAIRING_CODE_FILE`. `cancel_auth` dequeues or
kills a login; `auth_availability` names a missing CLI, API key, OAuth client
or bundle. Before a Telegram login the supervisor seeds tgcli's store config
with `TGCLI_API_ID`/`TGCLI_API_HASH` (tgcli ignores those env vars and would
prompt on stdin). A successful Google login clears the needs-auth sidecar
without a bot restart.

A parent that does not treat exit 79 as needs-auth still sees the standard
auto-restart loop; the sidecar file is benign on disk.

## The contract

```
adapter → on auth probe failure:
            1. write JSON sidecar at ${MAGICIAN_BOT_AUTH_SIDECAR_PATH}
            2. process.exit(EX_NEEDS_AUTH)   // = 79

supervisor → on observing exit code 79:
            1. read sidecar (if present)
            2. set BotRuntimeState::Failed,
               last_error = "bot requires authentication"
            3. halt auto-restart
            4. surface in BotAuthSnapshot { status: NeedsAuth, ... }
                — read by GET /bots/{name}/auth and GET /bots/auth (bulk)

operator → clicks "Authenticate" in UI:
            POST /bots/{name}/auth/start
            → supervisor dispatches by sidecar.provider
              · google_workspace → spawn `gws auth login` out of process
              · telegram_self    → `tgcli auth --qr` (or `tgcli auth` for a
                                     code login), stdin fed by auth/input
              · whatsapp_web     → restart bot (wu-cli onQr fires)
              · kapso            → restart bot (API-key reread)
              · <custom>         → restart bot (generic fallback)
            → on success, clear sidecar, restart bot
```

## Sidecar shape

Written by the adapter at the path the supervisor injects via the
`MAGICIAN_BOT_AUTH_SIDECAR_PATH` env var at spawn time. Missing fields
serialize as `null`.

```json
{
  "provider": "google_workspace",
  "profile_label": "BUSINESS",
  "expected_account": "ops@example.com",
  "current_account": null,
  "detail": "no refresh token stored",
  "written_at_ms": 1715900000000
}
```

The supervisor parses these into `BotAuthSnapshot` fields of the same name
(`provider`, `profile_label`, `expected_account`, `current_account`,
`detail`), with `status: AccountMismatch` derived when both
`current_account` and `expected_account` are set and differ, else
`status: NeedsAuth`.

## Exit-code choice

`79` is unassigned in BSD `sysexits.h` (which spans `EX_USAGE = 64` through
`EX_CONFIG = 78`), so the supervisor can treat it as a private signal. The SDK
exports it as `EX_NEEDS_AUTH` so adapters don't hardcode the literal.

## SDK API (`@magician/bot-sdk`)

```ts
import {
  EX_NEEDS_AUTH,
  exitNeedsAuth,
  writeNeedsAuthSidecar,
  type BotAuthProvider,
  type NeedsAuthSidecar,
} from '@magician/bot-sdk';

// One-shot: write sidecar + exit. The common case.
exitNeedsAuth({
  provider: 'google_workspace',
  profileLabel: 'BUSINESS',
  expectedAccount: 'ops@example.com',
  detail: 'no refresh token stored',
});

// Or write without exiting (rare):
writeNeedsAuthSidecar({ provider: 'kapso', detail: 'API key invalid' });
```

Outside the magician supervisor (local dev), `writeNeedsAuthSidecar`
returns `false` (env var absent) and `exitNeedsAuth` still calls
`process.exit(EX_NEEDS_AUTH)` so the parent shell sees the same signal.

## Adapter rules

- **No inline OAuth/QR launch during startup** — every restart would pop a
  browser or QR; "Authenticate" is the explicit operator action.
- **No cross-process login locks** — the supervisor serializes through
  `BotManager::auth_state` (Idle/Active/Queued).
- **No swallowing auth failures into auto-restart loops** — if the underlying
  CLI exits with its own auth-failure code (e.g. gws code 5 mid-watch), route
  through `exitNeedsAuth`.

## Current adopters

| Bot | Version | Provider id | Probe |
|-----|---------|-------------|-------|
| `@magician/bot-gmail` | `0.2.4+` | `google_workspace` | `gws auth status --format json` |
| `@magician/bot-telegram-self` | `0.2.2+` | `telegram_self` | `tgcli channels --limit 1 --json` |

The Gmail probe checks stored credentials locally, so it can accept ones Google
refuses. When Google rejects the watcher's credentials (`UNAUTHENTICATED`, 401,
`ACCESS_TOKEN_TYPE_UNSUPPORTED`) on two consecutive watcher runs, the Gmail bot
makes one read-only Gmail call (`gmail users getProfile`) with the same
credentials. Only when Gmail rejects them too does it exit needs-auth with its
sidecar. When Gmail accepts them, a new sign-in cannot clear the refusal: `gws`
caches one access token per scope set, and signing in replaces the credentials
but not the watch's cached entry. The bot renames `token_cache.json` to
`token_cache.rejected-<UTC stamp>.json` once and restarts the watcher; a
refusal that survives the fresh token exits 70 (the account's Google Cloud
access or project roles), as permission and quota errors do. The watcher reuses one Pub/Sub topic per
profile: the topic `gws` created on first watch activation is kept in
`$GWS_CONFIG_DIR/gmail-watch-topic.json` (mode `0600`) and passed back with
`--topic` for the same project; it is dropped when Google reports it not found.
The subscription that run creates is saved beside it (`subscription`), and later
runs pass `--subscription` instead, so a restart no longer leaves a new
subscription behind. That mode skips `gws`'s setup, including the 7-day Gmail
watch renewal, so the bot calls `gmail users watch` on the saved topic before
each such run and daily while it runs. A failed renewal falls back to `--topic`
(saving the new subscription); a subscription Google reports not found is
dropped and the topic kept.

WhatsApp (wu-cli) and Kapso are not on the contract: their auth is
connect-and-auth-in-one (QR appears during connect), which does not decompose
into exit-79 + external login. They keep inline behaviour.

`@magician/bot-agentmail` (inbound email envoy, `skillshub/bots/agentmail/`) is
also off the contract: it uses a static inbox-scoped `AGENT_MAIL_KEY`, so there
is no needs-auth state. It is a `ChannelAdapter` (`AgentMailAdapter`,
`channelType: "agentmail"`):

- **Inbound.** `message.received` webhook → sender parsed from `from` (bare
  email, **lower-cased** to match the owner allowlist and thread stably), body
  from `text` / HTML→text / `preview` → `handlers.onTextMessage`. Webhook server:
  svix verification gated on `AGENTMAIL_WEBHOOK_SECRET`, idempotent
  `webhooks.create` self-register, port `3011`, served at
  **`/agentmail-webhook`** (distinct from kapso's `/webhook` so both can share one
  public host).
- **Outbound.** `adapter.sendText` sends a **threaded reply** via
  `inboxes.messages.reply(inboxId, messageId, { text })`. Send errors are logged,
  never thrown.
- **Sender authenticity.** The payload exposes no SPF/DKIM/DMARC result, so
  `from` is spoofable here. The adapter does **not** authorize; the owner
  allowlist is enforced downstream by the `agentmail-processing` skill.

See [`consumer_channels.md`](../../consumer_channels.md).

## Status freshness — poll-path probe caching

The attention bar polls `GET /bots/auth` → `BotManager::list_auth` →
`auth_snapshot` per bot. The **sidecar is consulted first**, so a real auth
failure (exit 79) surfaces on the next poll regardless of caching.

Only with a clean sidecar does it fall back to a live probe
(`read_google_workspace_status` → `gws auth status --format json`, which shells
to `gcloud`). OAuth credentials are a setup-time property, so the
healthy-confirmation result is cached (`probe_google_workspace_status` is the
live body):

- **TTL** `GWS_STATUS_PROBE_TTL` (10 min), keyed by
  `(gws_config_dir, expected_account)`.
- **Force-fresh** on `POST /bots/{name}/auth/start` → `gws auth login`, so a
  just-completed login shows immediately.
- Caching only the healthy fallback is safe because failures arrive via the
  sidecar.

Every Magician-managed gws subprocess sets `CLOUDSDK_CONFIG` to
`<gws_config_dir>/cloudsdk` alongside `GOOGLE_WORKSPACE_CLI_CONFIG_DIR`, so
transient Cloud SDK configs and logs stay inside the per-account auth profile
instead of `<scope>/workdirs/home/.config/gcloud`.

## Receiver setup notes

- AgentMail's verified receiver needs `AGENTMAIL_WEBHOOK_SECRET` or a
  registration URL that supplies a provider signing secret. Bot-env setup warns
  when both are absent and preserves existing operator env files.
- A signing secret admits verified inbound events; full delivery status also
  needs delivered, bounced and complained subscriptions on the provider hook.
  Configuring the registration URL reuses the bot's stable client ID and merges
  those subscriptions, keeping existing event types. Verify hook ID,
  destination and signing authority on readback; a signed no-op proves only
  transport.
- A second machine can host a receiver without changing the existing webhook
  destination; local readiness alone does not prove public routing.
- Telegram polling has one runtime owner per token: when another server owns
  it, stop and disable the local bot, keeping its token and configuration.
