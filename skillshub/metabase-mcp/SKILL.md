---
name: metabase-mcp
version: 0.2.1
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
    browser_signin:
      callback_mode: public_when_available
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
              - agent:content:read
              - agent:content:write
              - agent:query:run
              - agent:sql:run
              - agent:delivery:write
          # Remote annotations do not grant authority. SQL can write data.
          tool_policies:
            search:
              risk: read_only
              trusted_description: Find Metabase content by keywords or meaning.
            browse_collection:
              risk: read_only
              trusted_description: List collection contents or its tree.
            browse_data:
              risk: read_only
              trusted_description: Inspect databases, schemas, tables, models and fields.
            get_content:
              risk: read_only
              trusted_description: Read content details and requested definition or fields.
            get_parameter_values:
              risk: read_only
              trusted_description: Read permitted values for a saved filter.
            glossary:
              risk: read_only
              trusted_description: Look up business definitions.
            learn:
              risk: read_only
              trusted_description: Read server guidance for a topic.
            run_saved_question:
              risk: read_only
              trusted_description: Run a saved question with bounded rows.
            execute_query:
              risk: read_only
              trusted_description: Run or validate a structured query; follow its cursor for more rows.
            render_drill_through:
              risk: read_only
              trusted_description: Read a permitted drill-through result.
            visualize_query:
              risk: read_only
              trusted_description: Render a query result without saving content.
            execute_sql:
              risk: external_side_effect
              trusted_description: Run raw SQL. It can change data; obtain approval even for a proposed read.
            question_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a question or model when requested.
            dashboard_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a dashboard when requested.
            collection_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a collection when requested.
            document_write:
              risk: external_side_effect
              trusted_description: Create, edit or archive a document when requested.
            metric_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a metric when requested.
            measure_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a measure when requested.
            segment_write:
              risk: external_side_effect
              trusted_description: Create, update or archive a segment when requested.
            bookmark_content:
              risk: external_side_effect
              trusted_description: Change a content bookmark when requested.
            duplicate_content:
              risk: external_side_effect
              trusted_description: Copy content into a collection when requested.
            alert_write:
              risk: external_side_effect
              trusted_description: Create or change an alert, its recipients and schedule only when requested.
            subscription_write:
              risk: external_side_effect
              trusted_description: Create or change scheduled delivery only when requested.
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

Structured queries and saved questions read data but can be expensive: add a
row limit and filters. Raw SQL is approval-gated because it can modify data.
Use parameter bindings for caller values, and report what ran.

Content writes, bookmarks, copies, alerts and subscriptions change the
team's shared Metabase or deliver its data. Only do it when the user asked for that change, name
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

Use the live schemas from `list_tools` for arguments. Current tools include
`search`, `browse_data`, `get_content`, `run_saved_question`, `execute_query`,
`execute_sql` and the `*_write` tools. Read details with
`get_content {items: [{type: "question", id: 123}], include: ["definition"]}`;
save an approved result with `question_write {method: "create", name: "…",
query_handle: "…"}`. `execute_query` pages with `cursor` from `next_cursor`.
