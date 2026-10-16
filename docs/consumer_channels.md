# Consumer Channels

## Working Assumption

We do **not** want to pay for WhatsApp Business Platform, Apple Messages for Business, or similar paid business messaging products.

That means the channel strategy should be:

1. Use **official free bot/app integrations** where they exist
2. Use **consumer-account bridges** only where there is no viable free official path
3. Default to **1:1 conversations**, not groups or broadcast channels

Telegram is the current official-bot path. Discord remains a viable future official integration, while WhatsApp and iMessage remain useful only through service-owned consumer accounts bridged into Magician. Kapso (official Meta WhatsApp Cloud API) is the production WhatsApp path (`bots/kapso/`), replacing the consumer-bridge approach for verified business accounts.

All CLI-based bots (WhatsApp/wu-cli, Telegram-self/tgcli, Gmail/gws) share a `ResilientProcess` SDK utility (`skillshub/bots/sdk/src/resilient-process.ts`) for retry/backoff/timeout logic: exponential backoff (3s→60s), credential purging after 2 failures, and automatic give-up after 10 minutes of continuous failures (restart manually from bots page). WhatsApp binary resolution uses local-then-workspace fallback for npm workspace compatibility.

Bot commands handled by the SDK (`channel-runtime.ts`): `/start` triggers the connect/welcome flow, `/new` archives the current chat session and starts fresh. Both work uniformly across all channels (Telegram bot, Telegram-self, WhatsApp, Gmail).

---

## Relationship to Chat Mode

Chat Mode (Phases 1-3, implemented) provides the **shared backend** that all consumer channels use:

```
All Clients (web, Telegram, Discord, etc.)
    |
    v
POST /enroll → resolve identity → get principal
    |
    v
Chat HTTP API (same endpoints for everyone)
    Authorization: Bearer <workspace-bound-token>
    GET  /chat/active?channel={c}&channel_address={a}
    POST /chat/sessions/{id}/messages
    |
    v
ChatService (shared backend)
    process_message(), agentic chat-inline runtime, task status, memory, persona
```

**Every client follows the same path.** Web UI, Telegram bot, Discord bot -- all are clients of the same HTTP API. All must enroll first. All get a principal back. All use that principal for chat. No special-casing.

The only difference between clients is **how they render the response** (HTML for web, Telegram messages, embeds for Discord, plain text for bridges).

### What Already Exists (from Chat Mode)

| Component | Used by channels? |
|---|---|
| `ChatService::process_message()` | Yes -- the core message handler |
| `ChatStore` / `FileChatStore` | Yes -- one active thread per principal, same for all channels |
| `ChatSession.origin_channel: ChatChannel` | Yes -- records which channel started the session |
| `ChatChannel` struct (`{ channel_type: String, address: Option<String> }`) | Yes -- open-ended, any channel type accepted. Passed to `get_or_create_active_session()` via API query params `?channel=telegram&channel_address=987654321` |
| `TaskWatcher` | Yes -- status updates injected into session |
| Agent persona + memory episodes | Yes -- same agent identity across all channels |

### What Gets Added

| Component | Where | Purpose |
|---|---|---|
| Enrollment API | Magician core | `POST /enroll`, `POST /enroll/approve`, `GET /enroll/status` -- single identity gateway for ALL clients |
| Enrollment store | Magician core | Maps `(channel_type, address, workspace) → principal`. Scoped JSON files under `magician_data_v3/scopes/<principal>/<workspace>/chat/enrollments.json`. |
| Web enrollment | Frontend | On first visit, frontend calls `POST /enroll { channel_type: "web", channel_address: <session_uuid> }`, stores principal in localStorage |
| Per-channel bot | Separate process | Platform-specific event loop + response formatting (Telegram, Discord, etc.) |

---

## Product Rule

Do **not** model the problem as "send via the user's own number/account" for a central service.

For Magician as a shared service, the workable model is:

- Magician owns a channel identity (bot account, service number)
- The user messages that identity
- Magician replies in the same 1:1 thread

For WhatsApp and iMessage, the user should message a **dedicated service identity**, not their own number.

---

## Channel Priority

| Channel | Mode | Rich buttons | Can service initiate | Needs always-on host | Risk | Priority |
|---------|------|:------------:|:--------------------:|:--------------------:|------|----------|
| Telegram | Official bot | Yes | After `/start` | No | Low | Highest |
| Discord | Official bot | Yes | Yes | No | Low | Deferred |
| Slack | Official app | Yes | Yes | No | Low | Medium |
| WhatsApp | Consumer bridge | No | Limited | Yes | High | Medium-Low |
| iMessage | Consumer bridge | No | Yes | Yes | High | Low |

---

## Architecture

### Enrollment API

**All clients** -- web UI, Telegram bot, Discord bot, any future client -- call the enrollment API before chatting. No client manages identity itself. Magician handles the policy.

**Endpoints on magician (new):**

