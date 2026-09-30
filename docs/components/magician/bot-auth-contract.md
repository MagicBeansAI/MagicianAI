# Bot Auth Contract — `EX_NEEDS_AUTH` + Sidecar

## Purpose

A provider-agnostic way for channel-bot adapters to signal "this bot cannot
start (or stay running) because the underlying account needs authentication"
to the magician supervisor, so the operator sees a single uniform escalation
in the UI attention bar instead of every bot inventing its own auth flow
(browser popup on startup, QR codes in stdout, etc.).

- **Adapter half.** `@magician/bot-sdk` `v0.3.5` exports the primitives
  every adapter calls to publish needs-auth state.
- **Supervisor half.** Reads the sidecar, parks the bot in `Failed`,
  exposes `BotAuthSnapshot` via `GET /bots/auth` and `GET /bots/{name}/auth`,
  and dispatches the right login flow on operator "Authenticate"
  (`POST /bots/{name}/auth/start`). `BotManager::auth_state` is
  `Active`/`Queued`.

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
              · telegram_self    → restart bot (tgcli QR shows in logs)
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
`EX_CONFIG = 78`). No real CLI tool returns 79 by accident, so the
supervisor can treat it as a private signal without false positives. The
adapter SDK exports it as `EX_NEEDS_AUTH` so adapters don't hardcode the
literal.

## SDK API (`@magician/bot-sdk` `v0.3.5`)

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
  `BotManager::auth_state` (Active/Queued).
- **No swallowing auth failures into auto-restart loops** — if the underlying
  CLI exits with its own auth-failure code (e.g. gws code 5 mid-watch), route
  through `exitNeedsAuth`.

## Current adopters

| Bot | Version | Provider id | Probe |
|-----|---------|-------------|-------|
| `@magician/bot-gmail` | `0.2.3+` | `google_workspace` | `gws auth status --format json` |
| `@magician/bot-telegram-self` | `0.2.2+` | `telegram_self` | `tgcli channels --limit 1 --json` |

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
