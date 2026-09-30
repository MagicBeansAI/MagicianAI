---
name: "calendar"
version: 0.1.0
description: "Google Calendar via Google Workspace CLI (gws) — view agendas, list calendars, and create or update events across multiple accounts"
metadata:
  magician:
    setup: { definition: google-workspace }
    requires:
      bins: ["gws"]
    install_hint:
      docs: "OAuth setup: CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-{account}/cloudsdk GOOGLE_WORKSPACE_CLI_CONFIG_DIR={scope_capability_auth_root}/gws-{account} gws auth login -s gmail,sheets,drive,docs,calendar"
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
          timeout_secs: 30
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: cli_profile
        requirement: required
        provider: google-workspace
        profile_selection:
          mode: selectable
          default: work
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
        agenda:
          description: Show upcoming Calendar events with typed filters.
          fixed_args: [calendar, +agenda]
          parameters:
            today:
              type: boolean
              description: Restrict to today's events.
            days:
              type: integer
              description: Look-ahead window in days.
            calendar_id:
              type: string
              description: Calendar id; omit for primary.
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
            - {type: bool_flag, flag: --today, parameter: today}
            - {type: flag, flag: --days, parameter: days}
            - {type: flag, flag: --calendar, parameter: calendar_id}
            - {type: flag, flag: --format, parameter: format}
            - {type: passthrough, parameter: extra_args}
        events_list:
          description: List upcoming or matching Calendar events.
          fixed_args: [calendar, events, list]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        events_get:
          description: Read one Calendar event by id.
          fixed_args: [calendar, events, get]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        events_insert:
          description: Create a Calendar event with explicit params and payload.
          fixed_args: [calendar, events, insert]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        events_patch:
          description: Patch one exact Calendar event.
          fixed_args: [calendar, events, patch]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        events_delete:
          description: Delete one exact Calendar event.
          fixed_args: [calendar, events, delete]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        calendars_get:
          description: Inspect Calendar metadata such as timezone and summary.
          fixed_args: [calendar, calendars, get]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        calendar_list:
          description: List calendars available to the selected account.
          fixed_args: [calendar, calendarList, list]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        raw:
          description: Run an unmodeled Calendar command with exact bounded argv.
          fixed_args: [calendar]
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
          description: Run bounded Calendar help without exposing root auth routes.
          fixed_args: [calendar]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded help argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_status:
          description: Inspect authentication for the selected account.
          route: auth_status
          fixed_args: [auth, status]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded status argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_login:
          description: Authenticate the selected account after explicit user intent.
          route: auth_login
          fixed_args: [auth, login]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded login argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_setup:
          description: Run first-time OAuth client setup for the selected account.
          fixed_args: [auth, setup]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded setup argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
    runtime_catalog:
      categories: [productivity, calendar, scheduling, meetings]
      composition_category: calendar_operations
      profile_parameter:
        name: account
        enum_values: [business, personal, work]
---

# Calendar

Tool name: `calendar`
Inner actions expose Calendar commands directly: `agenda`, `events_list`,
`events_get`, `events_insert`, `events_patch`, `events_delete`,
`calendars_get`, `calendar_list`, `raw`, `auth_status`, `auth_login`,
`auth_setup`, and `help`.
Requires: gws CLI available from the scoped capability bot runtime
Auth: OAuth2 via `gws auth login` (browser flow). One-time setup with `gws auth setup`.

Multi-account support: use the `account` parameter to select which Google account
to use. Each account has its own OAuth credentials stored in
{scope_capability_auth_root}/gws-<account>/. Default account is "work".

To set up a new account:
  CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-<account>/cloudsdk GOOGLE_WORKSPACE_CLI_CONFIG_DIR={scope_capability_auth_root}/gws-<account> gws auth setup --login

Each action forwards exact argv tokens after its fixed command prefix. Do not
include shell quotes or the `gws` binary name. Use one array element per argv
token.