```
POST /api/magician/v2/chat/enroll
{
    "channel_type": "telegram",
    "channel_address": "987654321",
    "display_name": "Alex"           // optional
}

→ Enrolled:
{ "principal": "default", "enrolled": true }

→ Pending approval:
{ "principal": null, "enrolled": false, "code": "ABC123" }


POST /api/magician/v2/chat/enroll/approve
{
    "code": "ABC123",
    "principal": "owner"
}

→ { "enrolled": true, "channel_type": "telegram", "channel_address": "987654321", "principal": "owner" }


GET /api/magician/v2/chat/enroll/status?channel_type=telegram&channel_address=987654321

→ { "enrolled": true, "principal": "owner" }
→ { "enrolled": false, "code": "ABC123" }
→ { "enrolled": false, "code": null }   // unknown
```

**Storage:** scoped JSON file `magician_data_v3/scopes/<principal>/<workspace>/chat/enrollments.json`:
```json
{
    "enrollments": {
        "web:a1b2c3d4-uuid:project-alpha": { "principal": "default", "workspace": "project-alpha" },
        "telegram:987654321:project-alpha": { "principal": "default", "workspace": "project-alpha", "display_name": "Alex" },
        "discord:456789012:project-alpha": { "principal": "default", "workspace": "project-alpha", "display_name": "Alex" }
    },
    "pending": {
        "ABC123": { "channel_type": "telegram", "channel_address": "987654321", "workspace": "project-alpha", "display_name": "Alex", "created_at": 1710000000 }
    }
}
```

**Tenant policy (configurable):**

- **Auto-approve mode:** `POST /enroll` auto-approves into the configured `default_principal` in the bearer-bound workspace. Bots get the principal immediately.
- **Approval-gated mode:** `POST /enroll` returns `enrolled: false` with a pending code. An admin approves via `POST /enroll/approve` from CLI, script, or API. Pending enrollments expire after 24h.

The policy is a config flag, not a code change:
```yaml
# magician-config.yaml
enrollment:
  auto_approve: true              # auto-approve into default_principal in the bearer workspace
  default_principal: "default"    # principal assigned on auto-approve
  pending_ttl_hours: 24           # how long pending enrollments last
```

**Every client follows the same flow** regardless of tenant mode:

```
Web UI:
1. On first visit, generate a session UUID, store in localStorage
2. Call POST /enroll { channel_type: "web", channel_address: <uuid> } with the workspace-bound bearer
3. Auto-approved → store principal in localStorage
4. Use principal for all chat API calls
5. On subsequent visits, call GET /enroll/status to verify still enrolled

Telegram/Discord/etc:
1. User sends /start (or first message)
2. Bot calls POST /enroll { channel_type: "telegram", channel_address: <chat_id>, display_name } with its workspace-bound bearer
3. If enrolled → "Connected! You're chatting as {principal}"
4. If pending → "Pending approval. An admin will connect you shortly."
   Bot polls GET /enroll/status periodically
5. When approved → bot gets principal, starts routing messages
```

The enrollment call is idempotent -- calling it again for an already-enrolled identity returns the existing principal.

When a client includes `channel` + `channel_address` on chat API calls, magician should treat enrollment as authoritative:

- Resolve the principal from the enrollment store
- Reject requests where the supplied principal disagrees with the enrolled principal
- Reject requests for unenrolled channel identities

For web, the frontend should also re-check `GET /enroll/status` before trusting a cached principal from `localStorage`.

### How Clients Use the Chat API

Every client -- web or bot -- calls the same HTTP API:

```
1. POST /enroll { channel_type, channel_address } → get principal
   (once, cached)

2. GET /chat/active?channel={c}&channel_address={a}
   → get or create active session

3. POST /chat/sessions/{id}/messages { text: "user message" }
   → ChatService processes, returns ChatResponse

4. Client formats returned messages for its platform:
   - Web: renders Svelte components
   - Telegram: `sendMessage` text chunks; runtime actions stay owned by Magician, not Telegram callback confirmations
   - Discord: embeds + button components
   - Bridges: plain text

5. For push events (task status):
   - Web: already connected via WebSocket
   - Bots: connect to ws://localhost:3002/api/magician/v2/realtime/ws with
     `magician-events-v2` followed by `magician-bearer.<token>` as the offered
     WebSocket subprotocols; the server selects only `magician-events-v2`
```

