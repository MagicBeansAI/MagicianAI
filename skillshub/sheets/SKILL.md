---
name: "sheets"
version: 0.1.1
description: "Collaborative cloud Google Sheets via Google Workspace CLI (gws) — read, write, append, clear, and manage spreadsheets"
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
        read:
          description: Read values from a range in a cloud Google Sheet.
          fixed_args: [sheets, +read]
          parameters:
            spreadsheet:
              type: string
              description: Spreadsheet id.
              required: true
            range:
              type: string
              description: A1-notation range.
              required: true
            format:
              type: string
              description: Output format.
              enum_values: [json, csv, table]
            extra_args:
              type: string_array
              description: Bounded additional argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --spreadsheet, parameter: spreadsheet}
            - {type: flag, flag: --range, parameter: range}
            - {type: flag, flag: --format, parameter: format}
            - {type: passthrough, parameter: extra_args}
        append:
          description: Append rows to an explicit spreadsheet range using the native Sheets API.
          fixed_args: [sheets, spreadsheets, values, append]
          parameters:
            params:
              type: string
              description: JSON object containing spreadsheetId, range (A1 notation), and valueInputOption (RAW or USER_ENTERED).
              required: true
              max_length: 4096
            json:
              type: string
              description: JSON ValueRange body containing values as an array of rows, optionally majorDimension (ROWS or COLUMNS). At most 4 KB (the runtime's per-argument limit); append larger data in several calls.
              required: true
              max_length: 4096
            extra_args:
              type: string_array
              description: Bounded additional supported gws argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings:
            - {type: flag, flag: --params, parameter: params}
            - {type: flag, flag: --json, parameter: json}
            - {type: passthrough, parameter: extra_args}
        spreadsheets_get:
          description: Inspect spreadsheet metadata and tab ids.
          fixed_args: [sheets, spreadsheets, get]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        spreadsheets_create:
          description: Create a cloud Google Sheet.
          fixed_args: [sheets, spreadsheets, create]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        spreadsheets_batch_update:
          description: Apply structural, formatting, validation, chart, or tab changes.
          fixed_args: [sheets, spreadsheets, batchUpdate]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_get:
          description: Read one spreadsheet range through the low-level API.
          fixed_args: [sheets, spreadsheets, values, get]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_update:
          description: Write values to one spreadsheet range.
          fixed_args: [sheets, spreadsheets, values, update]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_batch_get:
          description: Read multiple spreadsheet ranges.
          fixed_args: [sheets, spreadsheets, values, batchGet]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_batch_update:
          description: Write multiple spreadsheet ranges.
          fixed_args: [sheets, spreadsheets, values, batchUpdate]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_clear:
          description: Clear one spreadsheet range while retaining formatting.
          fixed_args: [sheets, spreadsheets, values, clear]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_batch_clear:
          description: Clear multiple spreadsheet ranges while retaining formatting.
          fixed_args: [sheets, spreadsheets, values, batchClear]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        values_append:
          description: Append rows through the low-level Sheets API.
          fixed_args: [sheets, spreadsheets, values, append]
          parameters:
            args:
              type: string_array
              description: Exact bounded argv tokens.
              required: true
              min_items: 1
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        copy_to:
          description: Copy one Sheet tab into another spreadsheet.
          fixed_args: [sheets, spreadsheets, sheets, copyTo]
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
          description: Run an unmodeled Sheets command using exact bounded argv tokens.
          fixed_args: [sheets]
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
          description: Run bounded Sheets help argv without exposing the root auth namespace.
          fixed_args: [sheets]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded argv tokens.
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
              description: Optional exact bounded argv tokens.
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
              description: Optional exact bounded argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
        auth_setup:
          description: Run first-time OAuth client setup for the selected account.
          fixed_args: [auth, setup]
          parameters:
            args:
              type: string_array
              description: Optional exact bounded argv tokens.
              max_items: 32
              max_item_bytes: 1024
          mappings: [{type: passthrough, parameter: args}]
    runtime_catalog:
      categories: [spreadsheet, google-sheets, google-workspace, cloud, cloud-data, collaboration, tabular]
      composition_category: data_operations
      profile_parameter:
        name: account
        enum_values: [business, personal, work]
---

# Sheets

Tool name: `sheets`
Inner actions expose Sheets commands directly: `read`, `append`,
`spreadsheets_get`, `spreadsheets_create`, `spreadsheets_batch_update`,
`values_get`, `values_update`, `values_batch_get`, `values_batch_update`,
`values_clear`, `values_batch_clear`, `values_append`, `copy_to`, `raw`, `auth_status`,
`auth_login`, `auth_setup`, and `help`.
Requires: gws CLI available from the scoped capability bot runtime
Auth: OAuth2 via `gws auth login` (browser flow). One-time setup with `gws auth setup`.

Multi-account support: use the `account` parameter to select which Google account
to use. Each account has its own OAuth credentials stored in
{scope_capability_auth_root}/gws-<account>/. Default account is "work".

To set up a new account:
  CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-<account>/cloudsdk GOOGLE_WORKSPACE_CLI_CONFIG_DIR={scope_capability_auth_root}/gws-<account> gws auth setup --login

`read` takes named parameters. `append` takes native JSON strings `params`
(spreadsheetId, range, valueInputOption) and `json` (values as rows), preserving
the selected tab and A1 range. Other actions accept argv arrays after their
fixed prefix; omit the `gws` binary name and shell quotes.

Use Sheets when the user wants a cloud spreadsheet as the working surface or
final deliverable: reading existing sheets, publishing rows, appending logs,
clearing/replacing ranges, formatting tabs, adding charts, or sharing a
spreadsheet-shaped result. This skill focuses on Sheets operations; broader
analysis choices belong to the caller.

Helper commands (recommended — simpler interface):

  +read --spreadsheet <ID> --range <RANGE>
    Read values from a spreadsheet range.
    Example: +read --spreadsheet 1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms --range 'Sheet1!A1:D10'

The typed `append` action uses `spreadsheets values append`, not the
range-less `+append` helper. Supply `valueInputOption: "RAW"` for literal data
or `"USER_ENTERED"` when Sheets should interpret formulas and formatted values.
Append large datasets in batches (the JSON body is bounded to 32,768 characters).

Low-level API commands (full Sheets API v4 access):

  spreadsheets get --params '{"spreadsheetId": "<ID>"}'
    Get spreadsheet metadata (sheets, titles, grid properties).

  spreadsheets create --json '{"properties": {"title": "My Sheet"}}'
    Create a new spreadsheet.

  spreadsheets batchUpdate --params '{"spreadsheetId": "<ID>"}' --json '{"requests": [...]}'
    Apply batch updates: add/delete sheets, merge cells, format cells,
    add charts, set data validation, conditional formatting, etc.

  spreadsheets values get --params '{"spreadsheetId": "<ID>", "range": "Sheet1!A1:D10"}'
    Read a range of values.

  spreadsheets values update --params '{"spreadsheetId": "<ID>", "range": "Sheet1!A1", "valueInputOption": "USER_ENTERED"}' --json '{"values": [["a","b"],["c","d"]]}'
    Write values to a range.

  spreadsheets values batchGet --params '{"spreadsheetId": "<ID>", "ranges": ["Sheet1!A1:B2","Sheet2!A1:A5"]}'
    Read multiple ranges in one call.

  spreadsheets values batchUpdate --params '{"spreadsheetId": "<ID>", "valueInputOption": "USER_ENTERED"}' --json '{"data": [{"range": "Sheet1!A1", "values": [["x"]]}]}'
    Write to multiple ranges in one call.

  spreadsheets values clear --params '{"spreadsheetId": "<ID>", "range": "Sheet1!A1:D10"}'
    Clear values from a range (keeps formatting).

  spreadsheets values batchClear --params '{"spreadsheetId": "<ID>"}' --json '{"ranges": ["Sheet1!A1:D10", "Sheet2!A1:B5"]}'
    Clear values from multiple ranges (keeps formatting).

  spreadsheets values append --params '{"spreadsheetId": "<ID>", "range": "Sheet1!A1", "valueInputOption": "USER_ENTERED"}' --json '{"values": [["new","row"]]}'
    Append rows after the last row with data in the range.

  spreadsheets sheets copyTo --params '{"spreadsheetId": "<ID>", "sheetId": 0}' --json '{"destinationSpreadsheetId": "<OTHER_ID>"}'
    Copy a sheet to another spreadsheet.

Inner-loop operating notes:
- Prefer command-level actions such as `read`, `append`,
  `spreadsheets_get`, `values_get`, `values_update`, `values_clear`,
  `values_batch_clear`, `values_append`, and `spreadsheets_batch_update` over
  `raw`.
- Use `raw` only for a Sheets CLI command not modeled as an action yet.
- Use `help` sparingly. Top-level `gws --help` is local, but service-specific
  help may fetch Google discovery docs; if help returns discoveryError,
  continue from this guide and the action descriptions.
- For low-level API calls, pass `--params` JSON as one argv token. For writes
  and batch updates, pass `--json` request bodies as one argv token.
- Inspect spreadsheet metadata (`spreadsheets_get`) when sheet titles,
  sheet ids, grid properties, or ranges are uncertain.
- For publication workflows, create or inspect the spreadsheet, write headers
  and data with `values_update`/`values_batch_update`, then apply formatting or
  charts with `spreadsheets_batch_update`.
- For replacement workflows, clear only the intended ranges before writing new
  values; do not clear whole sheets unless explicitly requested.

Safety and verification:
- Before writes, clears, appends, copy operations, or batch updates, verify
  the spreadsheet id, sheet/range, and intended account unless the user
  supplied exact values.
- Prefer `values_get` or `read` for the smallest range that proves current
  state. Do not dump entire sheets unless required.
- After write/append/clear/batchUpdate, verify with the smallest affected
  range or metadata field that proves the update.
- Preserve formulas and formatting unless the task explicitly asks to replace
  them. `values_clear` clears values only; structural deletion/format changes
  require `spreadsheets_batch_update`.

Tips:
- Use --format json for structured output (default)
- Use --format table for human-readable output
- Use --format csv to get raw CSV output
- Spreadsheet IDs are found in the URL: docs.google.com/spreadsheets/d/<ID>/edit
- Range notation: 'Sheet1!A1:D10', 'Sheet1' (whole sheet), 'A1:B2' (default sheet)
- valueInputOption: "USER_ENTERED" (parses formulas/numbers) or "RAW" (literal strings)

Examples:
- read {"spreadsheet":"<ID>","range":"Sheet1!A1:D10","format":"json"}
- append {"params":"{\"spreadsheetId\":\"<ID>\",\"range\":\"Sheet2!A:D\",\"valueInputOption\":\"RAW\"}","json":"{\"values\":[[\"Alice\",100],[\"Bob\",200]]}"}
- spreadsheets_get {"args":["--params","{\"spreadsheetId\":\"1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms\"}","--format","json"]}
- values_clear {"args":["--params","{\"spreadsheetId\":\"1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms\",\"range\":\"Sheet1!A1:D10\"}","--format","json"]}
- values_batch_clear {"args":["--params","{\"spreadsheetId\":\"1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms\"}","--json","{\"ranges\":[\"Sheet1!A1:D10\",\"Sheet2!A1:B5\"]}","--format","json"]}
- help {"args":["+read","--help"]}
- auth_status {"args":["--format","json"]}
