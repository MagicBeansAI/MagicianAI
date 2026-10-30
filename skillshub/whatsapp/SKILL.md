---
name: whatsapp
version: 0.3.1
description: WhatsApp messaging through wu (wu-cli) — send text and file attachments,
  react, list chats/DMs/communities, search contacts, manage groups
metadata:
  magician:
    setup:
      definition: whatsapp
    requires:
      bins:
      - whatsapp-cli-adapter
      - whatsapp-reaction-adapter
      - wu
      - node
      - whatsapp-poll-adapter
    install_hint:
      docs: Use the whatsapp login command to authenticate via QR code.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: every action delivers a message to a real recipient; a canary must
          never send. Tier 1 covers its manifest/adapter join.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - whatsapp-cli-adapter
        - whatsapp-reaction-adapter
        - wu
        - node
        - whatsapp-poll-adapter
        entrypoint: whatsapp-cli-adapter
        environment:
          WU_HOME: '{scope_workdirs_home}/.wu'
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
          timeout_secs: 600
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
          executable: whatsapp-cli-adapter
          description: 'Send one WhatsApp message to one named chat: text or a file
            attachment. THE GATED SEND PATH — the recipient is the typed `jid` parameter,
            so the outward gate can name who this act reaches, screen that person
            against the suppression register and write a disclosure record before
            anything leaves. Routes as `messages send <jid> [text] [--media <file_path>]
            [--caption <caption>] --json`; --json is appended by the runtime, so the
            result is `{"id":"...","timestamp":...}` and the top-level `id` is the
            WhatsApp message id — carry it as the proof of provider acceptance. If
            a send reports timeout, do NOT retry blindly: run `messages list <jid>
            --limit 5 --json` through the `run` action first to check whether the
            previous send already landed. Use this for every send. A send-shaped `run`
            command is classified as outward and refused because its opaque string
            cannot bind a recipient; it is not a fallback send path.'
          fixed_args:
          - messages
          - send
          parameters:
            jid:
              type: string
              description: Recipient chat JID — WHO this act reaches. Discover it
                from chats, contacts, dms, groups, or message-search results; never
                invent one. Individual chats end in @s.whatsapp.net, groups in @g.us,
                linked-device identities in @lid. Routed as the first positional argument
                of `messages send`.
              required: true
              min_length: 1
              max_length: 4096
            text:
              type: string
              description: Plain text body, or fallback caption with file_path. Provide
                text or file_path.
              min_length: 1
              max_length: 4096
            file_path:
              type: workspace_path
              access: read_file
              description: Optional attachment, as a regular file relative to the task
                workdir (scope workdirs outside a task). Absolute paths within the
                current task or scope workdirs are accepted; traversal and symlinks
                are refused. PDFs are documents; wu selects native image/video/audio
                media for those formats.
              max_length: 4096
            caption:
              type: string
              description: Optional attachment caption, overriding text for media
                (upstream audio sends do not carry captions).
              max_length: 4096
            reply_to:
              type: string
              description: Optional id of the message this one replies to. Maps to
                `--reply-to`.
              max_length: 4096
          mappings:
          - type: positional
            parameter: jid
          - type: positional
            parameter: text
          - type: flag
            flag: --media
            parameter: file_path
          - type: flag
            flag: --caption
            parameter: caption
          - type: flag
            flag: --reply-to
            parameter: reply_to
          - type: literal
            arguments:
            - --json
          timeout_secs: 300
        react:
          executable: whatsapp-reaction-adapter
          description: React to an existing WhatsApp message. This routes as `messages
            react <jid> <msg-id> <emoji>` and intentionally does not append --json
            because reactions do not accept it.
          fixed_args:
          - messages
          - react
          parameters:
            jid:
              type: string
              description: Chat JID discovered from chats, contacts, groups, or message
                search results.
              required: true
              max_length: 4096
            message_id:
              type: string
              description: WhatsApp message id to react to. Use `messages list <jid>
                --limit N --json` first if needed.
              required: true
              max_length: 4096
            emoji:
              type: string
              description: Reaction emoji text, for example a thumbs-up, heart, laugh,
                or check-mark emoji.
              required: true
              max_length: 4096
          mappings:
          - type: positional
            parameter: jid
          - type: positional
            parameter: message_id
          - type: positional
            parameter: emoji
          timeout_secs: 300
        run:
          executable: whatsapp-cli-adapter
          description: Run an arbitrary WhatsApp command selected from this capability
            guide. THE DIAGNOSTIC PATH — the whole command arrives as one opaque `command`
            string. Reading (list, search, info, status) is what this is for. Commands
            headed by `messages send` or `media send` are classified as outward and
            refused before `wu` runs because the opaque form cannot bind a recipient.
            Use the typed `send` action for text or attachments and `react` for reactions.
            Add --json inside the command for commands that support it and need structured
            output; do not add --json for `messages react`, which rejects it.
          parameters:
            command:
              type: string
              description: A read-only WhatsApp command without a leading binary name,
                for example "chats list --json".
              required: true
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: command
            max_items: 64
            max_item_bytes: 4096
          timeout_secs: 600
        poll:
          executable: whatsapp-poll-adapter
          description: Send a native single-choice poll to a typed recipient. Polls
            are not exposed by upstream MCP.
          fixed_args: []
          parameters:
            jid:
              type: string
              description: Recipient chat JID — WHO this act reaches. Discover it
                from chats, contacts, dms, groups, or message-search results; never
                invent one. Individual chats end in @s.whatsapp.net, groups in @g.us,
                linked-device identities in @lid. Routed as the first positional argument
                of `messages send`.
              required: true
              min_length: 1
              max_length: 4096
            poll_question:
              type: string
              description: Question shown above the poll's choices.
              required: true
              min_length: 1
              max_length: 4096
            poll_options:
              type: string_array
              description: Two to twelve distinct choices for a single-choice poll.
              required: true
              min_items: 2
              max_items: 12
              max_item_bytes: 100
          mappings:
          - type: positional
            parameter: jid
          - type: flag
            flag: --question
            parameter: poll_question
          - type: literal
            arguments:
            - --options
          - type: passthrough
            parameter: poll_options
          timeout_secs: 300
    runtime_catalog:
      categories:
      - messaging
      - whatsapp
      - communication
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 120
---