Every HTTP request above carries `Authorization: Bearer <token>`. Principal and
workspace are claims of that token; clients never send them in `X-*` headers,
request bodies, or query parameters. A bot learns its workspace by asking:
the SDK's `ChannelRuntime.start()` calls `GET /auth/session` and reads
`workspace` off the answer (for a runtime-minted bot token that answer names
the bot and carries no identity — see
[auth.md → Bot tokens](components/magician/auth.md#bot-tokens)). A 401 there
is a bot that never comes up. Enrollment responses may expose the
resolved principal as display metadata, but they do not create client-selected
scope authority.

### Tool-Use Contract

Action semantics are owned by the agentic runtime, not by channel clients.

- `POST /messages` is the turn-processing endpoint for chat sends
- the personal agent chooses capabilities through the normal runtime catalog
- streaming vs non-streaming is a transport difference only
- channel adapters render the messages they receive and must not reconstruct tool semantics client-side

No adapter struct in magician core. Each client handles its own formatting and delivery.

**Important:** the `channel` + `channel_address` query params are not just metadata. They are the identity binding for channel-aware clients, and should be sent on all chat API requests (`/chat/active`, `/chat/new`, `/chat/sessions`, session detail, and send message).

### Status Push

For push events (task status updates), the bot connects to magician's WebSocket:

```
ws://localhost:3002/api/magician/v2/realtime/ws
```

Filters for `ChatMessageReceived` events. Delivery is chat-session based, not a
separate bot-specific progress channel:
- Server-side progress subscriptions append `TaskStatusUpdate` chat messages to the scoped chat session
- Realtime `ChatMessageReceived` now includes the session `origin_channel`
- The bot runtime first uses its local `session_id -> target` binding, then falls back to `origin_channel.address` so delivery survives bot restarts

When a `TaskStatusUpdate` arrives, the bot formats and sends it via the platform API.

This is the same WebSocket the web dashboard uses. The bot is just another client.

### Response Formatting (Per Channel)

Each channel bot formats chat messages produced by `ChatResponse` for its platform:

| ChatMessageContent | Telegram | Discord | WhatsApp/iMessage |
|---|---|---|---|
| `Text(s)` | `sendMessage` | Plain text | Plain text |
| `ToolCallExecuted` | "{summary}" | Embed with neutral accent | Plain text |
| `TaskStatusUpdate` | "📋 Task {status}: {summary}" | Embed with status color | Plain text |

No shared `OutboundMessage` type -- each channel knows its own format.

---

## Channel Details

### Telegram (Build First)

**Mode:** Official bot. **Priority:** Highest.

**Why first:** Free, official, native buttons, no local device bridge needed. Best overall option.

**Setup:**
1. Create bot via BotFather, get token
2. Configure bot with magician URL (`MAGICIAN_URL=http://localhost:3002`)
3. Run bot process (long-polling or webhook mode)

**User flow:**
1. User sends `/start` to `@presto_bot`
2. Bot calls `POST /enroll` → auto-approved or pending, depending on enrollment policy
3. If approved: "Connected! Chat away."
4. User sends messages → bot calls `POST /chat/sessions/{id}/messages` → formats response → sends back

**Implementation status:** Implemented as `bots/telegram/` in the npm workspace.

**Implementation:**
- TypeScript/Node.js package using `telegraf`
- Long-polling bot process launched as a standalone Node process
- Private chats only; group chats are not part of the consumer-channel path
- `/start` performs enrollment and returns a local connect message without sending `/start` into chat history
- Calls magician HTTP API for enrollment, session management, and message processing
- Connects to magician WebSocket for push events (task status updates), with restart-safe routing via `origin_channel`
- Formats `ChatResponse` and realtime chat messages as Telegram messages
- Runtime actions are owned by Magician; Telegram does not implement a separate tool-confirmation path

### Discord (Deferred / Future)

**Mode:** Official bot. **Status:** Deferred to future expansion.

**Why keep it in the design:** Free, official, buttons and slash commands, easier onboarding for technical users when we expand beyond Telegram + WhatsApp.

**Setup options:**
1. User DMs the bot directly
2. User invokes a slash command in a server, then Magician moves follow-up to DM

**Where does the user send messages?** Prefer the bot DM. A server command is fine for starting a task, but conversation should continue in DM.

**Where does Magician reply?** To the DM channel with that user.

**If revisited later:** Discord gateway WebSocket connection. Rich embeds for task status and runtime action prompts. Extract a shared trait from Telegram + Discord if common patterns emerge.

### Slack (Optional)

**Mode:** Official workspace app. **Priority:** Medium.

**Where does the user send messages?** To the app DM or via slash command in Slack.

**Where does Magician reply?** To the DM channel with that user.

**Why lower priority:** Useful mostly when the user already lives in a Slack workspace. Less relevant for broad consumer/mobile reach.

**Implementation:** Block Kit for rich interactions. Workspace install flow.

### WhatsApp (Bridge)

**Mode:** Consumer-account bridge. **Priority:** Medium-Low.

**Setup:**
1. Create a dedicated WhatsApp account for the service
2. Link it once to a bridge process via QR code
3. Keep the bridge session alive

**Where does the user send messages?** To the service's dedicated WhatsApp number in a normal 1:1 chat.

**Where does Magician reply?** From that same service WhatsApp account back into the same 1:1 chat.

**What not to do:**
- Do not build around the user's own WhatsApp account for a central hosted service
- Do not expect users to message a shared group

**Operational caveats:**
- Linked-device automation is more brittle than an official bot/API
- Message formatting is limited (no rich buttons -- numbered plain text for options)
- Account/session management becomes a real operational concern
- Operationally fragile, unofficial

**Implementation status:** Implemented as `bots/whatsapp/` in the npm workspace.

**Implementation:**
- TypeScript/Node.js package using `baileys`
- Dedicated 1:1 chats only; group and broadcast messages are ignored
- The local bridge keeps the account's self-chat ("Note to self") as ordinary WhatsApp history, so self-messages do not enroll, open a session, or start the Magician chat flow. Kapso is the WhatsApp control surface: only messages to Kapso with the configured control prefix (default `@magic`) enter Magician control.
- The adapter matches both the phone JID and WhatsApp LID aliases for the self-chat, and deliberately does not treat `fromMe` alone as routable because every outbound message to any contact has that flag.
- QR code auth writes `qr.png` for the bridge account and persists session state via `useMultiFileAuthState`
- Runtime actions are owned by Magician; WhatsApp does not implement a local confirmation command path
- `read` subcommand returns cached local message history for later capability-side polling

### iMessage (Bridge)

**Mode:** Consumer-account bridge on a Mac host. **Priority:** Low.

**Setup:**
1. Create a dedicated Apple ID, email, or phone-backed Messages identity for the service
2. Sign into `Messages.app` on a Mac mini or always-on Mac
3. Send via AppleScript and read inbound messages from `chat.db`

**Where does the user send messages?** To the service's iMessage identity in a direct conversation.

**Where does Magician reply?** From that same signed-in Messages account, back into the same thread.

**What not to do:**
- Do not expect a free hosted iMessage business inbox
- Do not try to centralize around each user's own iMessage account

**Operational caveats:**
- Requires a Mac host with Full Disk Access
- Local automation is less stable than Telegram or Discord bots
- Platform behavior can change across macOS releases

---

## Module Structure

### In Magician Core (enrollment + bot control plane)

```text
magician/src/magician_v2/
    api/enrollment_api.rs    -- POST /enroll, POST /enroll/approve, GET /enroll/status
    api/bot_api.rs           -- GET /bots, start/stop/restart/logs
    bots/mod.rs              -- generic child-process supervisor for configured bots
    chat/enrollment.rs       -- EnrollmentStore (single JSON file)
```

### Bot SDK + Channel Bots (npm workspace, outside Rust cargo workspace)

All bots are TypeScript/Node.js. A shared SDK/runtime handles the Magician side once: enrollment, session lookup, message sending, websocket events, and the common channel loop. Each bot stays thin and only implements the platform adapter.

```text
bots/
├── package.json              # npm workspace root
├── tsconfig.base.json
├── sdk/
│   ├── package.json          # @magician/bot-sdk
│   ├── README.md
│   ├── tsconfig.json
│   ├── test/
│   │   ├── channel-runtime.test.mjs
│   │   └── magician-client.test.mjs
│   └── src/
│       ├── index.ts              # package exports
│       ├── types.ts              # Magician chat types + adapter contract
│       ├── magician-client.ts    # HTTP + realtime WebSocket client
│       ├── channel-runtime.ts    # shared enroll → session → message flow
│       └── formatters.ts         # text fallbacks for non-rich channels
│
├── telegram/
│   ├── package.json          # depends on @magician/bot-sdk + telegraf
│   ├── README.md
│   ├── tsconfig.json
│   ├── test/
│   │   └── callback-data.test.mjs
│   └── src/
│       ├── index.ts          # bootstraps Telegram runtime
│       ├── adapter.ts        # Telegram private-chat adapter
│       └── callback-data.ts  # callback encoding + text chunking helpers
│
├── whatsapp/
│   ├── package.json          # depends on @magician/bot-sdk + baileys + qrcode
│   ├── README.md
│   ├── tsconfig.json
│   ├── test/
│   │   └── helpers.test.mjs
│   └── src/
│       ├── index.ts              # bootstraps WhatsApp runtime + read command
│       ├── adapter.ts            # Baileys bridge adapter
│       ├── helpers.ts            # text extraction, address normalization, CLI parsing
│       └── message-store.ts      # local JSONL cache for later reads
│
├── agentmail/                # inbound email webhook receiver (phase 1: receive + log)
│   ├── package.json          # depends on agentmail (JS SDK) + express + svix
│   ├── tsconfig.json
│   ├── .env.example
│   └── src/
│       └── index.ts          # POST/GET /agentmail-webhook, svix verify, self-register, log-only
│
└── <channel>/
    ├── package.json
    └── src/
        ├── index.ts
        └── adapter.ts
```

**Why all TypeScript:**
- WhatsApp (Baileys) requires Node.js -- no Rust alternative
- Telegram (`telegraf`), Discord (`discord.js`), Slack (`@slack/bolt`) all have excellent Node.js SDKs
- One language, one SDK, one ecosystem for all bots
- Fastest to ship a batch of bots together

**Completely decoupled from Magician core:**
- Separate npm workspace, not in the Rust cargo workspace
- Only dependency on magician is the HTTP API (no shared Rust types)
- Each bot is a standalone process: `cd bots/telegram && npm start`

**AgentMail inbound bot (`bots/agentmail/`).** The webhook receiver forwards
verified `message.received` events through `@magician/bot-sdk` and sends threaded
replies. It always requires a valid Svix signature before recording reply
context or trusting the sender's authentication labels. Set
`AGENTMAIL_WEBHOOK_SECRET` for a manually registered webhook, or set
`AGENTMAIL_WEBHOOK_URL` to self-register and obtain the signing secret from the
provider (idempotent via `client_id`). Explicit secret configuration takes
precedence. Events are refused while registration is pending; if no secret is
available afterward, startup fails and closes the receiver. The secret is never
logged. The bot uses the usual registration (`bot_configs.yaml`, `LAUNCHABLE_BOTS`,
`write_agentmail` in `setup_bot_envs.py`). A public URL (ngrok / magictunnel →
`WEBHOOK_PORT`, default `3011`) is required for AgentMail to deliver events.

### Bot SDK (`@magician/bot-sdk`)

Gmail and AgentMail use one strict single-mailbox parser for sender attribution;
Gmail uses it for prepared reply recipients too. An address inside a quoted
display name never supplies the sender identity. Multiple senders, malformed
headers and unsupported forms (comments, quoted local parts and address
literals) are refused instead of choosing an address. Supported dot-atom
mailboxes are normalized to lowercase. This parsing establishes an address,
not SMTP sender authentication; the server's routing policy remains in force.

The SDK wraps Magician's API contract and exposes a shared runtime for channel implementers:

```typescript
class MagicianClient {
    constructor({ baseUrl })  // e.g., { baseUrl: 'http://localhost:3002' }

    async enroll({ channelType, channelAddress, displayName }): Promise<EnrollResult>
    async getEnrollmentStatus({ channelType, channelAddress }): Promise<EnrollmentStatus>
    async getActiveSession(identity): Promise<ChatSessionDetail>
    async newSession(identity): Promise<ChatSession>
    async listSessions(identity): Promise<ChatSession[]>
    async getSession(sessionId, identity): Promise<ChatSessionDetail>
    async sendMessage(sessionId, identity, text): Promise<ChatResponse>
    connectRealtime(onEvent): MagicianRealtimeConnection
}

interface ChannelAdapter {
    start(handlers): Promise<void>
    sendText(target, text): Promise<void>
    sendToolCallExecuted?(target, executed): Promise<void>
    sendTaskStatusUpdate?(target, update): Promise<void>
}

class ChannelRuntime {
    constructor({ channelType, adapter, magician, enableRealtime? })
    async start(): Promise<void>
    async stop(): Promise<void>
}
```

Each bot uses it like:

```typescript
// bots/telegram/src/index.ts
import { ChannelRuntime, MagicianClient } from '@magician/bot-sdk'
import { Telegraf } from 'telegraf'

const bot = new Telegraf(process.env.TELEGRAM_TOKEN!)
const magician = new MagicianClient({
    baseUrl: process.env.MAGICIAN_URL || 'http://localhost:3002'
})

const runtime = new ChannelRuntime({
    channelType: 'telegram',
    adapter: new TelegramAdapter(bot),
    magician,
    enableRealtime: true
})

await runtime.start()
```

The platform adapter is intentionally the only per-channel glue. It knows how to receive/send on Telegram, Discord, WhatsApp, etc. The shared SDK knows how to talk to Magician.

~20 lines of bot-specific code. The SDK handles the rest.

### Tech Stack Per Channel

Each channel uses **two tools**: a send-only CLI for outbound capabilities, and a persistent library for the inbound bot. The CLI tools cannot receive messages -- receiving requires a persistent connection to the platform.

#### Tool Capabilities (Researched)

**Mudslide** (WhatsApp CLI, npm) -- **send-only**:
- Send: text, image, file, location, poll ✅
- Group: list groups, list members, add/remove participants ✅
- Read messages: **No** ❌
- Receive messages: **No** ❌
- List chats: **No** ❌
- Auth: QR code via `mudslide login`

**telegram-send** (Telegram CLI, pip) -- **send-only**:
- Send: text, image, file, video, audio, sticker, animation, location ✅
- Formatting: Markdown, HTML, monospace ✅
- Stdin pipe: `echo 'msg' | telegram-send --stdin` ✅
- Read messages: **No** ❌
- Receive messages: **No** ❌
- Auth: bot token via `telegram-send --configure`

**Baileys** (WhatsApp library, npm) -- **full bidirectional**:
- Send: text, image, video, audio, document, sticker, location, reactions ✅
- Receive: **real-time** via WebSocket push (`messages.upsert` event) ✅
- Message edits/deletes: `messages.update`, `messages.delete` events ✅
- Read receipts: `message-receipt.update` event ✅
- Presence/typing: presence events ✅
- Chat list: `chats.upsert`, `chats.update` events ✅
- Contact list: `contacts.upsert` events ✅
- Group management: create, modify, participants ✅
- Call events: accept/decline/offer ✅
- History sync: loads old messages on connect ✅
- Auth: QR code, persistent session via `useMultiFileAuthState`

**Telegram Bot API** (via telegraf, npm) -- **full bidirectional**:
- Send: text, media, files, inline keyboards, embeds ✅
- Receive: **real-time** via long-polling (`getUpdates`) or webhook ✅
- Callback queries (button clicks) ✅
- Slash commands ✅
- Edited messages, channel posts ✅
- Group management ✅
- Auth: bot token from BotFather

#### How They're Used

| | Outbound (execution capability) | Inbound (chat channel bot) |
|---|---|---|
| **WhatsApp** | `mudslide send` (CLI, one-shot) | Baileys `messages.upsert` (persistent WebSocket) |
| **Telegram** | `telegram-send` (CLI, one-shot) | Telegraf/Bot API `getUpdates` (persistent long-poll) |

**Outbound CLI tools** are invoked by the agent via shell capability (YAML packs). They fire and forget -- no awareness of replies.

**Inbound bot libraries** maintain persistent connections and receive ALL messages in real-time. No polling of specific chats needed -- the platform pushes everything.

**When the agent needs to know about replies** (e.g., "did +91... respond?"), the bot must be running. The bot caches recent messages locally. A `read` CLI command queries this cache:

```
# Agent sends via capability (one-shot):
mudslide send +919876543210 'Is the invoice ready?'

# Later, agent checks for reply via bot's read command:
node bots/whatsapp/dist/index.js read +919876543210 --since 5m
→ reads from bot's local message cache
→ returns: [{ channelAddress: "919876543210@s.whatsapp.net", direction: "inbound", kind: "text", text: "Yes, attached", timestamp: ... }]
```

The `read` command is part of our bot wrapper (not mudslide). It queries the local message store written by the bridge.

| Channel | Platform SDK | Why |
|---|---|---|
| Telegram | `telegraf` | Most popular Node.js Telegram bot framework |
| Discord | `discord.js` | Standard Discord bot library |
| WhatsApp | `baileys` | Only viable free WhatsApp Web bridge |
| Slack | `@slack/bolt` | Official Slack SDK |
| iMessage | Custom (AppleScript + SQLite) | No SDK exists, requires Mac host |

---

## Critical-request alerts (secure HITL P5)

A bot built on `@magician/bot-sdk` ≥ 0.3.6 also delivers *critical-request
alerts*: when Magician needs a password, a verification code or a
time-bound decision, the runtime's delivery coordinator offers a value-free
card on the bot's realtime feed (`CriticalRequestAlert`, no address), the
bot claims it over the authenticated API (the claim returns the owner's
address for exactly that delivery — owner identities from
`envoy.owner_identities`, never the last inbound sender), sends the card
(Telegram: inline **Open secure request** button; Kapso: interactive CTA-URL
inside the 24-hour window, the fallback template outside it) and reports the
provider's answer. When the request resolves the bot edits (Telegram) or
annotates (Kapso) the card. The owner enables channels and their order under
Settings → Critical alerts. The P3 rule stands: a bot never solicits the
value and refuses a reply typed into the channel while a secret ask is open.
Contract: `docs/components/magician/critical-request-delivery.md`.

A registered bot or a generic message reader is *not* a verification-code
source (secure HITL P6): only an Observe account the owner granted **Use for
verification codes**, the local Messages store, or a permitted Android phone
is read for a live challenge — see
`docs/components/magician/verification-code-retrieval.md`.

## Bot Lifecycle Management

Backend control plane status:

- **Done now:** backend config parsing, child-process supervision, startup/shutdown wiring, `/api/magician/v2/bots` control endpoints, `/api/magician/v2/bots/{name}/qr`, and the `/channels` management UI
- **Still pending:** additional channel implementations

Magician manages bot processes directly inside the active scope. When magician starts, configured scoped bots start. When magician stops, bots stop. No external process manager is needed.

### Process Management

Magician spawns bot processes via `tokio::process::Command` (same pattern as shell actions). Each bot is a Node.js process:

```rust
// In scoped bot startup:
let bot_process = Command::new("node")
    .arg("{scope_capabilities_root}/bots/telegram/dist/index.js")
    .env("MAGICIAN_URL", "http://localhost:3002")
    .env("TELEGRAM_TOKEN", token)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
```

**Lifecycle:**
- On magician start → spawn configured bots
- On magician shutdown → send SIGTERM to all bot processes, wait 5s, SIGKILL
- On bot crash → log error, auto-restart with backoff (1s, 2s, 4s, max 30s)
- Stdout/stderr captured and streamed to magician logs. Bot/sdk producers are responsible for using the right lane: ordinary lifecycle/reconnect/auth-guidance notices should stay on stdout/info, while genuinely failure-like conditions belong on stderr/error. The current Gmail and Telegram-self adapters also classify child-process stderr at the source so lines like watch/listening guidance do not surface as bot-page errors.

### Bot Configuration

Bot process config is scoped runtime state, not global config. The canonical source lives at `skillshub/bots/bot_configs.yaml`; `make -C skillshub seed-scope` (run as part of `setup-all`) seeds it into `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/bot_configs.yaml` on first install per scope. The scope copy is the live editable file — operators can edit it directly, or via the `/channels` UI, or via `PUT /api/magician/v2/bots/{name}/config`. The scoped `bots:` map is optional; if omitted, magician exposes an empty bot list for that scope and starts nothing. (Pre-refactor the file lived under `<scope>/capabilities/bot_configs.yaml`; the `capabilities/` umbrella was collapsed in the AgentSkills migration so bots, auth, workdirs, and skills are all first-class siblings under the scope root now.)

```yaml
# $MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/bots/bot_configs.yaml
bots:
  telegram:
    enabled: true
    command: "node"
    args:
      - "--env-file={scope_capabilities_root}/bots/telegram/.env.development"
      - "{scope_capabilities_root}/bots/telegram/dist/index.js"
    env: {}
    cwd: "{scope_capabilities_root}/bots/telegram"
    auto_restart: true
    restart_max_backoff_secs: 30

  whatsapp:
    enabled: false
    command: "node"
    args:
      - "--env-file={scope_capabilities_root}/bots/whatsapp/.env.development"
      - "{scope_capabilities_root}/bots/whatsapp/dist/index.js"
    env:
      WU_HOME: "{scope_capability_workdir_root}/home/.wu"
    cwd: "{scope_capabilities_root}/bots/whatsapp"
    auto_restart: true
    restart_max_backoff_secs: 30
```

Notes on the variable substitutions:
- `{scope_capabilities_root}` — kept for callsite stability; substitutes to the scope root (`magician_data_v3/scopes/<principal>/<workspace>/`) post-umbrella-collapse, NOT a `capabilities/` subdir.
- `{scope_capability_workdir_root}` — substitutes to `<scope>/workdirs/`.
- Per-bot secrets live in adjacent `.env.<account>` (gmail multi-account) or `.env.development` (single-instance bots), populated by `setup-bot-envs`. Bot env files are not committed; only the `bot_configs.yaml` shape is.
- `whatsapp` deviates from the single-bundle model — its bundle externalizes `@ibrahimwithi/wu-cli` and `install-bot-bundles` runs `npm install --omit=dev` once at `<scope>/bots/whatsapp/` to materialize the externalized dep. See `skillshub/bots/whatsapp/README.md` for why.

Environment variables support `${ENV_VAR}` substitution from the host environment. `enabled: false` means the bot is configured but not started.

### Settings UI

Presto now includes a **Bot Control** page at `/channels` backed directly by the live bot control plane. It is intentionally operational, not decorative:

- Lists configured bots with live runtime state
- Starts, stops, and restarts configured bot processes
- Shows captured stdout/stderr logs
- Shows QR pairing state for bots that expose a QR artifact
- Shows managed auth state for Gmail/GWS bots, including expected vs current account
- Starts a single active Gmail re-auth flow at a time so multiple Google tabs do not race
- Creates, edits, and deletes bot definitions in the real scoped `bot_configs.yaml`

The current page edits the real `bots:` map inside `magician_data_v3/scopes/<principal>/<workspace>/bots/bot_configs.yaml`. Saving a bot updates the stored scoped config and syncs the live bot manager immediately; if the bot was already running, the new config is applied right away.

`magician-config.yaml` no longer accepts a top-level `bots:` block. If a custom config still contains one, startup now fails with a validation error instead of silently ignoring it.

The page presents each configured bot as a control card plus a real editor modal:

| Bot | Status | Channel | Actions |
|-----|--------|---------|---------|
| Telegram | Running (uptime: 2h 15m) | Telegram | [Restart] [Stop] [Logs] [Edit config] |
| WhatsApp | Stopped | WhatsApp | [Start] [Restart] [Logs] [Edit config] + inline QR when available |
| New bot | Not yet started | Derived from config | [Create config] |

**API endpoints (new):**
```
GET  /api/magician/v2/bots                → list configured bots with status
GET  /api/magician/v2/bots/auth/state     → active/queued managed auth flow state
POST /api/magician/v2/bots/{name}/start   → start a bot
POST /api/magician/v2/bots/{name}/stop    → stop a bot
POST /api/magician/v2/bots/{name}/restart → restart a bot
GET  /api/magician/v2/bots/{name}/logs    → recent stdout/stderr (last 100 lines) with severity inferred from the producer's stream choice (`stdout`, `stderr`, `supervisor`, `auth`)
GET  /api/magician/v2/bots/{name}/qr      → current QR image when the bot exposes one
GET  /api/magician/v2/bots/{name}/auth    → provider-specific managed auth status
POST /api/magician/v2/bots/{name}/auth/start → enqueue/start managed re-auth for that bot
GET  /api/magician/v2/bots/{name}/config  → current editable bot definition
PUT  /api/magician/v2/bots/{name}/config  → create/update a bot definition and sync runtime
DELETE /api/magician/v2/bots/{name}/config → remove a bot definition and stop it
```

### WhatsApp QR Code Flow

For Baileys (WhatsApp), the first run requires QR code scanning:

1. Operator starts the configured WhatsApp bot from `/channels` (or it starts on boot if `enabled: true`)
2. Bot generates QR code → writes to the scoped QR artifact path, typically `magician_data_v3/scopes/<principal>/<workspace>/capabilities/bots/whatsapp/qr.png`, and emits via stdout
3. `/channels` requests `/api/magician/v2/bots/{name}/qr` → shows it inline for pairing
4. User scans → Baileys authenticates → QR file deleted → status changes to "Running"
5. On subsequent restarts, Baileys auto-reconnects from saved auth state

If session expires, bot emits a new QR code and settings UI shows it again.

---

## Build Order

1. **Enrollment API + store** in magician core -- done (3 endpoints + JSON file) ✅
2. **Enrollment config + identity-bound chat usage** in web/chat foundation -- done ✅
3. **Bot SDK/runtime foundation** (shipped under the V3 bot template roots) -- done ✅
4. **Bot lifecycle backend control plane** (scoped config + supervisor + `/api/magician/v2/bots`) -- done ✅
5. **Telegram bot template + scoped runtime** -- done ✅
6. **WhatsApp bot template + scoped runtime** -- done ✅
7. **Settings UI** (`/channels`) -- done ✅
8. Later: Discord, Slack, iMessage

---

## Key Decision

The architecture optimizes for:

- **ChatService as the single backend** -- all channels converge on the same HTTP API
- **Shared TypeScript SDK/runtime** -- one `@magician/bot-sdk` package used by all bots, handles enrollment + chat + push while channel implementers supply thin adapters
- **Decoupled npm workspace** -- `bots/` is independent of the Rust cargo workspace
- **Enrollment API handles identity** -- bots don't manage principals, magician does. Single-tenant auto-approves, multi-tenant adds approval step. Same bot code for both.
- **Official free bots first** -- cheapest path aligned with cleanest implementation

## Envoy Claims Review

Envoy text replies use a paired backend/bot receipt protocol before dispatch. Adapters return explicit acceptance only for the exact text; suppressed or partial sends stay unknown. Receipt retries do not resend messages. See [Claims Review](components/unified-ui/claims-review.md).

After the immediate receipt retries fail, the running SDK retains the phase
and original binding, retries every 30 seconds and on replay, and never invokes
the adapter again for that pending receipt. The recovery queue reserves a slot
before a tracked send and stops new sends at 2,048 outstanding receipts rather
than discarding proof. Shutdown stops the retry timer; pending receipt recovery
is in memory, so a restart can leave the durable server record unknown.
Each Envoy Begin/receipt HTTP attempt has a 30-second deadline covering both
response headers and body. A stalled response releases the active attempt so
the existing bounded retries and receipt recovery can continue with the same
binding. This deadline does not shorten model-backed chat requests.

Tracked AgentMail replies set the provider SDK's retry count to zero: a retryable
HTTP error can follow acceptance, so another POST could duplicate the mail.
Acceptance requires a returned provider message ID. A separate 30-second abort
signal covers AgentMail response-body reading as well as headers: the provider
SDK clears its own timeout before reading the body. Stalled success/error bodies
leave the send unknown and cannot cause another email POST.

Tracked Telegram replies use a 30-second deadline per chunk and require a
positive message ID in the intended chat. An empty SDK success or a receipt for
another chat stops remaining chunks and stays unknown. A transport failure or
deadline also stops the reply; replay never repeats the external send.

Tracked Kapso replies never fall back to a template, whose wording differs from
the prepared body; rejection
outside the messaging window leaves the attempt unconfirmed, including after a
partial chunk send. Ordinary Kapso replies retain their configured fallback.
Kapso requires a nonempty provider message ID for every tracked text chunk;
an HTTP success with a missing or malformed receipt stops the remaining chunks
and reports an unknown outcome. Its provider requests have a 30-second deadline
covering response headers and body reading. A timeout does not retry the send
or trigger a template fallback. Provider-free regressions exercise the installed
Kapso SDK and channel runtime together, including partial replies and replay.

Tracked WhatsApp sends wait up to 30 seconds per chunk for the matching server
acknowledgement on the same socket. A successful socket write alone is not
acceptance. Negative acknowledgements, disconnects and timeouts stop further
chunks and leave the reply unknown; the adapter never retries the send. Fast
acknowledgements received before the send call returns are retained and matched
to its message ID. Ordinary WhatsApp replies keep their existing send behavior.
The acknowledgement deadline also bounds a send promise that has not settled.
Timeout or socket closure releases that wait and removes its listeners; an
in-flight write may finish later, but it cannot change the returned unknown
outcome, start another chunk, or authorize a resend. Late send failures are
observed without unhandled promise rejections.

The SDK binds Begin and receipts to a random attempt UUID, the adapter's recipient
and a SHA-256 digest of the canonical text. It can retry a lost Begin response
before adapter entry, but consumes that identity before any send. Transport
adapters with opaque targets can implement `prepareEnvoyTextDelivery`: read-only
preparation returns the resolved address and a send closure that captures those
exact words and recipient.

Gmail uses the incoming **message** ID (not thread ID), resolves recipient metadata,
and sends tracked replies with an explicit MIME body through
[`messages.send`](https://developers.google.com/workspace/gmail/api/guides/sending).
This avoids the [`+reply` helper's quoted original](https://github.com/googleworkspace/cli/blob/main/skills/gws-gmail-reply/SKILL.md).
The body round-trips UTF-8 and whitespace; thread ID and source threading headers
are retained. A Reply-To differing from the session recipient is refused by the
host, and missing provider acceptance stays unknown. Untracked replies use
`+reply --message-id`.

AgentMail tracked replies explicitly set `to` to the bound session target and
clear CC/BCC, preventing inherited reply recipients from widening the recorded
audience. Channel bot credentials can acknowledge delivery but cannot confirm
or reject statements, or confirm commitments, as a human owner.
