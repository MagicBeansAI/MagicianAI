---
name: kapso-whatsapp-read
version: 0.2.2
description: 'Read Presto''s OWN WhatsApp (its Kapso/Meta-Cloud number): list/get conversations and messages,
  list/resolve numbers. Free reads (the send cap is on kapso-whatsapp-send). This is the agent''s WhatsApp
  identity, NOT the user''s personal WhatsApp.'
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.kapso.ai
    requires:
      bins:
      - kapso-governed
      - kapso
    install_hint:
      docs: 'The exact Kapso CLI version is pinned once in the skillshub root npm workspace; run
        `make -C skillshub setup-kapso-cli`. Configure KAPSO_API_KEY + KAPSO_PHONE_NUMBER_ID
        for each intended scope, then run `make -C skillshub setup-env SCOPE=<principal>/<workspace>`.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          its read-only actions require a provisioned business number and
          would read real conversations.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - kapso-governed
        - kapso
        entrypoint: kapso-governed
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: denied
          sensitivity: public
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 30
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        provider: kapso
        secret_bindings:
        - name: KAPSO_API_KEY
          secret_ref: KAPSO_API_KEY
        - name: KAPSO_PHONE_NUMBER_ID
          secret_ref: KAPSO_PHONE_NUMBER_ID
        injections:
        - source:
            kind: secret
            binding: KAPSO_API_KEY
          target:
            kind: environment
            name: KAPSO_API_KEY
        - source:
            kind: secret
            binding: KAPSO_PHONE_NUMBER_ID
          target:
            kind: environment
            name: KAPSO_PHONE_NUMBER_ID
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        messages_list:
          description: 'Run `kapso whatsapp messages list` to list messages on Presto''s own

            WhatsApp number (cursor-paginated). Use `direction inbound` to see

            messages people sent to Presto, `since`/`limit` to bound results. The

            identity adapter scopes this to Presto''s number automatically.

            '
          fixed_args:
          - whatsapp
          - messages
          - list
          parameters:
            direction:
              type: string
              description: Filter by direction. Maps to `--direction`.
              enum_values:
              - inbound
              - outbound
              max_length: 4096
            status:
              type: string
              description: Filter by message status. Maps to `--status`.
              enum_values:
              - pending
              - sent
              - delivered
              - read
              - failed
              max_length: 4096
            since:
              type: string
              description: Only messages created at/after this ISO timestamp. Maps to `--since`.
              max_length: 4096
            until:
              type: string
              description: Only messages created at/before this ISO timestamp. Maps to `--until`.
              max_length: 4096
            conversation:
              type: string
              description: Filter by conversation id. Maps to `--conversation`.
              max_length: 4096
            limit:
              type: integer
              description: Max messages to return. Maps to `--limit`.
            after:
              type: string
              description: Cursor for the next page. Maps to `--after`.
              max_length: 4096
            before:
              type: string
              description: Cursor for the previous page. Maps to `--before`.
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --direction
            parameter: direction
          - type: flag
            flag: --status
            parameter: status
          - type: flag
            flag: --since
            parameter: since
          - type: flag
            flag: --until
            parameter: until
          - type: flag
            flag: --conversation
            parameter: conversation
          - type: flag
            flag: --limit
            parameter: limit
          - type: flag
            flag: --after
            parameter: after
          - type: flag
            flag: --before
            parameter: before
          - type: passthrough
            parameter: extra_args
        messages_get:
          description: Run `kapso whatsapp messages get <message-id>` to fetch one message by its WhatsApp
            message id.
          fixed_args:
          - whatsapp
          - messages
          - get
          parameters:
            message_id:
              type: string
              description: WhatsApp message id (positional MESSAGEID), e.g. a wamid.* value.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: message_id
          - type: passthrough
            parameter: extra_args
        conversations_list:
          description: 'Run `kapso whatsapp conversations list` to list conversations on Presto''s

            own WhatsApp number, sorted by most recent activity. Filter by `status`

            (active/ended) or by contact `phone`. The identity adapter scopes this to Presto''s

            number automatically.

            '
          fixed_args:
          - whatsapp
          - conversations
          - list
          parameters:
            status:
              type: string
              description: Filter by conversation status. Maps to `--status`.
              enum_values:
              - active
              - ended
              max_length: 4096
            phone:
              type: string
              description: Filter by contact phone number. Maps to `--phone`.
              max_length: 4096
            assigned_user:
              type: string
              description: Filter by assigned user id. Maps to `--assigned-user`.
              max_length: 4096
            unassigned:
              type: boolean
              description: Only include unassigned conversations. Maps to `--unassigned` (value flag).
            page:
              type: integer
              description: Page number. Maps to `--page`.
            per_page:
              type: integer
              description: Results per page. Maps to `--per-page`.
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --status
            parameter: status
          - type: flag
            flag: --phone
            parameter: phone
          - type: flag
            flag: --assigned-user
            parameter: assigned_user
          - type: flag
            flag: --unassigned
            parameter: unassigned
          - type: flag
            flag: --page
            parameter: page
          - type: flag
            flag: --per-page
            parameter: per_page
          - type: passthrough
            parameter: extra_args
        conversations_get:
          description: Run `kapso whatsapp conversations get <conversation-id>` to fetch one conversation
            (with its messages) by id.
          fixed_args:
          - whatsapp
          - conversations
          - get
          parameters:
            conversation_id:
              type: string
              description: Conversation id (positional CONVERSATIONID).
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: conversation_id
          - type: passthrough
            parameter: extra_args
        numbers_list:
          description: 'Run `kapso whatsapp numbers list` to list the WhatsApp numbers in this

            Kapso project (normally Presto''s own number plus any sandbox). Confirms

            the phone-number-id in use.

            '
          fixed_args:
          - whatsapp
          - numbers
          - list
          parameters:
            page:
              type: integer
              description: Page number. Maps to `--page`.
            per_page:
              type: integer
              description: Results per page. Maps to `--per-page`.
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --page
            parameter: page
          - type: flag
            flag: --per-page
            parameter: per_page
          - type: passthrough
            parameter: extra_args
        numbers_resolve:
          description: 'Run `kapso whatsapp numbers resolve <number-ref>` to resolve a WhatsApp

            number reference (a display phone number or a Meta phone-number-id) to its

            canonical phone-number-id.

            '
          fixed_args:
          - whatsapp
          - numbers
          - resolve
          parameters:
            number_ref:
              type: string
              description: WhatsApp phone-number-id or display phone number (positional NUMBERREF).
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: number_ref
          - type: passthrough
            parameter: extra_args
        help:
          description: Show top-level Kapso CLI help without exposing an arbitrary command lane.
          fixed_args:
          - --help
    runtime_catalog:
      categories:
      - messaging
      - whatsapp
      - kapso
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Kapso WhatsApp — Read

