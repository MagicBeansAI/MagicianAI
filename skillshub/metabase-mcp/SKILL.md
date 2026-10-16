---
name: metabase-mcp
version: 0.1.0
description: |
  Toolskill for a Metabase instance's own remote MCP server. Use it to search
  Metabase, read databases, tables, collections, questions, dashboards and
  metrics, construct and run queries or SQL, and create or update Metabase
  questions, dashboards, metrics and collections when asked. Access is the
  signed-in Metabase user's browser OAuth session, so results follow that
  user's Metabase permissions. Each workspace points at its own Metabase:
  setup renders the endpoint from the workspace's METABASE_BASE_URL.
metadata:
  magician:
    setup: { definition: governed-mcp-oauth }
    skill_type: tool
    user_invocable: true
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it needs the workspace's own Metabase instance and a signed-in
          Metabase user; there is no public instance to probe.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      runtime:
        protocol: mcp
        transport:
          kind: streamable_http
          # Placeholder (RFC 2606 reserved). Setup renders each workspace's
          # own SKILL.md with its METABASE_BASE_URL; see render.yaml.
          endpoint: https://metabase.invalid/api/metabase-mcp
        discovery:
          namespace: metabase
          oauth:
            authorization_issuer: https://metabase.invalid
            scopes:
              - agent:search
              - agent:resource:read
              - agent:query
              - agent:query:construct
              - agent:query:execute
              - agent:sql:construct
              - agent:sql:execute
              - agent:question:execute
              - agent:question:create
              - agent:question:update
              - agent:dashboard:create
              - agent:dashboard:update
              - agent:metric:create
              - agent:metric:update
              - agent:collection:create
          # Reads and queries run without a prompt; anything that changes the
          # team's shared Metabase asks every time. Each description replaces
          # the remote one the runtime does not trust, so the agent can pick a
          # tool without reading the whole catalog.
          tool_policies:
            search:
              risk: read_only
              trusted_description: Find tables, models, metrics, questions, dashboards and collections. Pass term_queries (exact words, e.g. ["gmv"]) and semantic_queries (natural phrases); either may be an empty list.
            read_resource:
              risk: read_only
              trusted_description: Read full details of Metabase items by URI (for example a table's fields, a question's query, a metric's definition) from uris returned by search.
            execute_question:
              risk: read_only
              trusted_description: Run an existing saved question or model by its numeric id and return its rows. Prefer this when a saved question already answers the request.
            construct_query:
              risk: read_only
              trusted_description: Build a Metabase query from a natural-language prompt plus a query description; returns a query and query_handle to run with execute_query or query. Does not run anything.
            construct_native_query:
              risk: read_only
              trusted_description: Prepare a SQL query for database_id; returns a query_handle to run with execute_query or query. Does not run anything.
            execute_query:
              risk: read_only
              trusted_description: Run a query prepared by construct_query or construct_native_query (pass its query and query_handle) and return rows.
            query:
              risk: read_only
              trusted_description: Run a prepared query and page through large results; pass continuation_token from the previous page, or an empty string for the first page.
            execute_sql:
              risk: read_only
              trusted_description: Run read-only SQL directly against database_id and return rows. Add filters and a LIMIT; aggregate in SQL rather than pulling raw rows.
            create_question:
              risk: external_side_effect
              trusted_description: Save a new question in the team's shared Metabase. Only when the user asked for a saved question.
            update_question:
              risk: external_side_effect
              trusted_description: Change or archive an existing saved question in the team's shared Metabase. Only when the user asked for that change.
            create_metric:
              risk: external_side_effect
              trusted_description: Save a new metric in the team's shared Metabase. Only when the user asked for it.
            update_metric:
              risk: external_side_effect
              trusted_description: Change or archive an existing metric in the team's shared Metabase. Only when the user asked for that change.
            create_dashboard:
              risk: external_side_effect
              trusted_description: Create a dashboard from saved question ids in the team's shared Metabase. Only when the user asked for it.
            update_dashboard:
              risk: external_side_effect
              trusted_description: Change or archive an existing dashboard in the team's shared Metabase. Only when the user asked for that change.
            create_collection:
              risk: external_side_effect
              trusted_description: Create a collection in the team's shared Metabase. Only when the user asked for it.
        limits:
          timeout_secs: 180
          stdout_bytes: 67108864
          stderr_bytes: 1048576
      auth:
        kind: oauth_session
        requirement: required
        provider: metabase-mcp
        profile_selection:
          mode: selectable
          default: personal
      policy_floor:
        approval: ordinary
        resource_scopes: [external_mcp]
    runtime_catalog:
      categories: [analytics, business_intelligence, data]
      composition_category: data_analysis
      profile_parameter:
        name: profile
        enum_values: [personal]
---

# Metabase MCP

Metabase's built-in MCP server (`<METABASE_BASE_URL>/api/metabase-mcp`). The
governed runtime connects through the official Rust MCP SDK. First use opens
browser OAuth against the workspace's Metabase (the instance is its own
authorization server; the client registers itself, PKCE S256); tokens stay in
the encrypted principal/workspace vault and refresh on their own.

Every call runs as the signed-in Metabase user, so a question or table that
user cannot see in Metabase is not visible here either. Nothing is generated
or built locally: the server lists its own tools.

## Tool Actions

Use `status` once first. `state: expired` with `refresh_available: true` is
normal: the access token is short-lived and the next `list_tools` or
`call_tool` refreshes it on its own, so go straight to the work. Use
`auth_start` only when `status` reports no binding, `refresh_available: false`,
or a call fails with "authorization is required"; the Auth Broker then opens
the user's browser on their Metabase login. Never ask the user for a Metabase
password or token in chat.

- `status`: inspect the scoped OAuth binding without exposing a token.
- `auth_start`: start browser OAuth for the exact profile binding.
- `list_tools`: list the live Metabase MCP tools and input schemas.
- `call_tool`: call a Metabase MCP tool by name with `arguments_json`.
- `clear_auth`: delete scoped OAuth state for the selected profile binding.

## Operating Rules

Prefer searching and reading before running anything: find the existing
question, model or metric that already answers the request, and run that,
rather than writing new SQL against raw tables.

Running a query or SQL reads data but can be expensive on large tables: add a
sensible row limit and filters, and say what you ran when you report a result.

Creating or updating a question, dashboard, metric or collection changes the
team's shared Metabase. Only do it when the user asked for that change, name
the asset and its collection in `intent_summary`, and report the link or id of
what was created. Never delete or overwrite someone else's asset without an
explicit request naming it.

A newly listed tool without an exact local rule receives conservative
one-time approval: the runtime does not trust remote tool annotations.

## Failure Modes

- `status` reports no binding, or `refresh_available: false`: run
  `auth_start` and complete the Metabase login in the browser. An expired
  token with a refresh available needs nothing; just make the call.
- Tool returns permission denied: the signed-in Metabase user lacks access;
  say which collection or database is involved rather than retrying.
- Skill missing from a workspace: that workspace has no METABASE_BASE_URL in
  its `skills/metabase-mcp/config/.env`; setup renders the manifest once it is
  set.
