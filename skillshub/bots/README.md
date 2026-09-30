# Bot Templates Workspace

This directory is the shipped template source for consumer-channel bots.
**Adding a new messaging platform is implementing the `ChannelAdapter`
interface from [`@magician/bot-sdk`](./sdk/README.md) — six methods, the SDK
handles everything else.**

- **[`sdk/`](./sdk/README.md)** — the channel-adapter framework. Typed
  `ChannelAdapter<TTarget, TContext>` interface + `ChannelRuntime` that owns
  the Magician HTTP/WebSocket client, enrollment flow, session lifecycle,
  tool-call rendering, and file-attachment delivery. **Read this first.**
- `telegram/` — Telegram bot template using Telegraf.
- `telegram-self/` — Telegram user-bot variant (self-mode account).
- `whatsapp/` — WhatsApp bot built on [wu-cli](https://github.com/ibrahimhajjaj/wu-cli).
- `gmail/` — Gmail bot (OAuth, multi-account).
- `kapso/` — Kapso channel adapter.
- `agentmail/` — AgentMail email channel adapter.
- Future platform bots should stay thin and implement the shared adapter
  contract rather than reimplementing the Magician chat flow.

**Process topology:** unlike monolithic gateway designs (e.g. Hermes's single
`gateway` process hosting every platform), Magician runs each bot as its own
scope-aware daemon supervised by `magic-supervisor`. This gives per-platform
process isolation (one bad adapter can't crash the others), parallel restart,
and independent dependency chains — at the cost of a slightly larger config
surface per scope. The SDK is the shared library, not a shared process.

## Layout

- Bot source lives here under `skillshub/bots/<bot>/` (TypeScript). Each
  bot is built into a `dist/index.js` via esbuild (`build_bundle.mjs`).
  For most bots (gmail, kapso, telegram, telegram-self) the bundle is
  fully self-contained — npm deps are inlined and the bot can be
  launched with just `dist/index.js` plus an env file.
  - **Whatsapp deviates** — its dep chain (`@ibrahimwithi/wu-cli` →
    `pino` → `baileys`) is bundle-hostile (worker_threads, runtime
    `_require("../../package.json")`, `__dirname`-based resolution).
    Its bundle externalizes `@ibrahimwithi/wu-cli`, which resolves at
    runtime from the hoisted `skillshub/node_modules/` through the
    symlinked bundle. See `whatsapp/README.md` for the full rationale.
- Per-scope deployment lives at `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/<bot>/` (default `~/MagicianNotes`):
  - `dist/index.js` — symlink to the source bundle, created by `install-bot-bundles`.
  - `.env.<account>` (gmail) or `.env.development` (others) — per-instance secrets,
    populated by `setup-bot-envs` from `operator-config.yaml` (runtime root, else `skillshub/`).
- `bot_configs.yaml` lives at `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/bot_configs.yaml`
  (template seeded from `skillshub/bots/bot_configs.yaml` on first scope materialization).
- OAuth state lives at `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/auth/gws-<account>/` (gws bots).

## Setup

```bash
# Build all bot bundles + deploy to default scope.
make -C skillshub setup-bots
make -C skillshub install-bot-bundles  # symlinks skillshub/bots/<bot>/dist/index.js → scope
make -C skillshub setup-bot-envs       # writes per-bot .env from skillshub/operator-config.yaml
```

The runtime auto-materializes scope bot dirs + bundles on first scope touch via
`materialize_scope`; the make targets above just front-load that work.

## Auth and env files

- Per-bot env files at `<scope>/bots/<bot>/.env.<account>` (or `.env.development`).
  Source: `operator-config.yaml` (runtime root, else `skillshub/`; `secrets:` section).
- gws OAuth: `<scope>/auth/gws-<account>/{client_secret.json, token_cache.json, ...}`.
  Source: `operator-config.yaml` (runtime root, else `skillshub/`; `gws_accounts:` section) +
  `skillshub/client_secret.json` (the OAuth Desktop client JSON).
- Bot processes run with scoped `HOME=<scope>/workdirs/home/` so any
  `~/.config/` writes (e.g. gws) land scope-locally.

## Running and editing bots

- Edit bot code under `skillshub/bots/<bot>/src/` and re-run `make -C skillshub setup-bots`.
- Edit `bot_configs.yaml` in the active scope or through `/channels`.
- Start/stop/restart bots through the control plane at `/api/magician/v2/bots/*` or the `/channels` UI.
- On startup, Magician loads each scope's `bot_configs.yaml` and starts the bots with `enabled: true` for that scope.

## Runtime integration

- Bot lifecycle is managed by the scoped bot runtime, not by `magician-config.yaml`.
- API control plane: `/api/magician/v2/bots/*`
- The unified UI exposes the operator UI for this control plane at `/channels`.
- Telegram, Telegram self-mode, WhatsApp, Gmail, Kapso, and AgentMail are implemented today; additional channels can be added as new bot templates.

This template workspace remains separate from the Rust cargo workspace.
