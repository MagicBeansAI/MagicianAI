# `@magician/bot-sdk` — Channel Adapter Framework

**Current package version:** `0.3.6`.

**This is Magician's adapter framework for new messaging channels.** Implement
six methods on a typed `ChannelAdapter` interface and the SDK gives you
enrollment, sessions, realtime streaming, tool-call rendering, file attachments,
and resilient process supervision for free. It is the architectural equivalent
of Hermes's `BasePlatformAdapter` or Slack-style "bot framework" libraries —
the difference is that Magician runs each platform as its own scope-aware
daemon (supervised by `magic-supervisor`) rather than one monolithic gateway
process, so a misbehaving platform can't drag the others down with it.

Six reference adapters ship today: **telegram**, **telegram-self**,
**whatsapp**, **gmail**, **kapso**, **agentmail**.

## What you implement

```typescript
import type { ChannelAdapter } from "@magician/bot-sdk";

interface ChannelAdapter<TTarget = string, TContext = unknown> {
  start(handlers: ChannelAdapterHandlers<TTarget, TContext>): Promise<void>;
  stop?(): Promise<void>;
  resolveRealtimeTarget?(channelAddress: string): TTarget | null;
  prepareEnvoyTextDelivery?(target: TTarget, text: string): Promise<{
    channelAddress: string;
    send(): Promise<{ accepted: boolean }>;
  }>;
  sendText(target: TTarget, text: string, options?: ChannelSendTextOptions): Promise<void | { accepted: boolean }>;
  sendToolCallExecuted?(target: TTarget, executed: ToolCallExecutedRender): Promise<void>;
  sendTaskStatusUpdate?(target: TTarget, update: TaskStatusUpdateRender): Promise<void>;
  sendFile?(target: TTarget, file: ChannelFileAttachment): Promise<void>;
  sendCriticalAlert?(target: TTarget, alert: CriticalRequestAlertCard): Promise<{ providerMessageId?: string } | void>;
  retireCriticalAlert?(target: TTarget, sent: { providerMessageId?: string; outcome: string }): Promise<void>;
}
```

That's the entire surface. `TTarget` is the platform's native chat identifier
(Telegram `chat_id`, Discord channel ID, etc.). `TContext` is anything you
want to thread through the handlers — message metadata, user tier, locale —
and is opaque to the SDK.

## What the SDK gives you for free

| Concern | Where it lives | What it does |
|---|---|---|
| HTTP client to magician backend | `magician-client.ts` (564 LOC) | Enrollment, session lifecycle, chat send, scoped API helpers |
| Realtime subscription | `channel-runtime.ts` | Scope-aware lazy WebSocket open after enrollment; `ChatMessageReceived` event routing back to the right channel target |
| Enrollment / pairing flow | `ChannelRuntime` | `onConnectRequest` → pending-approval → enrolled → realtime open |
| Tool-call rendering | `formatters.ts` + `sendToolCallExecuted` hook | Pretty-prints tool calls with summaries and channel-appropriate formatting |
| Task-status rendering | `formatters.ts` + `sendTaskStatusUpdate` hook | Streams task progress updates |
| File attachments | `ChannelFileAttachment` type | Capability-pack `output_files` (image-gen, video-gen, deep-research PDFs) → real platform attachments; falls back to a URL message when `sendFile` is absent |
| Resilient process supervision | `resilient-process.ts` (127 LOC) | Process crash recovery, exit-code interpretation |
| Scope-awareness | Built into client | `MAGICIAN_BEARER_TOKEN` binds principal/workspace for HTTP, WebSocket, and output downloads; caller scope selectors are never sent. The runtime **injects** this at spawn (a scoped in-memory token, revoked on stop) — bots do not carry one in an env file |

## Minimal adapter skeleton

```typescript
import {
  ChannelRuntime,
  MagicianClient,
  type ChannelAdapter,
} from "@magician/bot-sdk";

class MyPlatformAdapter implements ChannelAdapter<string, MyContext> {
  async start(handlers) {
    // 1. Connect to your platform (auth, open listeners)
    // 2. For each inbound text: handlers.onTextMessage({ target, channelAddress, text, ... })
    // 3. For each inbound "/connect"-style request: handlers.onConnectRequest({ ... })
  }
  async stop() { /* close listeners */ }
  async sendText(target, text) { /* native platform send */ }
}

const runtime = new ChannelRuntime({
  channelType: "myplatform",
  adapter: new MyPlatformAdapter(),
  magician: new MagicianClient({
    baseUrl: process.env.MAGICIAN_URL!,
    bearerToken: process.env.MAGICIAN_BEARER_TOKEN!,
  }),
  enableRealtime: true,
});

await runtime.start();
```

The SDK calls `adapter.start(handlers)` once and keeps the process alive.
Inbound text messages flow `adapter → handlers.onTextMessage → ChannelRuntime
→ MagicianClient.sendMessage → magician backend → realtime event →
adapter.sendText(reply)`.

Bots that expose an agent-owned chat surface can set `controlPrefix` (default
`/magic` on the shipped Kapso and Telegram bots) plus optional
`controlPrefixes` aliases such as the legacy `@magic`. Prefixed messages are
stripped and dispatched with
`control_intent=true`; ordinary messages either dispatch with
`control_intent=false` (Kapso and Telegram, so owner texts stay on the envoy
lane) or are ignored before enrollment/session creation. Kapso also suppresses
outside-window WhatsApp template fallback for ordinary replies. The backend uses
the `source_surface` marker to keep the per-sender thread and memory trail,
strip tools/delegation for the turn, and select the model through the configured
public-chat operation mapping.

