---
name: "presto-gmail"
version: 0.1.0
description: "Presto's OWN Gmail (reach.magican@gmail.com) via gws — send, read, triage, reply, forward, search as Presto itself. Hard-pinned to the gws-presto profile; NO account parameter."
metadata:
  magician:
    setup: { definition: google-workspace }
    requires:
      bins: ["gws"]
    install_hint:
      docs: "OAuth setup: CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-presto/cloudsdk GOOGLE_WORKSPACE_CLI_CONFIG_DIR={scope_capability_auth_root}/gws-presto gws auth login -s gmail,sheets,drive,docs,calendar"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          its read-only actions still require a live operator OAuth profile
          and would read the operator's private mailbox, calendar, or
          documents. A canary must not touch private user data.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [gws]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 300
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: cli_profile
        requirement: required
        provider: google-workspace
        profile_selection:
          mode: fixed
          alias: presto
        storage:
          kind: scoped_directory
          namespace: gws
          partition_by_profile: true
        injections:
          - source: {kind: profile_auth_root, path: []}
            target: {kind: environment, name: GOOGLE_WORKSPACE_CLI_CONFIG_DIR}
          - source: {kind: profile_auth_root, path: [cloudsdk]}
            target: {kind: environment, name: CLOUDSDK_CONFIG}
        lifecycle:
          status:
            args: [auth, status]
            interaction: batch
            timeout_secs: 30
          status_observation:
            format: json
            rules:
              - state: ready
                exit_codes: [0]
                all:
                  - {kind: exists, pointer: /user}
          login:
            args: [auth, login, -s, "gmail,sheets,drive,docs,calendar"]
            interaction: pty
            timeout_secs: 300
        identity:
          mode: profile_expected
          selector:
            kind: json_pointer_ascii_case_insensitive
            pointer: /user
      policy_floor:
        approval: conditional_external_side_effect
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        triage:
          description: Summarize unread or query-matched Gmail messages.
          fixed_args: [gmail, +triage]
          timeout_secs: 30
          parameters:
            query:
              type: string
              description: Gmail search query, mapped to --query.
            max:
              type: integer
              description: Maximum messages to triage, mapped to --max.
            label:
              type: string
              description: Restrict to this Gmail label.
            format:
              type: string
              description: Output format.
              enum_values: [json, table, text]
            extra_args:
              type: string_array
              description: Bounded additional argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --query, parameter: query}
            - {type: flag, flag: --max, parameter: max}
            - {type: flag, flag: --label, parameter: label}
            - {type: flag, flag: --format, parameter: format}
            - {type: passthrough, parameter: extra_args}
        send:
          description: Send an email after confirming recipients, subject, and body.
          fixed_args: [gmail, +send]
          timeout_secs: 30
          parameters:
            to:
              type: string
              description: Primary recipients, comma-separated when multiple.
              required: true
            cc:
              type: string
              description: CC recipients, comma-separated when multiple.
            bcc:
              type: string
              description: BCC recipients, comma-separated when multiple.
            subject:
              type: string
              description: Email subject.
              required: true
            body:
              type: string
              description: Email body text.
              required: true
            reply_to_message_id:
              type: string
              description: Message id used for reply headers.
            extra_args:
              type: string_array
              description: Bounded additional argv, including repeated attachment flags.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --to, parameter: to}
            - {type: flag, flag: --cc, parameter: cc}
            - {type: flag, flag: --bcc, parameter: bcc}
            - {type: flag, flag: --subject, parameter: subject}
            - {type: flag, flag: --body, parameter: body}
            - {type: flag, flag: --reply-to-message-id, parameter: reply_to_message_id}
            - {type: passthrough, parameter: extra_args}
        reply:
          description: Reply to one Gmail message.
          fixed_args: [gmail, +reply]
          timeout_secs: 30
          parameters:
            message_id:
              type: string
              description: Gmail message id.
              required: true
            body:
              type: string
              description: Reply body text.
              required: true
            include_original:
              type: boolean
              description: Include the original message in the reply.
            extra_args:
              type: string_array
              description: Bounded additional argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --message-id, parameter: message_id}
            - {type: flag, flag: --body, parameter: body}
            - {type: bool_flag, flag: --include-original, parameter: include_original}
            - {type: passthrough, parameter: extra_args}
        reply_all:
          description: Reply to all recipients of one Gmail message.
          fixed_args: [gmail, +reply-all]
          timeout_secs: 30
          parameters:
            message_id:
              type: string
              description: Gmail message id.
              required: true
            body:
              type: string
              description: Reply body text.
              required: true
            include_original:
              type: boolean
              description: Include the original message in the reply.
            extra_args:
              type: string_array
              description: Bounded additional argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --message-id, parameter: message_id}
            - {type: flag, flag: --body, parameter: body}
            - {type: bool_flag, flag: --include-original, parameter: include_original}
            - {type: passthrough, parameter: extra_args}
        forward:
          description: Forward one Gmail message to explicit recipients.
          fixed_args: [gmail, +forward]
          timeout_secs: 30
          parameters:
            message_id:
              type: string
              description: Source Gmail message id.
              required: true
            to:
              type: string
              description: Forward recipients, comma-separated when multiple.
              required: true
            body:
              type: string
              description: Optional introduction before the forwarded content.
            extra_args:
              type: string_array
              description: Bounded additional argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --message-id, parameter: message_id}
            - {type: flag, flag: --to, parameter: to}
            - {type: flag, flag: --body, parameter: body}
            - {type: passthrough, parameter: extra_args}
        watch:
          description: Stream new Gmail messages when a watcher is explicitly requested.
          fixed_args: [gmail, +watch]
          timeout_secs: 300
          parameters:
            args:
              type: string_array
              description: Optional exact bounded watcher argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        messages_list:
          description: List Gmail messages using exact bounded API argv.
          fixed_args: [gmail, users, messages, list]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        messages_get:
          description: Read one Gmail message by id.
          fixed_args: [gmail, users, messages, get]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        messages_trash:
          description: Trash one exact Gmail message.
          fixed_args: [gmail, users, messages, trash]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        messages_modify:
          description: Add or remove labels on one exact Gmail message.
          fixed_args: [gmail, users, messages, modify]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        labels_list:
          description: List Gmail labels.
          fixed_args: [gmail, users, labels, list]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Optional exact bounded argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        threads_list:
          description: List matching Gmail conversation threads.
          fixed_args: [gmail, users, threads, list]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        threads_get:
          description: Read one Gmail conversation thread.
          fixed_args: [gmail, users, threads, get]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        raw:
          description: Run an unmodeled Gmail command using exact bounded argv tokens.
          fixed_args: [gmail]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        help:
          description: Run bounded Gmail help argv without exposing the root auth namespace.
          fixed_args: [gmail]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Optional exact bounded help argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_status:
          description: Inspect authentication for Presto's fixed account.
          route: auth_status
          fixed_args: [auth, status]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Optional exact bounded status argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_login:
          description: Authenticate Presto's fixed account after explicit user intent.
          route: auth_login
          fixed_args: [auth, login]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Optional exact bounded login argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_setup:
          description: Run first-time OAuth client setup for Presto's fixed account.
          fixed_args: [auth, setup]
          timeout_secs: 30
          parameters:
            args:
              type: string_array
              description: Optional exact bounded setup argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
    runtime_catalog:
      categories: [messaging, email, gmail, communication]
      composition_category: messaging_operations
