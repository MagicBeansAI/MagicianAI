# Messaging as Execution Capabilities

> **Status (2026-09-02 audit):** Keep as a future proposal; do not archive.
> The 2026-05 pack-layout (`bots/whatsapp/`, `tool_schema.yaml`,
> `GET /capabilities/auth-status`) did **not** ship. Messaging that did
> ship is AgentSkills (`skillshub/whatsapp`, `skillshub/telegram`,
> `skillshub/kapso-whatsapp-*`). The proposed `auth:` lifecycle **did**
> ship: `CapabilityAuthConfig` in `execution/capability.rs` and
> `ensure_auth` / `maybe_reauth_and_retry` in `compiled_providers.rs`.
> This file is the original proposal, not the living contract.
>
> **Status (2026-05-09):** Future proposal. Pack location references point at
> `magician_data_v3/system/capability_templates/packs/...` from the pre-
> AgentSkills layout. If/when the unbuilt pack layout lands, those packs
> would be AgentSkills v1 skills under `skillshub/<name>/`.

## Context

The consumer channel bots (`bots/whatsapp/`, `bots/telegram/`, etc.) maintain persistent connections to messaging platforms. They serve two roles:

1. **Chat channel** (inbound) -- receive messages, route to ChatService (see `consumer_channels.md`)
2. **Execution capability** (outbound + read) -- the agent sends messages and reads conversations during task execution

This doc covers role 2. Each bot exposes CLI commands (`send`, `read`, `list-chats`) that the execution pipeline would invoke via skill `tool_schema.yaml` definitions through the shell capability.

## How It Works

Each bot binary supports two modes:

```
Bot mode (long-running, managed by magician):
  node bots/whatsapp/dist/index.js --mode bot
  → persistent Baileys connection, receives messages, caches locally, calls magician chat API

CLI mode (one-shot, for execution capabilities):
  node bots/whatsapp/dist/index.js send +919876543210 "Flight confirmed"
  node bots/whatsapp/dist/index.js read +919876543210 --since 5m --limit 10
  node bots/whatsapp/dist/index.js list-chats
  → reads from shared auth state + local message cache, executes, exits
```

Both modes share:
- **Auth state** on disk (Baileys `auth_info/`, Telegram bot token file)
- **Message cache** on disk (SQLite or JSON -- bot mode writes, CLI mode reads)

CLI `read` depends on the bot having run and cached messages. If the bot hasn't run, `read` returns empty.

## Standard CLI Commands

Each bot implements these commands:

| Command | What it does | Depends on bot running? |
|---|---|---|
| `send <recipient> <message>` | Send a text message | No (uses auth state directly) |
| `send-image <recipient> <path>` | Send an image | No |
| `send-file <recipient> <path>` | Send a file/document | No |
| `read <contact> [--since N] [--limit N]` | Read recent messages from local cache | Yes (bot must have cached messages) |
| `list-chats` | List recent conversations from local cache | Yes |
| `status` | Check connection/auth status | No |

## Capability Packs

YAML packs teach the LLM what CLI commands are available:

### WhatsApp (proposed: `skillshub/whatsapp/tool_schema.yaml` → `magician_data_v3/scopes/<…>/<…>/skills/whatsapp/tool_schema.yaml`)

```yaml
name: whatsapp
description: Send and read WhatsApp messages via the WhatsApp bot CLI
version: "1.0.0"
auth:
  required: true
  setup_command: "node {scope_capabilities_root}/bots/whatsapp/dist/index.js setup"
  check_command: "node {scope_capabilities_root}/bots/whatsapp/dist/index.js status"
  reauth_command: "node {scope_capabilities_root}/bots/whatsapp/dist/index.js setup"
  error_patterns: ["not authenticated", "session expired", "QR code required"]
guide: |
  Send a message:
    node {scope_capabilities_root}/bots/whatsapp/dist/index.js send <phone_with_country_code> '<message>'
    Example: node {scope_capabilities_root}/bots/whatsapp/dist/index.js send +919876543210 'Your flight is confirmed'
  Read recent messages from a contact (requires bot to be running):
    node {scope_capabilities_root}/bots/whatsapp/dist/index.js read <phone> --since 5m --limit 10
  List recent chats (requires bot to be running):
    node {scope_capabilities_root}/bots/whatsapp/dist/index.js list-chats
  Send an image:
    node {scope_capabilities_root}/bots/whatsapp/dist/index.js send-image <phone> <path>
  Send a file:
    node {scope_capabilities_root}/bots/whatsapp/dist/index.js send-file <phone> <path>
parameters: []
implementation:
  type: compiled
  provider_name: shell
```

### Telegram (proposed: `skillshub/telegram_msg/tool_schema.yaml` → `magician_data_v3/system/skills/telegram_msg/tool_schema.yaml`)