Public chat bots should also set `channelThreading: "per-address"`. With that
policy the SDK requests `ui_thread_id=ext:<channel>:<address>` for ordinary
messages and `ui_thread_id=ext:<channel>:<address>:magic` for explicit
`/magic` owner-control messages. This keeps bot conversations out of the
owner's `general` thread while preserving a stable reviewable thread per
channel sender. The option is not enabled by default so non-chat adapters that
use `ChannelRuntime` keep their current backend routing until they explicitly
opt in.

Slash commands such as `/start`, `/new`, `/stop`, `/queue`, and `/clearqueue`
are handled by the SDK even on prefix-gated public-chat surfaces, so a user can
manage the session/queue without entering the Magician work-control rail.

## Per-bot bundling and deploy

Each bot under `skillshub/bots/<name>/` is bundled with esbuild
(`build_bundle.mjs`) into a self-contained `dist/index.js`. `make
install-bot-bundles` (or `install-bot-bundles ALL=1` for every scope) symlinks
the bundle into `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/<name>/`,
and `magic-supervisor` starts/stops the daemons via scoped `bot_configs.yaml`.

**Whatsapp deviates** — its dep chain (`@ibrahimwithi/wu-cli` → `pino` →
`baileys`) is bundle-hostile, so the bundle externalises `@ibrahimwithi/wu-cli`,
which resolves at runtime from the hoisted `skillshub/node_modules/` through the
symlinked bundle.

## Process model

| | Hermes gateway | Magician Bot SDK |
|---|---|---|
| Adapter contract | `BasePlatformAdapter` Python ABC | `ChannelAdapter<TTarget, TContext>` TypeScript interface |
| Process model | One gateway process, all adapters mounted | Per-platform daemon, each uses the SDK |
| Blast radius | One bad adapter can affect others | Process isolation per platform |
| Config | One shared config + per-platform sections | Per-bot `.env.<account>` / `.env.development` + scoped `bot_configs.yaml` |
| Supervision | Internal to gateway | External — `magic-supervisor` restarts daemons |
| Cron delivery to platforms | Shared `*_HOME_CHANNEL` resolver | Each bot owns its own delivery channel |

**This is a deliberate architectural choice**, not a missing feature.
Per-platform process isolation costs slightly more config surface but gives
parallel restart, independent failure domains, and per-bot dependency
isolation (e.g. whatsapp's baileys chain doesn't pollute the telegram
runtime).

## Adapter checklist

When you add a new platform:

1. **Implement `ChannelAdapter`** with at minimum `start`, `sendText`. Add
   `sendFile` if the platform supports media attachments.
2. **Filter self-messages** — don't reply to your own bot.
3. **Filter sync / echo events** — many platforms re-deliver outbound
   messages as inbound; drop those.
4. **Redact sensitive identifiers in logs** — phone numbers, tokens, refresh
   tokens.
5. **Reconnect with exponential backoff + jitter** for streaming connections.
6. **Set `MAX_MESSAGE_LENGTH`** if the platform has size limits; chunk
   accordingly.
7. **Add a bot folder** at `skillshub/bots/<name>/` with `package.json`,
   `tsconfig.json`, `src/index.ts`, and the per-account env template.
8. **Wire it into `bot_configs.yaml`** so the runtime can find it.

## Message queueing and control commands

- **Queue during in-flight chat runs.** The backend holds a message that
  arrives while a chat turn is running in a per-session pending queue
  (`MAX_QUEUED_PER_SESSION`, `chat/service/queue_actions.rs`). The chat
  response carries `queued` (the SDK replies `Queued (#N). Type /stop to
  cancel the current run.`) and `pending_queue_depth`; `ChannelRuntime`
  drains the queue after the turn settles (`channel-runtime.ts`).
- **Control commands bypass the queue.** `/stop` cancels the current run;
  `/queue` lists queued messages; `/clearqueue`, `/queue-clear` and
  `/queue clear` clear them.
- **Queue inspector.** The unified UI reads and acts on the same queue through
  `GET /api/magician/v2/chat/sessions/{id}/queue`, per-message `…/action`, and
  `DELETE …/queue/{messageId}` (`chatStore.ts`).

## Known gaps (open work)

Remaining SDK-level additions that would benefit every existing bot.

### Other gateway-level gaps

- **Shared `allowedTargets` config schema** — every bot rolls its own
  gating today. A common schema across all adapters would mean per-bot
  allowlists configure identically.
- **`standalone_sender_fn` equivalent** — out-of-process cron delivery so
  scheduled magician tasks can send to a platform without booting the
  bot daemon.

Issues / PRs welcome on any of the above.

## Cross-references

- Bot deploy + ops: [`../README.md`](../README.md)
- Architecture topology: [`docs/ARCHITECTURE_V2.md`](../../../docs/ARCHITECTURE_V2.md)
- Consumer-channels design: [`docs/consumer_channels.md`](../../../docs/consumer_channels.md)
- Per-platform reference adapters: `telegram/`, `telegram-self/`, `whatsapp/`, `gmail/`, `kapso/`, `agentmail/`