---

# Presto Gmail (own identity)

Tool name: `presto-gmail`
Inner actions expose Gmail commands directly: `triage`, `send`, `reply`,
`reply_all`, `forward`, `watch`, `messages_list`, `messages_get`,
`messages_trash`, `messages_modify`, `labels_list`, `threads_list`,
`threads_get`, `raw`, `auth_status`, `auth_login`, `auth_setup`, and `help`.
Requires: gws CLI available from the scoped capability bot runtime
Auth: OAuth2 via `gws auth login` (browser flow). One-time setup with `gws auth setup`.

Identity: this skill is HARD-PINNED to Presto's OWN Google Workspace account
(reach.magican@gmail.com, the `gws-presto` profile). There is NO `account` parameter —
every call acts as Presto itself. The owner's accounts (personal/work/business)
are reached through the separate multi-account `gmail` skill, not this one.

To set up a new account:
  CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-presto/cloudsdk GOOGLE_WORKSPACE_CLI_CONFIG_DIR={scope_capability_auth_root}/gws-presto gws auth setup --login

Each action forwards exact argv tokens after its fixed command prefix. Do not
include shell quotes or the `gws` binary name. Use one array element per argv
token.

Helper commands (recommended — simpler interface):

  +send --to <emails> --subject <subject> --body <text>
    Send an email. Use --cc, --bcc, --html as needed.
    Example: +send --to alice@example.com --subject 'Meeting' --body 'See you at 3pm'

  +triage [--max N] [--query <gmail-query>]
    Show unread inbox summary (sender, subject, date).
    Example: +triage --max 10 --query 'from:boss'

  +reply --id <message-id> --body <text>
    Reply to a message (handles threading automatically).
    Example: +reply --id 18f3a2b1c4d5e6f7 --body 'Got it, thanks!'

  +reply-all --id <message-id> --body <text>
    Reply-all to a message.

  +forward --id <message-id> --to <emails>
    Forward a message to new recipients.

  +watch
    Watch for new emails and stream them as NDJSON.