```yaml
name: telegram_msg
description: Send and read Telegram messages via the Telegram bot CLI
version: "1.0.0"
auth:
  required: true
  setup_command: "node {scope_capabilities_root}/bots/telegram/dist/index.js setup"
  check_command: "node {scope_capabilities_root}/bots/telegram/dist/index.js status"
  reauth_command: "node {scope_capabilities_root}/bots/telegram/dist/index.js setup"
  error_patterns: ["not configured", "unauthorized", "token invalid"]
guide: |
  Send a message:
    node {scope_capabilities_root}/bots/telegram/dist/index.js send <chat_id> '<message>'
  Read recent messages (requires bot to be running):
    node {scope_capabilities_root}/bots/telegram/dist/index.js read <chat_id> --since 5m --limit 10
  Send a file:
    node {scope_capabilities_root}/bots/telegram/dist/index.js send-file <chat_id> <path>
  Send an image:
    node {scope_capabilities_root}/bots/telegram/dist/index.js send-image <chat_id> <path>
parameters: []
implementation:
  type: compiled
  provider_name: shell
```

### Google Workspace (already partially shipped: see `skillshub/{gmail,calendar,sheets}/tool_schema.yaml` — not the unified pack proposed below)

```yaml
name: google_workspace
description: Gmail, Calendar, Drive, Sheets via Google Workspace CLI (gws)
version: "1.0.0"
auth:
  required: true
  setup_command: "gws auth setup"
  check_command: "gws auth status"
  reauth_command: "gws auth login"
  error_patterns: ["token expired", "invalid_grant", "401", "not authenticated"]
guide: |
  Gmail - list recent emails:
    gws gmail messages list --maxResults 10
  Gmail - send email:
    gws gmail messages send --to '<email>' --subject '<subject>' --body '<body>'
  Gmail - search:
    gws gmail messages list --q 'from:ruchika subject:invoice'
  Calendar - list events:
    gws calendar events list --calendarId primary --maxResults 5
  Calendar - create event:
    gws calendar events create --calendarId primary --summary '<title>' --start '<datetime>'
  Drive - list files:
    gws drive files list --q "name contains '<query>'"
  Drive - upload:
    gws drive files upload --file <path>
  Sheets - read:
    gws sheets values get --spreadsheetId '<id>' --range 'Sheet1!A1:D10'
parameters: []
implementation:
  type: compiled
  provider_name: shell
```

## What This Enables

| Use Case | Agent Action |
|---|---|
| "Send this to my WhatsApp" | `shell: node {scope_capabilities_root}/bots/whatsapp/dist/index.js send +91... 'Flight confirmed'` |
| "Message Priya on Telegram" | `shell: node {scope_capabilities_root}/bots/telegram/dist/index.js send 987654321 'Task done'` |
| "Check if vendor replied on WhatsApp" | `shell: node {scope_capabilities_root}/bots/whatsapp/dist/index.js read +91... --since 10m` |
| "Forward receipt via WhatsApp" | `shell: node {scope_capabilities_root}/bots/whatsapp/dist/index.js send-image +91... /tmp/receipt.png` |
| "Send email with the report" | `shell: gws gmail messages send --to 'boss@co.com' --subject 'Report' --body '...'` |
| "What meetings do I have today?" | `shell: gws calendar events list --calendarId primary --maxResults 10` |

## Unsolicited Inbound Messages

When someone sends a message WITHOUT the agent asking for it, the **bot process** (not the CLI) receives it via its persistent connection. This is a **consumer channel** concern, not an execution capability:

```
Someone sends WhatsApp message
    |
    v
Bot process (Baileys WebSocket, always listening)
    |
    v
Bot calls magician chat API → routes to ChatService → user sees in chat
    |
    v
(Optionally) Agent evaluates and decides to process or ignore
```

This is covered in `consumer_channels.md`, not here.

---

## Auth-Aware Capability Packs

CLI tools that require authentication declare their auth lifecycle in the YAML capability pack. This is a **general mechanism** -- not messaging-specific. Any CLI tool (gws, gh, aws, etc.) can use it.

### Schema

```yaml
auth:
  required: true
  setup_command: "gws auth setup"           # first-time setup
  check_command: "gws auth status"          # verify auth valid (exit 0 = ok)
  reauth_command: "gws auth login"          # re-authenticate on expiry
  error_patterns:                           # strings in output that indicate auth failure
    - "token expired"
    - "401"
    - "not authenticated"
```

### System Behavior

**Before first use:**
1. Run `check_command` (if defined)
2. If exit code != 0 → run `setup_command`
3. If setup requires user interaction (QR, browser), surface via settings UI
4. Cache auth status until next failure

**During execution:**
1. Shell capability runs the command
2. If command fails AND output matches any `error_patterns` → auth expired
3. Run `reauth_command`
4. Retry the original command once
5. If still fails → report auth error to the agent

**In settings UI:**
- Show auth status per capability: "Authenticated" / "Needs setup" / "Expired"
- "Authenticate" / "Re-authenticate" buttons

### API Endpoints

```
GET  /api/magician/v2/capabilities/auth-status     → list auth-requiring capabilities with status
POST /api/magician/v2/capabilities/{name}/auth      → trigger setup/login
POST /api/magician/v2/capabilities/{name}/reauth    → trigger reauth
```

---

## Dependencies

- Existing shell capability (executes CLI commands)
- Existing YAML capability pack system (extended with `auth` section)
- Bot binaries (`bots/whatsapp/`, `bots/telegram/`) with CLI mode
- `gws` CLI (npm install -g @googleworkspace/cli)
- Bot lifecycle management for `read`/`list-chats` (bot must be running to cache messages)
