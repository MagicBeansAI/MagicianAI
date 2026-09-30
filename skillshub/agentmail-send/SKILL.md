---
name: agentmail-send
version: 0.2.1
description: Send/reply/forward email from Magican's AgentMail inbox (magican@agentmail.to). Capped at 50
  EMAIL_SENDS/day. Reading is the separate (free) agentmail-read tool.
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
          every action delivers a message to a real recipient; a canary must
          never send. Tier 1 covers its manifest/adapter join.
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
        approval: conditional_external_side_effect
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        send:
          description: 'Run `agentmail inboxes:messages send` to send a new email as Presto.

            Required: to, subject; provide one of text/html. The from-inbox is the

            account''s inbox (default "work" → magican@agentmail.to). Confirm

            recipients/subject/body before sending — each send debits 1 EMAIL_SENDS.

            '
          fixed_args:
          - inboxes:messages
          - send
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email to send from). Normally omit —
                resolved from `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            to:
              type: string
              description: Primary recipient. Maps to `--to`. For multiple recipients pass extra `--to`
                pairs via extra_args.
              required: true
              max_length: 4096
            subject:
              type: string
              description: Email subject line. Maps to `--subject`.
              required: true
              max_length: 4096
            text:
              type: string
              description: Plain-text body. Provide this or html. Maps to `--text`.
              max_length: 4096
            html:
              type: string
              description: HTML body. Provide this or text. Maps to `--html`.
              max_length: 4096
            cc:
              type: string
              description: CC recipient. Maps to `--cc`.
              max_length: 4096
            bcc:
              type: string
              description: BCC recipient. Maps to `--bcc`.
              max_length: 4096
            labels:
              type: string
              description: Label to attach to the sent message. Maps to `--label`.
              max_length: 4096
            extra_args:
              type: string_array
              description: 'Escape hatch — extra argv tokens for repeated/unsurfaced flags,

                e.g. additional recipients ["--to", "b@x.com", "--cc", "c@x.com"]

                or attachments ["--attachment", "/tmp/x.pdf"].

                '
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --inbox-id
            parameter: inbox_id
          - type: flag
            flag: --to
            parameter: to
          - type: flag
            flag: --subject
            parameter: subject
          - type: flag
            flag: --text
            parameter: text
          - type: flag
            flag: --html
            parameter: html
          - type: flag
            flag: --cc
            parameter: cc
          - type: flag
            flag: --bcc
            parameter: bcc
          - type: flag
            flag: --label
            parameter: labels
          - type: passthrough
            parameter: extra_args
        reply:
          description: 'Run `agentmail inboxes:messages reply` to reply to a message by id.

            Required: message_id; provide one of text/html. Set reply_all to reply

            to all recipients. Debits 1 EMAIL_SENDS.

            '
          fixed_args:
          - inboxes:messages
          - reply
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email to send from). Normally omit —
                resolved from `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            message_id:
              type: string
              description: Id of the message to reply to. Maps to `--message-id`.
              required: true
              max_length: 4096
            text:
              type: string
              description: Plain-text reply body. Provide this or html. Maps to `--text`.
              max_length: 4096
            html:
              type: string
              description: HTML reply body. Provide this or text. Maps to `--html`.
              max_length: 4096
            reply_all:
              type: boolean
              description: Reply to all recipients of the original message. Maps to `--reply-all` (value
                flag).
            cc:
              type: string
              description: CC recipient. Maps to `--cc`.
              max_length: 4096
            bcc:
              type: string
              description: BCC recipient. Maps to `--bcc`.
              max_length: 4096
            extra_args:
              type: string_array
              description: 'Escape hatch — extra argv tokens for repeated/unsurfaced flags,

                e.g. additional recipients ["--to", "b@x.com", "--cc", "c@x.com"]

                or attachments ["--attachment", "/tmp/x.pdf"].

                '
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
            flag: --text
            parameter: text
          - type: flag
            flag: --html
            parameter: html
          - type: flag
            flag: --reply-all
            parameter: reply_all
          - type: flag
            flag: --cc
            parameter: cc
          - type: flag
            flag: --bcc
            parameter: bcc
          - type: passthrough
            parameter: extra_args
        forward:
          description: 'Run `agentmail inboxes:messages forward` to forward a message to new

            recipients. Required: message_id, to. Debits 1 EMAIL_SENDS.

            '
          fixed_args:
          - inboxes:messages
          - forward
          parameters:
            inbox_id:
              type: string
              description: Optional inbox-id override (the inbox email to send from). Normally omit —
                resolved from `account`. Maps to `--inbox-id`.
              max_length: 4096
              default: magican@agentmail.to
            message_id:
              type: string
              description: Id of the message to reply to. Maps to `--message-id`.
              required: true
              max_length: 4096
            to:
              type: string
              description: Forward recipient. Maps to `--to`. For multiple, pass extra `--to` pairs via
                extra_args.
              required: true
              max_length: 4096
            text:
              type: string
              description: Optional intro text added before the forwarded content. Maps to `--text`.
              max_length: 4096
            html:
              type: string
              description: Optional HTML intro added before the forwarded content. Maps to `--html`.
              max_length: 4096
            cc:
              type: string
              description: CC recipient. Maps to `--cc`.
              max_length: 4096
            bcc:
              type: string
              description: BCC recipient. Maps to `--bcc`.
              max_length: 4096
            extra_args:
              type: string_array
              description: 'Escape hatch — extra argv tokens for repeated/unsurfaced flags,

                e.g. additional recipients ["--to", "b@x.com", "--cc", "c@x.com"]

                or attachments ["--attachment", "/tmp/x.pdf"].

                '
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
            flag: --to
            parameter: to
          - type: flag
            flag: --text
            parameter: text
          - type: flag
            flag: --html
            parameter: html
          - type: flag
            flag: --cc
            parameter: cc
          - type: flag
            flag: --bcc
            parameter: bcc
          - type: passthrough
            parameter: extra_args
    runtime_catalog:
      categories:
      - messaging
      - email
      - agentmail
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 30
      spend:
        type: counted
        commodity: EMAIL_SENDS
        cost_per_action: '1'
---

# AgentMail — Send

Send email as Magican from its own AgentMail inbox. The default `account` is
**work** → magican@agentmail.to; pass `account` to send from another inbox
identity (each has its own inbox-scoped key; aliases are flexible). Reading the
inbox is the separate, free `agentmail-read` tool.

**Cap: 50 sends/day.** Each `send`/`reply`/`forward` debits 1 `EMAIL_SENDS`
against Presto's daily budget. Confirm recipients/subject/body before sending —
a send is metered work, not a free read.

The from-inbox is resolved from `account`, so you normally **don't pass
`inbox_id`** — it's an optional override only.

Inner actions:
- `send` — new message. Required: `to`, `subject`; provide one of
  `text` / `html`. Optional: `cc`, `bcc`, `labels`.
- `reply` — reply to a message by `message_id` with `text`/`html` (set
  `reply_all` to reply to everyone).
- `forward` — forward a message (`message_id`) to new `to` recipients.

Output is JSON.

For an operation not surfaced here, the full reference is
<https://docs.agentmail.to/llms.txt> (overview) and
<https://docs.agentmail.to/llms-full.txt> (complete) — consult ONLY if a needed
send operation isn't in the actions above; do not fetch them routinely.