# WhatsApp CLI companion

Prefer `whatsapp-mcp` for upstream-native schemas: messages, files, reactions,
search/context/counts, exports, history, downloads, enrichment and groups.
This CLI companion retains compatible typed sends, repairs linked-device
reaction keys and supports polls (not exposed by upstream MCP).

- `send`: text or a staged attachment, with an optional reply/caption. Relative
  paths use the task workdir (scope workdirs outside a task); explicit absolute
  paths may name files in either admitted root. Traversal and symlinks are refused.
- `poll`: a question and 2–12 distinct choices. The small adapter uses upstream
  `messages.send` IPC with a real options array, preserving commas in a choice.
- `react`: stored message keys are preserved and the bot's existing connection
  is reused when available.
- `run`: any native wu command, including writes and administration. Arguments
  are split into inert argv without a shell; write/admin/unknown commands ask
  for approval. Use upstream `--help` for options. Import/deletion, `doctor`,
  reindex/vacuum and other CLI-only capabilities remain available here.

Use typed `send`/`poll` or native MCP for sends: opaque CLI send strings cannot
bind their recipients for the outward gate. Every typed send passes recipient,
suppression, capture and disclosure controls. A provider id proves acceptance,
not delivery. If a send times out, inspect recent messages before retrying.

The scoped bot owns the WhatsApp connection and daemon IPC lock; tools reuse
that socket. Authentication lives in scoped `WU_HOME`. Use the WhatsApp setup
flow for pairing and native status diagnostics; never request auth files or
credentials in chat. wu's collection/read/send/manage constraints still apply.
