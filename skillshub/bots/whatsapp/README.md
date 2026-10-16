# `@magician/bot-whatsapp`

WhatsApp bot package for Magician consumer channels.

It currently supports:

- dedicated 1:1 WhatsApp chats only
- Baileys QR-code auth with persisted multi-file session state
- local message-cache collection for observation/history reads
- local message-cache reads via `node dist/index.js read <number> --since 5m`

The Magician API credential (`MAGICIAN_BEARER_TOKEN`) is **not** listed
below. Injected by the runtime at spawn, not configured: a scoped in-memory
token minted for this bot's `(principal, workspace)` and revoked when it
stops. Nothing is stored on disk. A value set in the env file is
overridden by the injected one.

Required environment variables:


Optional environment variables:

- `MAGICIAN_URL` (default `http://127.0.0.1:3002`)
- `MAGICIAN_REALTIME_ENABLED` (default `true`)
- `WU_HOME` (default `$HOME/.wu`; Magician scopes this to `<scope>/workdirs/home/.wu`)
- `WHATSAPP_QR_FILE` (no default; unset means no QR image is written. The bot
  runtime seeds a scope-local path when the bot config does not set one)
- `WHATSAPP_STATUS_FILE` (connected marker; seeded by the runtime like the QR file)
- `WHATSAPP_PHONE` (optional; requests pairing-code login for this number)
- `WHATSAPP_SELF_JID` (optional fallback; accepts bare phone numbers, `+`-prefixed
  phone numbers, or full WhatsApp JIDs)

Auth bootstrap is handled by `wu-cli`. If the scoped `WU_HOME/auth` directory is missing,
startup will request QR or pairing-code login instead of failing closed.

## Inbound chat routing

The local consumer bridge does not forward the account's own WhatsApp self-chat
("Note to self") into Magician chat. Ordinary self-chat messages remain ordinary
WhatsApp history for observation/distillation and do not enroll, open a session,
or start an agent turn. Kapso is the WhatsApp control surface: messages to Kapso
must start with the configured control prefix (default `/magic`; legacy
`@magic` is accepted as an alias) to enter Magician control; non-prefixed Kapso
messages stay on the envoy lane.

The adapter must not forward every outbound message just because WhatsApp marks
it as `fromMe`; a message sent from the phone to any other contact is also
`fromMe`.

WhatsApp may identify the same self-chat with either:

- the phone-number JID, for example `919876543210@s.whatsapp.net`
- the linked-device LID JID, for example `210000000000001@lid`

The adapter therefore builds a self-alias set from the live socket, the scoped
`WU_HOME/auth/creds.json`, and optional `WHATSAPP_SELF_JID`. This protects the
message cache and local tooling from confusing every outbound `fromMe` event with
the self-chat.

## Deployment model — different from the other bots

The other bots in this workspace (gmail, kapso, telegram, telegram-self) ship as
a single self-contained `dist/index.js` produced by esbuild. Whatsapp deviates.

**Why:** the dep chain `@ibrahimwithi/wu-cli` → `pino` → `baileys` uses several
patterns that don't survive bundling cleanly:

1. **`pino` worker_threads.** Pino spawns a child Node process to handle async
   logging. The child loads its transport module via a file-system path that
   only exists in the original `node_modules/pino/...` tree. When pino is
   bundled, the path is wrong and the worker errors out.
2. **`_require("../../package.json")`** for self-version introspection. wu-cli
   builds a runtime require via `createRequire(import.meta.url)` and reads its
   own version from a relative path. Once bundled into a single file, that
   relative path no longer resolves against the original wu-cli location.
3. **`__dirname`-based resource resolution.** Several inner helpers compute
   resource paths relative to `__dirname`, which esbuild ESM output does not
   provide for nested module bodies.

We chased each layer for an afternoon. Each fix uncovered the next. The honest
conclusion: pino + bundling is a known-hard problem in the JS ecosystem; the
upstream maintainers explicitly recommend not bundling pino.

**What we do instead.** Whatsapp's bundle (`dist/index.js`, ~1 MB) inlines its
own source plus `@magician/bot-sdk` but keeps `@ibrahimwithi/wu-cli` as
runtime-external (esbuild `--external @ibrahimwithi/wu-cli`). At install time,
`skillshub/scripts/install_bot_bundles.py` symlinks
`<scope>/bots/whatsapp/dist/index.js` to the source bundle. Node resolves
imports from the symlink target, so `@ibrahimwithi/wu-cli` is found in the
hoisted `skillshub/node_modules/` installed once by the npm workspace
(`skillshub/package.json`). **`skillshub/` must therefore exist at runtime.**

Scopes no longer carry a per-bot `package.json` or `node_modules/`; the
installer removes those legacy files on its next run.

**If you bump `@ibrahimwithi/wu-cli` in this package's `package.json`,**
re-run `make -C skillshub setup-deps` so the workspace root picks it up.

**If you want a fully bundled whatsapp** in the future, the upstream paths are
either (a) replace `@ibrahimwithi/wu-cli` with a bundle-friendly Baileys
wrapper, or (b) wait for esbuild's worker_threads handling to mature.