Presto has its **own WhatsApp number** (`KAPSO_PHONE_NUMBER`, the "Presto" number
on Kapso's Meta Cloud API). This is **Presto's agent identity** — like its
AgentMail inbox — and this tool READS the conversations and messages on it.
Sending is the separate `kapso-whatsapp-send` tool (capped at 50/day).

**This is NOT the user's personal WhatsApp.** The user's own WhatsApp (via
WhatsApp Web / QR pairing) is the separate `whatsapp` tool. Use that for the
user's chats; use this for Presto's own number.

Authentication is the project-scoped `KAPSO_API_KEY` (no browser/login). The
per-number reads are scoped to Presto's number automatically: the identity adapter
injects `--phone-number-id` (`KAPSO_PHONE_NUMBER_ID`) for `messages list` and
`conversations list`; by-id lookups and `numbers *` do not need it.

Inner actions: `messages_list`, `messages_get`, `conversations_list`,
`conversations_get`, `numbers_list`, `numbers_resolve`, `help`. Output is
JSON.

Common use: list active conversations, find a recent inbound message (e.g. a
verification code or a reply someone sent to Presto), fetch a message body, or
fetch a full conversation by id. The typed actions cover normal use.

For an operation not surfaced here, the reference is
<https://docs.kapso.ai/docs/whatsapp/cli> — consult only when a needed read
operation is absent, then add a reviewed typed action instead of using the read
credential through an arbitrary command lane.