Helper commands (recommended — simpler interface):

  +agenda [--today] [--timezone <IANA-tz>]
    Show upcoming events. Uses the Google account timezone by default.
    Example: +agenda --today
    Example: +agenda --today --timezone America/New_York

  +insert
    Create a new event using the built-in calendar helper.
    Use low-level `events insert` when you need precise JSON payload control.

Low-level API commands (full Calendar API v3 access):

  events list --params '{"calendarId": "primary", "maxResults": 10, "singleEvents": true, "orderBy": "startTime"}'
    List upcoming events from a calendar.

  events get --params '{"calendarId": "primary", "eventId": "<event-id>"}'
    Get one event.

  events insert --params '{"calendarId": "primary"}' --json '{"summary": "Design review", "start": {"dateTime": "2026-03-18T10:00:00+05:30"}, "end": {"dateTime": "2026-03-18T10:30:00+05:30"}}'
    Create an event with explicit JSON payload.

  events patch --params '{"calendarId": "primary", "eventId": "<event-id>"}' --json '{"summary": "Updated title"}'
    Update selected fields on an existing event.

  events delete --params '{"calendarId": "primary", "eventId": "<event-id>"}'
    Delete an event.

  calendars get --params '{"calendarId": "primary"}'
    Get metadata for a calendar, including timezone and summary.

  calendarList list
    List calendars available to the account.

Inner-loop operating notes:
- Prefer command-level actions such as `agenda`, `events_list`,
  `events_get`, `events_insert`, `events_patch`, `events_delete`,
  `calendars_get`, and `calendar_list` over `raw`.
- Use `raw` only for a Calendar CLI command not modeled as an action yet.
- Use `help` sparingly. Top-level `gws --help` is local, but service-specific
  help may fetch Google discovery docs; if help returns discoveryError,
  continue from this guide and the action descriptions.
- For low-level API calls, pass `--params` JSON as one argv token. For create
  or patch calls, pass `--json` request bodies as one argv token.
- Use `--page-all` plus `--page-limit` only when the task needs more than one
  page. Start with a calendar id and a bounded time/result window.

Safety and verification:
- Before `events_insert`, inspect the relevant calendar/time context when the
  user has not fully specified it.
- Before `events_patch` or `events_delete`, identify the exact event id from
  `events_list` or `events_get`.
- Datetimes should be RFC3339 with timezone offsets whenever possible. If the
  timezone is unknown, inspect `calendars_get` or ask the user.
- After create/update/delete, verify from the CLI result or a focused
  `events_get`/`events_list` when durable evidence is needed.

Tips:
- Use --format json for structured output (default)
- Use --format table for human-readable output
- For upcoming events, prefer `singleEvents: true` with `orderBy: "startTime"`
- Datetimes should be RFC3339 with timezone offsets when possible
- Use `primary` as `calendarId` unless you need a shared or secondary calendar
- Event IDs come from `events list` results

Examples:
- agenda {"args":["--today","--format","json"]}
- agenda {"args":["--today","--timezone","America/New_York","--format","json"],"account":"work"}
- events_list {"args":["--params","{\"calendarId\":\"primary\",\"maxResults\":10,\"singleEvents\":true,\"orderBy\":\"startTime\"}","--format","json"]}
- events_insert {"args":["--params","{\"calendarId\":\"primary\"}","--json","{\"summary\":\"1:1 with Alice\",\"start\":{\"dateTime\":\"2026-03-18T15:00:00+05:30\"},\"end\":{\"dateTime\":\"2026-03-18T15:30:00+05:30\"}}"],"account":"personal"}
- events_patch {"args":["--params","{\"calendarId\":\"primary\",\"eventId\":\"abc123\"}","--json","{\"summary\":\"Moved meeting\"}"],"account":"business"}
- help {"args":["events","list","--help"]}
- auth_status {"args":["--format","json"]}
