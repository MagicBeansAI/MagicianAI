---
name: agentmail-read
version: 0.2.1
description: 'Read Magican''s AgentMail inbox (magican@agentmail.to): list/get messages, threads, drafts,
  attachments. Free reads (the send cap is on agentmail-send).'
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.agentmail.to
    requires:
      bins:
      - agentmail
    install_hint:
      docs: npm install -g agentmail-cli; set AGENT_MAIL_KEY (inbox-scoped) in skillshub/operator-config.yaml;
        make -C skillshub setup-env
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          its read-only actions require a live mailbox and would read real
          correspondence; there is no synthetic inbox to probe.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - agentmail
      runtime:
        protocol: cli
        command_prefix:
        - --format
        - json
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
        provider: agentmail
        secret_bindings:
        - name: AGENT_MAIL_KEY
          secret_ref: AGENT_MAIL_KEY
        injections:
        - source:
            kind: secret
            binding: AGENT_MAIL_KEY
          target:
            kind: environment
            name: AGENTMAIL_API_KEY
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        inboxes_list:
          description: 'Run `agentmail inboxes list` to list the inboxes this account''s key can

            read (normally just the account''s own inbox). Confirms the inbox to use.

            '
          fixed_args:
          - inboxes
          - list
          parameters:
            limit:
              type: integer
              description: Max items to return. Maps to `--limit`.
            ascending:
              type: boolean
              description: Sort in ascending temporal order. Maps to `--ascending` (value flag).
            page_token:
              type: string
              description: Pagination cursor from a prior page. Maps to `--page-token`.
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --limit
            parameter: limit
          - type: flag
            flag: --ascending
            parameter: ascending
          - type: flag
            flag: --page-token
            parameter: page_token
          - type: passthrough
            parameter: extra_args
        messages_list:
          description: 'Run `agentmail inboxes:messages list` to list messages in the account''s

            inbox. Use `label` to filter and `limit` to cap results.

            '
          fixed_args:
          - inboxes:messages
          - list
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            limit:
              type: integer
              description: Max messages to return. Maps to `--limit`.
            label:
              type: string
              description: Label to filter by. Maps to `--label`.
              max_length: 4096
            ascending:
              type: boolean
              description: Sort in ascending temporal order. Maps to `--ascending` (value flag).
            before:
              type: string
              description: Only messages before this timestamp. Maps to `--before`.
              max_length: 4096
            after:
              type: string
              description: Only messages after this timestamp. Maps to `--after`.
              max_length: 4096
            include_spam:
              type: boolean
              description: Include spam in results. Maps to `--include-spam` (value flag).
            include_trash:
              type: boolean
              description: Include trash in results. Maps to `--include-trash` (value flag).
            page_token:
              type: string
              description: Pagination cursor. Maps to `--page-token`.
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --limit
            parameter: limit
          - type: flag
            flag: --label
            parameter: label
          - type: flag
            flag: --ascending
            parameter: ascending
          - type: flag
            flag: --before
            parameter: before
          - type: flag
            flag: --after
            parameter: after
          - type: flag
            flag: --include-spam
            parameter: include_spam
          - type: flag
            flag: --include-trash
            parameter: include_trash
          - type: flag
            flag: --page-token
            parameter: page_token
          - type: passthrough
            parameter: extra_args
        messages_get:
          description: Run `agentmail inboxes:messages get` to fetch one message (headers + body) by id.
          fixed_args:
          - inboxes:messages
          - get
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            message_id:
              type: string
              description: Id of the message. Maps to `--message-id`.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --message-id
            parameter: message_id
          - type: passthrough
            parameter: extra_args
        threads_list:
          description: Run `agentmail inboxes:threads list` to list conversation threads in the account's
            inbox.
          fixed_args:
          - inboxes:threads
          - list
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            limit:
              type: integer
              description: Max threads to return. Maps to `--limit`.
            label:
              type: string
              description: Label to filter by. Maps to `--label`.
              max_length: 4096
            ascending:
              type: boolean
              description: Sort in ascending temporal order. Maps to `--ascending` (value flag).
            before:
              type: string
              description: Only threads before this timestamp. Maps to `--before`.
              max_length: 4096
            after:
              type: string
              description: Only threads after this timestamp. Maps to `--after`.
              max_length: 4096
            page_token:
              type: string
              description: Pagination cursor. Maps to `--page-token`.
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --limit
            parameter: limit
          - type: flag
            flag: --label
            parameter: label
          - type: flag
            flag: --ascending
            parameter: ascending
          - type: flag
            flag: --before
            parameter: before
          - type: flag
            flag: --after
            parameter: after
          - type: flag
            flag: --page-token
            parameter: page_token
          - type: passthrough
            parameter: extra_args
        threads_get:
          description: Run `agentmail inboxes:threads get` to fetch one thread (all messages) by id.
          fixed_args:
          - inboxes:threads
          - get
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            thread_id:
              type: string
              description: Id of the thread. Maps to `--thread-id`.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --thread-id
            parameter: thread_id
          - type: passthrough
            parameter: extra_args
        drafts_list:
          description: Run `agentmail inboxes:drafts list` to list saved drafts in the account's inbox.
          fixed_args:
          - inboxes:drafts
          - list
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            limit:
              type: integer
              description: Max drafts to return. Maps to `--limit`.
            label:
              type: string
              description: Label to filter by. Maps to `--label`.
              max_length: 4096
            ascending:
              type: boolean
              description: Sort in ascending temporal order. Maps to `--ascending` (value flag).
            page_token:
              type: string
              description: Pagination cursor. Maps to `--page-token`.
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --limit
            parameter: limit
          - type: flag
            flag: --label
            parameter: label
          - type: flag
            flag: --ascending
            parameter: ascending
          - type: flag
            flag: --page-token
            parameter: page_token
          - type: passthrough
            parameter: extra_args
        attachment_get:
          description: 'Run `agentmail inboxes:messages get-attachment` to fetch one attachment

            from a message by id.

            '
          fixed_args:
          - inboxes:messages
          - get-attachment
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email). Normally omit — resolved from
                `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            message_id:
              type: string
              description: Id of the message. Maps to `--message-id`.
              required: true
              max_length: 4096
            attachment_id:
              type: string
              description: Id of the attachment (from the message's attachments list). Maps to `--attachment-id`.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --message-id
            parameter: message_id
          - type: flag
            flag: --attachment-id
            parameter: attachment_id
          - type: passthrough
            parameter: extra_args
        help:
          description: Show top-level AgentMail CLI help without exposing an arbitrary command lane.
          fixed_args:
          - --help
    runtime_catalog:
      categories:
      - messaging
      - email
      - agentmail
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# AgentMail — Read

Magican has its own email inbox at **magican@agentmail.to** via AgentMail (an Email
API for agents). This tool READS that inbox. Sending is the separate
`agentmail-send` tool (capped at 50/day). The key is inbox-scoped — no
org/domain ops.

**Accounts:** pass `account` to choose which AgentMail inbox to read. The
default `account` is **work** → magican@agentmail.to. Each account has its own
inbox-scoped key; aliases are flexible. The inbox is resolved from the account,
so you normally **don't pass `inbox_id`** — it's an optional override only.

Inner actions: `inboxes_list`, `messages_list`, `messages_get`, `threads_list`,
`threads_get`, `drafts_list`, `attachment_get`, `help`. Output is JSON.

Common use: read the inbox to find a specific email (e.g. a verification code),
list recent threads, fetch a message body. You rarely need anything else — the
typed actions cover normal use.

For an operation not surfaced here, the full reference is
<https://docs.agentmail.to/llms.txt> (overview) and
<https://docs.agentmail.to/llms-full.txt> (complete) — consult only when a
needed read operation is absent, then add a reviewed typed action instead of
using the read credential through an arbitrary command lane.
