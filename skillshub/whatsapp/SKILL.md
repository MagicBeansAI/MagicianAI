---
name: whatsapp
version: 0.2.1
description: WhatsApp messaging — send messages, react, list chats/DMs/communities, search contacts, manage
  groups
metadata:
  magician:
    setup: { definition: whatsapp }
    requires:
      bins:
      - whatsapp-reaction-adapter
      - wu
      - node
    install_hint:
      docs: Use the whatsapp login command to authenticate via QR code.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action delivers a message to a real recipient; a canary must
          never send. Tier 1 covers its manifest/adapter join.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - whatsapp-reaction-adapter
        - wu
        - node
        entrypoint: wu
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: denied
          sensitivity: public
        working_directory:
          mode: denied
        limits:
          timeout_secs: 120
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: cli_profile
        requirement: required
        provider: whatsapp
        profile_selection:
          mode: implicit
        storage:
          kind: cli_owned
      policy_floor:
        approval: conditional_external_side_effect
        resource_scopes: []
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        send:
          executable: wu
          description: 'Send one WhatsApp text message to one named chat. THE GATED SEND PATH
            — the recipient is the typed `jid` parameter, so the outward gate can name who
            this act reaches, screen that person against the suppression register and write
            a disclosure record before anything leaves. Routes as `messages send <jid> <text>
            --json`; --json is appended by the runtime, so the result is `{"id":"...","timestamp":...}`
            and the top-level `id` is the WhatsApp message id — carry it as the proof of
            delivery. If a send reports timeout, do NOT retry blindly: run `messages list
            <jid> --limit 5 --json` through the `run` action first to check whether the
            previous send already landed. Use this for every send. A send-shaped `run`
            command is classified as outward and refused because its opaque string cannot
            bind a recipient; it is not a fallback send path.'
          fixed_args:
          - messages
          - send
          parameters:
            jid:
              type: string
              description: Recipient chat JID — WHO this act reaches. Discover it from chats,
                contacts, dms, groups, or message-search results; never invent one. Individual
                chats end in @s.whatsapp.net, groups in @g.us, linked-device identities in
                @lid. Routed as the first positional argument of `messages send`.
              required: true
              min_length: 1
              max_length: 4096
            text:
              type: string
              description: The message body, as plain text. Routed as the second positional
                argument of `messages send`.
              required: true
              min_length: 1
              max_length: 4096
            reply_to:
              type: string
              description: Optional id of the message this one replies to. Maps to `--reply-to`.
              max_length: 4096
          mappings:
          - type: positional
            parameter: jid
          - type: positional
            parameter: text
          - type: flag
            flag: --reply-to
            parameter: reply_to
          - type: literal
            arguments: [--json]
          timeout_secs: 120
        react:
          executable: whatsapp-reaction-adapter
          description: React to an existing WhatsApp message. This routes as `messages react <jid> <msg-id>
            <emoji>` and intentionally does not append --json because reactions do not accept it.
          fixed_args:
          - messages
          - react
          parameters:
            jid:
              type: string
              description: Chat JID discovered from chats, contacts, groups, or message search results.
              required: true
              max_length: 4096
            message_id:
              type: string
              description: WhatsApp message id to react to. Use `messages list <jid> --limit N --json`
                first if needed.
              required: true
              max_length: 4096
            emoji:
              type: string
              description: Reaction emoji text, for example a thumbs-up, heart, laugh, or check-mark emoji.
              required: true
              max_length: 4096
          mappings:
          - type: positional
            parameter: jid
          - type: positional
            parameter: message_id
          - type: positional
            parameter: emoji
          timeout_secs: 30
        run:
          executable: wu
          description: 'Run an arbitrary WhatsApp command selected from this capability guide.
            THE DIAGNOSTIC PATH — the whole command arrives as one opaque `command` string.
            Reading (list, search, info, status) is what this is for. Commands headed by
            `messages send` or `media send` are classified as outward and refused before `wu`
            runs because the opaque form cannot bind a recipient. Use the typed `send` action
            for text and the `react` action for reactions. Add --json inside the command for
            commands that support it and need structured output; do not add --json for
            `messages react`, which rejects it.'
          parameters:
            command:
              type: string
              description: The WhatsApp command to execute, without a leading binary name (for example,
                "messages send <chat_jid> 'Hello' --json")
              required: true
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: command
            max_items: 64
            max_item_bytes: 4096
          timeout_secs: 120
    runtime_catalog:
      categories:
      - messaging
      - whatsapp
      - communication
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 120
---

# Whatsapp

Tool name: `whatsapp`
Requires: WhatsApp Web session.
Auth: Run `login` through this capability to authenticate via QR code.
Use for: sending messages, reading chats/DMs/communities, searching contacts,
managing groups, sending media, reacting to messages on WhatsApp.

## Two action shapes, and only one can send

**`send` is the typed, bindable path.** `run` remains available for reads, but
send-shaped command heads fail closed before the CLI runs.

| | `send` | `run` |
|---|---|---|
| Recipient | typed `jid` parameter | buried inside a `command` string |
| Disclosure record | written before dispatch, naming the recipient | refusal record only |
| Suppression screen | the `jid` is checked against the register | cannot run: recipient is unbindable |
| Capture mode | applies — a rehearsal composes but does not send | cannot run |
| Message id | `id` returned as JSON, available to reconcile against | none; dispatch is refused |