Low-level API commands (full Gmail API access):

  users messages list --params '{"userId": "me", "q": "is:unread"}'
    List messages matching a query.

  users messages get --params '{"userId": "me", "id": "<message-id>"}'
    Get full message content.

  users messages trash --params '{"userId": "me", "id": "<message-id>"}'
    Move a message to trash.

  users messages modify --params '{"userId": "me", "id": "<message-id>"}' --json '{"addLabelIds": ["STARRED"]}'
    Add/remove labels on a message.

  users labels list --params '{"userId": "me"}'
    List all labels.

  users threads list --params '{"userId": "me", "q": "subject:invoice"}'
    List threads matching a query.

  users threads get --params '{"userId": "me", "id": "<thread-id>"}'
    Get full thread with all messages.

Inner-loop operating notes:
- Prefer command-level actions such as `triage`, `messages_list`,
  `messages_get`, `labels_list`, `threads_list`, and `threads_get` over
  `raw`.
- Use `raw` only for a Gmail CLI command not modeled as an action yet.
- Use `help` sparingly. Top-level `gws --help` is local, but service-specific
  help may fetch Google discovery docs; if help returns discoveryError,
  continue from this guide and the command-level action descriptions.
- For low-level API calls, pass `--params` JSON as one argv token. For write
  calls, pass `--json` request bodies as one argv token.
- Use `--page-all` plus `--page-limit` only when the task needs more than one
  page. Start with a narrow Gmail query (`q`) and a small result set.

Safety and verification:
- Before `send`, `reply`, `reply_all`, `forward`, `messages_trash`, or
  `messages_modify`, identify the exact recipient/message/thread/label from
  a read-only result unless the user already supplied it.
- After a send/reply/forward, verify from the CLI result. After label/trash
  changes, re-read the message or list with a focused query when evidence is
  needed.
- If the target message is ambiguous, call `need_user_input` instead of
  guessing.

Tips:
- Use --format json for structured output (default)
- Use --format table for human-readable output
- Gmail search queries work in --query and --params q field (e.g., "from:alice after:2026/03/01")
- Message IDs come from list/triage results

Examples:
- triage {"args":["--format","json"]}
- triage {"args":["--max","5","--query","is:unread from:team","--format","json"]}
- send {"args":["--to","alice@example.com","--subject","Quick update","--body","The deploy is done."]}
- reply {"args":["--id","18f3a2b1c4d5e6f7","--body","Thanks, acknowledged."]}
- forward {"args":["--id","18f3a2b1c4d5e6f7","--to","bob@example.com"]}
- messages_list {"args":["--params","{\"userId\":\"me\",\"q\":\"is:unread\"}","--format","json"]}
- labels_list {"args":["--params","{\"userId\":\"me\"}","--format","json"]}
- help {"args":["+send","--help"]}
- auth_status {"args":["--format","json"]}
