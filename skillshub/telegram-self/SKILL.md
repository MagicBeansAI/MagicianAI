---
name: telegram-self
version: 0.2.1
description: Telegram personal account via tgcli — send messages, search chats, download media as yourself
  (not bot)
metadata:
  magician:
    setup: { definition: telegram-self }
    requires:
      bins:
      - tgcli
    install_hint:
      docs: 'OAuth setup: tgcli auth --qr'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it drives the operator's own Telegram user account; any call acts
          as that person to their real contacts.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - tgcli
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
          timeout_secs: 30
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: cli_profile
        requirement: required
        provider: telegram-self
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
          description: 'Send one text message from the operator OWN Telegram account to one
            named target. THE GATED SEND PATH — the recipient is the typed `to` parameter, so
            the outward gate can name who this act reaches, screen that person against the
            suppression register and write a disclosure record before anything leaves. Routes
            as `send text --to <target> --message <text> --json`. The result is
            `{"channelId":"<target>","messageId":<n>}` — the top-level `messageId` is the
            Telegram message id and is the proof of delivery. This speaks AS THE OPERATOR to
            their real contacts, not as a bot. Use this for every send. `run` commands headed
            by `send text`, `send photo`, or `send file` are classified as outward and refused
            because their opaque string cannot bind a recipient.'
          fixed_args:
          - send
          - text
          parameters:
            to:
              type: string
              description: Recipient — WHO this act reaches. A @username, a phone number in
                +E.164 form, a channel or group name, or a numeric id. Discover it from
                `channels` or `contacts` through `run`; never invent one. Maps to `--to`.
              required: true
              min_length: 1
              max_length: 4096
            message:
              type: string
              description: The message body, as plain text. No parse mode is applied, so
                Markdown and HTML markup arrive literally. Maps to `--message`.
              required: true
              min_length: 1
              max_length: 4096
            reply_to:
              type: string
              description: Optional id of the message this one replies to, as a positive
                integer written as a string. Maps to `--reply-to`.
              max_length: 64
          mappings:
          - type: flag
            flag: --to
            parameter: to
          - type: flag
            flag: --message
            parameter: message
          - type: flag
            flag: --reply-to
            parameter: reply_to
          - type: literal
            arguments: [--json]
          timeout_secs: 30
        run:
          description: 'Run the telegram-self capability with the arguments selected from this
            capability guide. THE DIAGNOSTIC PATH — the whole invocation arrives as one opaque
            `command` string. Reading (channels, contacts, messages list, messages search,
            media) is what this is for. Commands headed by `send text`, `send photo`, or `send
            file` are classified as outward and refused before `tgcli` runs because the opaque
            form cannot bind a recipient. Use the typed `send` action for text. Inspect
            stdout/stderr, update the runtime ledger when useful, and call goal_reached only
            after the requested result is present.'
          parameters:
            command:
              type: string
              description: The tgcli command to execute (e.g., "send text --to @user --message 'Hello'")
              required: true
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: command
            max_items: 64
            max_item_bytes: 4096
          - type: literal
            arguments: [--json]
          timeout_secs: 30
    runtime_catalog:
      categories:
      - messaging
      - telegram
      - communication
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Telegram Self

Tool name: `telegram-self`
Requires: tgcli available from the scoped capability bot runtime (from @dapi/tgcli)
Auth: Telegram MTProto user session required. Run `tgcli auth --qr` to authenticate via QR code.
Use for: sending messages, reading chats, searching contacts, downloading media,
listing channels on your personal Telegram account (not a bot).

**This speaks as the operator, to their real contacts.** Every call acts as that
person; there is no bot identity standing between the act and their name.

## Two action shapes, and only one can send

**`send` is the typed, bindable path.** `run` remains available for reads, but
send-shaped command heads fail closed before `tgcli` runs.

| | `send` | `run` |
|---|---|---|
| Recipient | typed `to` parameter | buried inside a `command` string |
| Disclosure record | written before dispatch, naming the target | refusal record only |
| Suppression screen | the `to` is checked against the register | cannot run: recipient is unbindable |
| Capture mode | applies — a rehearsal composes but does not send | cannot run |
| Message id | `messageId` returned as JSON, available to reconcile | none; dispatch is refused |

`run` is the diagnostic path for listing channels, reading contacts, searching
messages and downloading media. The runtime recognizes `send text`, `send
photo`, and `send file` command heads and refuses them before `tgcli` runs
because the opaque form cannot authoritatively bind a target. Use `send` for
text messages.

## Sending — the `send` action

    send {"to":"@username","message":"Hello, how are you?"}
    send {"to":"@username","message":"Got it","reply_to":"4242"}

Routes as `tgcli send text --to <target> --message <text> [--reply-to <id>] --json`.
The result is

    {"channelId":"<target>","messageId":4242}

The top-level `messageId` is the Telegram message id and is the proof of
delivery — report it.

Photos and files are not on the typed `send` action yet. Their opaque `run`
forms are refused, so they are currently unavailable rather than ungated.

## The `run` action — reading and everything unmodelled

The `command` parameter is the tgcli subcommand and its arguments. All output is
returned as JSON (--json flag is appended automatically).

Available `run` commands (reading and diagnostics):

Messages:
  send text --to <target> --message <text>      — refused on `run`; use the `send` action
  send photo --to <target> --photo <path> --caption <text> — refused until a typed action exists
  send file --to <target> --file <path>         — refused until a typed action exists
  messages list --chat <target> --limit N        — list messages in a chat
  messages search <query> --chat <target>        — search messages in a chat

Channels:
  channels                                       — list all channels/groups

Contacts:
  contacts                                       — list all contacts

Media:
  media                                          — download media from messages

Target format: Telegram targets can be usernames (@username), phone numbers (+1234567890),
channel/group names, or numeric IDs. Use `channels` or `contacts` to discover targets.

Examples:
- {"to":"@username","message":"Hello, how are you?"} using the `send` action — the gated send
- {"command":"channels"}
- {"command":"contacts"}
- {"command":"send photo --to @username --photo /path/to/image.png --caption 'Check this out'"} — refused; no typed photo action exists yet
- {"command":"send file --to @username --file /path/to/document.pdf"} — refused; no typed file action exists yet
- {"command":"messages list --chat @username --limit 20"}
- {"command":"messages search 'meeting tomorrow' --chat @username"}