`run` is the diagnostic path for listing, searching, inspection and recovery.
Although `wu` itself can send, the runtime recognizes commands headed by
`messages send` or `media send` as outward and refuses them before the CLI runs:
the opaque string cannot authoritatively bind a recipient. Use `send` to send.

The `command` parameter of `run` is the WhatsApp subcommand and its arguments,
executed through this capability. Do not include any leading binary name. The
runtime does not append `--json` globally to `run` because `messages react`
rejects that flag. Add `--json` explicitly for commands that support it and
need structured output, such as listing or searching messages/chats.

## Sending — the `send` action

    send {"jid":"<chat_jid>","text":"<message>"}
    send {"jid":"<chat_jid>","text":"<message>","reply_to":"<message_id>"}

Routes as `wu messages send <jid> <text> [--reply-to <id>] --json`. The result is

    {"id":"<whatsapp_message_id>","timestamp":<unix_seconds>}

The top-level `id` is the WhatsApp message id and is the proof of delivery —
report it. If the dispatch reports a timeout, do NOT retry blindly: run
`messages list <jid> --limit 5 --json` through `run` first, because the local
store may need a moment to ingest the outgoing message and a blind retry sends
the message twice.

Media and polls are not on the typed `send` action yet. Their opaque `run`
forms are refused, so they are currently unavailable rather than ungated.

When the task frame contains a complete literal command such as
`messages react <jid> <msg-id> <emoji>` or
`messages send <jid> <text> --json`, treat it as naming the requested
operation, then perform it through the action that owns it — `react` and `send`
respectively — rather than retyping it into `run`. A successful `status --json`
readiness check is not completion for a send/react request; completion requires
the send/react itself to succeed.

Available `run` commands (reading and diagnostics):

Messages:
  messages list <jid> --limit 20 --json — list messages in a chat
  messages search <query> --json     — search messages across all chats
  messages send <jid> <text> --json  — refused on `run`; use the `send` action
  messages react <jid> <msg-id> <emoji> — reacts, but prefer the `react` action

For reactions, prefer the dedicated `react` action when available:
  react {"jid":"<chat_jid>","message_id":"<message_id>","emoji":"<emoji>"}
Do not pass `--json` to `messages react`; reactions reject it.
Reactions work for both direct messages and group messages when the target
message exists in the local message store. If the message id is unknown, list or
search messages first to get the correct chat JID and message id.

Chats:
  chats list --json                  — list all chats
  chats search <query> --json        — search chats by name or content

Direct messages:
  dms list --json                    — list opted-in 1:1 chats
  dms list --all --json              — include DMs blocked by constraints (jid only)
  dms search <query> --json          — search 1:1 chats by name

Contacts:
  contacts list --json               — list all contacts
  contacts search <query> --json     — search contacts by name or number
  contacts info <jid> --json         — get contact details

Groups:
  groups list --json                 — list all groups
  groups list --allowed-only --json  — list only groups whose constraint is read/full
  groups list --live --json          — fetch live group metadata from WhatsApp
  groups info <jid> --json           — get group details (members, description)

Communities:
  communities list --json            — list known WhatsApp Communities
  communities list --with-subgroups --json — include linked subgroups

Media:
  media send <jid> <path> --caption <text> --json — send media file with optional caption
  media download <msg-id> --out <dir> --json — download media from a message
  media download-batch <jid> --limit 50 --out <dir> --json — batch download media from a chat

Status:
  status --json                      — check connection status

**Reading `status` output — IMPORTANT.** The session is ready to send/list
messages when:
  • `authenticated: true`, AND
  • `phone` is populated.

Two fields commonly read as failure signals are misleading in this setup
and should be IGNORED:
  • `daemon_running: false` — does NOT mean WhatsApp is offline. It only
    reports a background listener mode that this capability does not use for normal
    sends, lists, searches, or reactions. The capability can be ready even when
    this field is `false`.
  • `registered: false` — can remain `false` after a valid pairing. Treat
    `authenticated: true` plus a populated `phone` as the readiness signal.

If you see `authenticated: true` and `phone` populated, proceed with the typed
`send` action for text. Opaque `messages send` and `media send` commands through
`run` are refused by the outward gate; media remains unavailable until it has a
typed action. Do NOT bail with "WhatsApp not ready" based on `daemon_running`
or `registered`.

JID format: WhatsApp JIDs usually end in `@s.whatsapp.net` for individual
chats, `@g.us` for group chats, and sometimes `@lid` for linked-device identity
chats. Use `chats list`, `contacts search`, or message search results to
discover JIDs rather than inventing them.

Examples:
- {"jid":"<contact_jid>","text":"Hello"} using the `send` action — the gated send
- {"command":"chats list --json"}
- {"command":"dms list --all --json"}
- {"command":"messages list <chat_jid> --limit 20 --json"}
- {"command":"messages search '<query>' --json"}
- {"command":"contacts search '<name_or_number>' --json"}
- {"command":"contacts info <contact_jid> --json"}
- {"command":"groups list --json"}
- {"command":"groups list --allowed-only --json"}
- {"command":"groups info <group_jid> --json"}
- {"command":"communities list --with-subgroups --json"}
- {"command":"media send <chat_jid> <file_path> --caption '<caption>' --json"} — refused; no typed media action exists yet
- {"command":"messages react <chat_jid> <message_id> <emoji>"}
- {"jid":"<chat_jid>","message_id":"<message_id>","emoji":"<emoji>"} using the `react` action
- {"command":"status --json"}
