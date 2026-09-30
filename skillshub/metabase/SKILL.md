---
name: metabase
version: 0.2.0
description: 'Metabase access: discover databases, collections, dashboards, cards, models, tables, fields, snippets,
  and metrics; inspect saved SQL; run saved or ad-hoc queries; export results; and create or update Metabase-native
  assets when requested.'
metadata:
  magician:
    requires:
      bins:
      - metabase-pp-cli
      env:
      - METABASE_BASE_URL
      - METABASE_API_KEY
    install_hint:
      docs: 'requires binary on PATH: metabase-pp-cli'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it requires a reachable Metabase deployment with the operator's
          API key; there is no public instance to probe.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - metabase-pp-cli
      runtime:
        protocol: cli
        command_prefix:
        - --json
        - --no-input
        - --no-color
        - --yes
        interaction: batch
        stdin:
          mode: optional
          sensitivity: private
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 180
          stdin_bytes: 1048576
          stdout_bytes: 67108864
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
        - name: metabase_base_url
          secret_ref: METABASE_BASE_URL
        - name: metabase_api_key
          secret_ref: METABASE_API_KEY
        injections:
        - source:
            kind: secret
            binding: metabase_base_url
          target:
            kind: environment
            name: METABASE_BASE_URL
        - source:
            kind: secret
            binding: metabase_api_key
          target:
            kind: environment
            name: METABASE_API_KEY
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        database_list:
          description: Run `database list` - list connected databases.
          fixed_args:
          - database
          - list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_get:
          description: Run `database get <id>` - single database details.
          fixed_args:
          - database
          - get
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_metadata:
          description: Run `database metadata database <id>` — tables + columns + types for a database.
          fixed_args:
          - database
          - metadata
          - database
          parameters:
            database_id:
              type: integer
              description: Numeric Metabase database id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: database_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        database_schemas:
          description: Run `database schemas database <id>` - list schema names in a database.
          fixed_args:
          - database
          - schemas
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_schema:
          description: Run `database schema database <id> <schema-name>` - tables in one schema.
          fixed_args:
          - database
          - schema
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_fields:
          description: Run `database fields database <id>` - flat list of all fields across the database.
          fixed_args:
          - database
          - fields
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_list:
          description: Run `collection list` - list collections.
          fixed_args:
          - collection
          - list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_tree:
          description: Run `collection tree` - full collection hierarchy. Best overview of how content is organized.
          fixed_args:
          - collection
          - tree
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_get:
          description: Run `collection get <id>` - one collection's metadata.
          fixed_args:
          - collection
          - get
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_root:
          description: Run `collection root` - the root collection.
          fixed_args:
          - collection
          - root
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_root_items:
          description: Run `collection root-items` - items at the top level.
          fixed_args:
          - collection
          - root-items
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_items:
          description: Run `collection items collection <id>` — items inside a collection.
          fixed_args:
          - collection
          - items
          - collection
          parameters:
            collection_id:
              type: integer
              description: Numeric Metabase collection id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: collection_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        dashboard_list:
          description: Run `dashboard list` - list dashboards.
          fixed_args:
          - dashboard
          - list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_get:
          description: Run `dashboard get <id>` — dashboard details + the cards on it.
          fixed_args:
          - dashboard
          - get
          parameters:
            dashboard_id:
              type: integer
              description: Numeric Metabase dashboard id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: dashboard_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        dashboard_items:
          description: Run `dashboard items dashboard <id>` - items on a dashboard (cards, tabs, links).
          fixed_args:
          - dashboard
          - items
          - dashboard
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_query_metadata:
          description: Run `dashboard query-metadata dashboard <id>` - query metadata across all dashboard cards.
          fixed_args:
          - dashboard
          - query-metadata
          - dashboard
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        find_search:
          description: 'Run `metabase-search` — keyword search across Metabase entities.

            Almost always the right first call when you don''t yet know where

            data lives. The `q` parameter is required (the schema enforces

            this) — the old "missing query" failure is impossible now.


            "card" is the legacy name for "dataset" — use "dataset". Saved

            questions are returned under `model: "dataset"`.

            '
          fixed_args:
          - metabase-search
          parameters:
            q:
              type: string
              description: Free-text search query (required).
              required: true
              max_length: 4096
            models:
              type: string
              description: 'Comma-separated entity types to restrict the search to. Whole

                value is one string; do NOT split by entity. Canonical values

                (anything else → HTTP 400): dashboard, table, dataset, segment,

                collection, measure, transform, document, database, action,

                indexed-entity.'
              max_length: 4096
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens for flags this schema does not yet surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            parameter: q
            flag: --q
          - type: flag
            parameter: models
            flag: --models
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        card_list:
          description: 'Run `card list` — list saved questions. Returns ~5k items on this

            instance; trim with `--select id,name,collection_id,database_id,updated_at`

            to reduce payload.


            There is NO `-f` / `--filter` / `f` flag on `card list`. To find

            cards by name fragment use `metabase-search --q <fragment>

            --models dataset` (saved cards are model="dataset").

            '
          fixed_args:
          - card
          - list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_get:
          description: Run `card get <id>` — saved question details (SQL, parameters, result_metadata). Always inspect
            before card_run.
          fixed_args:
          - card
          - get
          parameters:
            card_id:
              type: integer
              description: Numeric Metabase card (saved-question) id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: card_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        card_create:
          description: Run `card create --stdin` - save a new SQL question/card. stdin must include name, display,
            type, and dataset_query.
          fixed_args:
          - card
          - create
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        card_update:
          description: Run `card update <id> --stdin` - update an existing saved question/card, including dataset_query
            SQL.
          fixed_args:
          - card
          - update
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        card_run:
          description: 'Run `card query card-run <card-id>` — execute a saved question

            and return rows. Fastest path when a card already answers the

            question.


            If you get HTTP 400 "Error preprocessing query" or HTTP 403

            "missing-required-permissions", the card''s underlying database

            is not readable with the current API key. Inspect via

            `card_get` (look at `dataset_query.database`), then either

            request the missing grant or pivot to `dataset_query` against a

            database the key does have access to (use `database_list` to see

            which).

            '
          fixed_args:
          - card
          - query
          - card-run
          parameters:
            card_id:
              type: integer
              description: Numeric Metabase card id to execute.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: card_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 120
          stdin: denied
          suffix_args:
          - --compact
        card_run_with_body:
          description: Run `card query card-run <card-id> --stdin` - execute a parameterized saved question. Provide
            stdin JSON with parameters or other query options.
          fixed_args:
          - card
          - query
          - card-run
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
          - --compact
        card_export:
          description: 'Run `card query card-export <card-id> --export-format <csv|json|xlsx|api>`

            — execute a saved question and return exported bytes. Use for

            large results.


            Same permission caveat as `card_run`: HTTP 400 "Error preprocessing

            query" or 403 means the card''s `dataset_query.database` is not

            readable with this key. Inspect via `card get <id>` and pivot to

            `dataset query --stdin` against an accessible database if needed.

            '
          fixed_args:
          - card
          - query
          - card-export
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: denied
        card_export_with_body:
          description: Run `card query card-export <card-id> --export-format <csv|json|xlsx|api> --stdin` - export
            a parameterized saved question. Provide stdin JSON with parameters or format options.
          fixed_args:
          - card
          - query
          - card-export
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_query:
          description: 'Run `dataset query --stdin` — execute ad-hoc native SQL or MBQL.

            Provide stdin JSON: {"database":<id>,"type":"native","native":{"query":"SELECT ..."}}.


            The `database` id must be one this API key has read access to.

            A 403 "missing-required-permissions" means the key cannot read

            that database — list accessible dbs via `database list --agent`

            and pivot to one that holds the same / replicated data.

            '
          fixed_args:
          - dataset
          - query
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
          - --compact
        dataset_export:
          description: Run `dataset export --export-format <csv|json|xlsx|api> --stdin` - execute ad-hoc query and
            stream exported bytes. Use for large result sets.
          fixed_args:
          - dataset
          - export
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 180
          stdin: required
          suffix_args:
          - --stdin
        dataset_to_native:
          description: Run `dataset to-native --stdin` - convert MBQL to native SQL without executing. Useful for
            seeing the SQL Metabase would run.
          fixed_args:
          - dataset
          - to-native
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: required
          suffix_args:
          - --stdin
        doctor:
          description: Run `doctor` - auth + connectivity check. Use first if any other action fails unexpectedly.
          fixed_args:
          - doctor
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 30
          stdin: denied
        raw:
          description: 'Escape hatch: run exact argv tokens through metabase-pp-cli (no fixed prefix). Use only
            when no native action fits.'
          fixed_args: []
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: optional
        help:
          description: Print metabase-pp-cli help for a subcommand or the top-level CLI.
          fixed_args: []
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 15
          stdin: denied
        card_copy:
          description: Run `card copy card <id>` - copy a saved question/card. Use args for the source card id.
          fixed_args:
          - card
          - copy
          - card
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: denied
        card_copy_with_body:
          description: Run `card copy card <id> --stdin` - copy a card while supplying JSON options such as name
            or collection placement.
          fixed_args:
          - card
          - copy
          - card
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        card_dashboards:
          description: Run `card dashboards card <id>` - list dashboards where a card appears.
          fixed_args:
          - card
          - dashboards
          - card
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_query_metadata:
          description: Run `card query-metadata card <id>` - metadata needed to understand or safely edit a saved
            question.
          fixed_args:
          - card
          - query-metadata
          - card
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_series:
          description: Run `card series card <id>` - compatible series/cards for combining visualizations.
          fixed_args:
          - card
          - series
          - card
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_param_values:
          description: Run `card params card-values <card-id> <param-key>` - possible values for a saved-card parameter.
          fixed_args:
          - card
          - params
          - card-values
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_param_search:
          description: Run `card params card-search <card-id> <param-key> <query>` - search possible values for
            a saved-card parameter.
          fixed_args:
          - card
          - params
          - card-search
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_param_remapping:
          description: Run `card params card-remapping <id> <param-key> --value <value>` - display/remapped value
            for a saved-card parameter.
          fixed_args:
          - card
          - params
          - card-remapping
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        card_collections:
          description: Run `card collections --stdin` - bulk move cards into a collection using JSON body.
          fixed_args:
          - card
          - collections
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        cards_dashboards:
          description: Run `cards dashboards --stdin` - find dashboards containing multiple cards.
          fixed_args:
          - cards
          - dashboards
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        cards_move:
          description: Run `cards move --stdin` - move cards to a collection or dashboard.
          fixed_args:
          - cards
          - move
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_query_metadata:
          description: Run `dataset query-metadata --stdin` - fetch metadata needed to run or validate an ad-hoc
            query.
          fixed_args:
          - dataset
          - query-metadata
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_parameter_values:
          description: Run `dataset parameter-values --stdin` - possible values for an ad-hoc/dashboard/card parameter.
          fixed_args:
          - dataset
          - parameter-values
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_parameter_search:
          description: Run `dataset parameter-search <query> --stdin` - search parameter values for an ad-hoc/dashboard/card
            parameter.
          fixed_args:
          - dataset
          - parameter-search
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_parameter_remapping:
          description: Run `dataset parameter-remapping --stdin` - remapped display values for a parameter.
          fixed_args:
          - dataset
          - parameter-remapping
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dataset_pivot:
          description: Run `dataset pivot --stdin` - pivot an ad-hoc query result.
          fixed_args:
          - dataset
          - pivot
          parameters: {}
          mappings: []
          timeout_secs: 180
          stdin: required
          suffix_args:
          - --stdin
        database_autocomplete_suggestions:
          description: Run `database autocomplete-suggestions database <id>` - SQL autocomplete suggestions for
            a database.
          fixed_args:
          - database
          - autocomplete-suggestions
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_card_autocomplete_suggestions:
          description: Run `database card-autocomplete-suggestions database <id>` - autocomplete saved-card references
            for a database.
          fixed_args:
          - database
          - card-autocomplete-suggestions
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_schema_list:
          description: Run `database schema database-list <id>` - schema/table listing endpoint for one database.
          fixed_args:
          - database
          - schema
          - database-list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_idfields:
          description: Run `database idfields database <id>` - fields Metabase considers identifier fields.
          fixed_args:
          - database
          - idfields
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        database_usage_info:
          description: Run `database usage-info database <id>` - database usage metadata.
          fixed_args:
          - database
          - usage-info
          - database
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_create:
          description: Run `collection create --stdin` - create a collection with JSON body.
          fixed_args:
          - collection
          - create
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        collection_update:
          description: Run `collection update <id> --stdin` - update/archive/move a collection.
          fixed_args:
          - collection
          - update
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        collection_graph:
          description: Run `collection graph` - collection permissions graph.
          fixed_args:
          - collection
          - graph
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_update_graph:
          description: Run `collection update-graph --stdin` - batch update collection permissions graph. Use only
            when explicitly requested.
          fixed_args:
          - collection
          - update-graph
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        collection_trash:
          description: Run `collection trash` - fetch the trash collection.
          fixed_args:
          - collection
          - trash
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_dashboard_question_candidates:
          description: Run `collection dashboard-question-candidates collection <id>` - cards that can be moved
            into dashboards in a collection.
          fixed_args:
          - collection
          - dashboard-question-candidates
          - collection
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_move_dashboard_question_candidates:
          description: Run `collection move-dashboard-question-candidates collection <id> --stdin` - move candidate
            cards to dashboards in a collection.
          fixed_args:
          - collection
          - move-dashboard-question-candidates
          - collection
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        collection_root_dashboard_question_candidates:
          description: Run `collection root-dashboard-question-candidates` - root cards that can be moved into dashboards.
          fixed_args:
          - collection
          - root-dashboard-question-candidates
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        collection_root_move_dashboard_question_candidates:
          description: Run `collection root-move-dashboard-question-candidates --stdin` - move root candidate cards
            to dashboards.
          fixed_args:
          - collection
          - root-move-dashboard-question-candidates
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_create:
          description: Run `dashboard create --stdin` - create a dashboard using JSON body.
          fixed_args:
          - dashboard
          - create
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_update:
          description: Run `dashboard update <id> --stdin` - update dashboard metadata, tabs, parameters, or dashcards.
          fixed_args:
          - dashboard
          - update
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_update_cards:
          description: Run `dashboard cards dashboard-update <id> --stdin` - update cards/layout/tabs on a dashboard.
          fixed_args:
          - dashboard
          - cards
          - dashboard-update
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_copy:
          description: Run `dashboard copy dashboard <from-dashboard-id>` - copy a dashboard.
          fixed_args:
          - dashboard
          - copy
          - dashboard
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: denied
        dashboard_copy_with_body:
          description: Run `dashboard copy dashboard <from-dashboard-id> --stdin` - copy dashboard with JSON options
            such as name, collection, or deep copy.
          fixed_args:
          - dashboard
          - copy
          - dashboard
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_save:
          description: Run `dashboard save --stdin` - save a denormalized dashboard description.
          fixed_args:
          - dashboard
          - save
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_save_in_collection:
          description: Run `dashboard save-in-collection <parent-collection-id> --stdin` - save denormalized dashboard
            into a collection.
          fixed_args:
          - dashboard
          - save-in-collection
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_related:
          description: Run `dashboard related dashboard <id>` - entities related to a dashboard.
          fixed_args:
          - dashboard
          - related
          - dashboard
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_valid_filter_fields:
          description: Run `dashboard valid-filter-fields --filtered ... --filtering ...` - fields valid for dashboard
            filter wiring.
          fixed_args:
          - dashboard
          - valid-filter-fields
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_param_values:
          description: Run `dashboard params dashboard-values <id> <param-key>` - possible values for a dashboard
            parameter.
          fixed_args:
          - dashboard
          - params
          - dashboard-values
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_param_search:
          description: Run `dashboard params dashboard-search <id> <param-key> <query>` - search values for a dashboard
            parameter.
          fixed_args:
          - dashboard
          - params
          - dashboard-search
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_param_remapping:
          description: Run `dashboard params dashboard-remapping <id> <param-key> --value <value>` - remapped display
            value for a dashboard parameter.
          fixed_args:
          - dashboard
          - params
          - dashboard-remapping
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        dashboard_dashcard_query:
          description: Run `dashboard dashcard dashboard-query <dashboard-id> <dashcard-id> <card-id>` - run a card
            in dashboard context.
          fixed_args:
          - dashboard
          - dashcard
          - dashboard-query
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: denied
        dashboard_dashcard_query_with_body:
          description: Run `dashboard dashcard dashboard-query <dashboard-id> <dashcard-id> <card-id> --stdin` -
            run a parameterized card in dashboard context.
          fixed_args:
          - dashboard
          - dashcard
          - dashboard-query
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        dashboard_dashcard_export:
          description: Run `dashboard dashcard dashboard-export <dashboard-id> <dashcard-id> <card-id> --export-format
            <csv|json|xlsx|api>` - export a dashboard-context card result.
          fixed_args:
          - dashboard
          - dashcard
          - dashboard-export
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 180
          stdin: denied
        dashboard_dashcard_export_with_body:
          description: Run `dashboard dashcard dashboard-export <dashboard-id> <dashcard-id> <card-id> --export-format
            <csv|json|xlsx|api> --stdin` - export parameterized dashboard-context card result.
          fixed_args:
          - dashboard
          - dashcard
          - dashboard-export
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 180
          stdin: required
          suffix_args:
          - --stdin
        table_list:
          description: 'Run `table list` — list tables. Typed filters; the JSON Schema

            validator rejects malformed shapes before the call leaves the LLM,

            so the previous `--can-query` / `--term` arity failures are now

            impossible by construction. Use `extra_args` only for flags this

            schema does not surface (rare).

            '
          fixed_args:
          - table
          - list
          parameters:
            term:
              type: string
              description: Case-insensitive substring filter on table name.
              max_length: 4096
            can_query:
              type: boolean
              description: When true, only return tables this API key can query. When false, includes tables the
                key cannot read.
            data_source:
              type: integer
              description: Restrict listing to one database id.
            extra_args:
              type: string_array
              description: Escape hatch — additional argv tokens for flags this schema does not yet surface.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            parameter: term
            flag: --term
          - type: flag
            parameter: can_query
            flag: --can-query
          - type: flag
            parameter: data_source
            flag: --data-source
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        table_get:
          description: Run `table get <id>` — table metadata.
          fixed_args:
          - table
          - get
          parameters:
            table_id:
              type: integer
              description: Numeric Metabase table id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: table_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        table_query_metadata:
          description: Run `table query-metadata table <id>` — DB, fields, FK, and values useful for query construction.
          fixed_args:
          - table
          - query-metadata
          - table
          parameters:
            table_id:
              type: integer
              description: Numeric Metabase table id.
              required: true
            extra_args:
              type: string_array
              description: Escape hatch — extra argv tokens.
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: positional
            parameter: table_id
          - type: passthrough
            parameter: extra_args
          timeout_secs: 60
          stdin: denied
        table_fks:
          description: Run `table fks table <id>` - foreign keys whose destination belongs to this table.
          fixed_args:
          - table
          - fks
          - table
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        table_related:
          description: Run `table related table <id>` - related entities for a table.
          fixed_args:
          - table
          - related
          - table
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        table_data:
          description: Run `table data table <table-id>` - sample/table data from Metabase.
          fixed_args:
          - table
          - data
          - table
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: denied
        table_card_query_metadata:
          description: Run `table card-query-metadata <id>` - virtual table metadata for a saved card/model.
          fixed_args:
          - table
          - card-query-metadata
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        table_card_fks:
          description: Run `table card-fks <id>` - virtual table FK info for a saved card/model.
          fixed_args:
          - table
          - card-fks
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_get:
          description: Run `field get <id>` - field metadata.
          fixed_args:
          - field
          - get
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_values:
          description: Run `field values field <id>` - values cached/known for a field.
          fixed_args:
          - field
          - values
          - field
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_summary:
          description: Run `field summary field <id>` - summary statistics for a field.
          fixed_args:
          - field
          - summary
          - field
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_search:
          description: Run `field search field <id> <search-id> --value <text>` - search field values.
          fixed_args:
          - field
          - search
          - field
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_related:
          description: Run `field related field <id>` - related entities for a field.
          fixed_args:
          - field
          - related
          - field
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_remapping:
          description: Run `field remapping field <id> <remapped-id> --value <value>` - remapped display value for
            a field.
          fixed_args:
          - field
          - remapping
          - field
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        field_table_ids:
          description: Run `field table-ids --stdin` - resolve unique table ids for a list of field ids.
          fixed_args:
          - field
          - table-ids
          parameters: {}
          mappings: []
          timeout_secs: 60
          stdin: required
          suffix_args:
          - --stdin
        snippet_list:
          description: Run `native-query-snippet snippet-list` - list SQL snippets.
          fixed_args:
          - native-query-snippet
          - snippet-list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        snippet_get:
          description: Run `native-query-snippet snippet-get <id>` - fetch a SQL snippet.
          fixed_args:
          - native-query-snippet
          - snippet-get
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        snippet_create:
          description: Run `native-query-snippet snippet-create --stdin` - create a SQL snippet.
          fixed_args:
          - native-query-snippet
          - snippet-create
          parameters: {}
          mappings: []
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        snippet_update:
          description: Run `native-query-snippet snippet-update <id> --stdin` - update/archive a SQL snippet.
          fixed_args:
          - native-query-snippet
          - snippet-update
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 120
          stdin: required
          suffix_args:
          - --stdin
        metric_list:
          description: Run `metric list` - list Metabase metrics.
          fixed_args:
          - metric
          - list
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        metric_get:
          description: Run `metric get <id>` - metric metadata with dimensions.
          fixed_args:
          - metric
          - get
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        metric_dimension_values:
          description: Run `metric dimension metric-values <id> <dimension-key>` - values for a metric dimension.
          fixed_args:
          - metric
          - dimension
          - metric-values
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        metric_dimension_search:
          description: Run `metric dimension metric-search <id> <dimension-key> --query <text>` - search values
            for a metric dimension.
          fixed_args:
          - metric
          - dimension
          - metric-search
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
        metric_dimension_remapping:
          description: Run `metric dimension metric-remapping <id> <dimension-key> --value <value>` - remapped value
            for a metric dimension.
          fixed_args:
          - metric
          - dimension
          - metric-remapping
          parameters:
            args:
              type: string_array
              description: 'Exact argv tokens appended after the fixed command prefix. ONE

                TOKEN PER ELEMENT — a flag and its value are SEPARATE elements:

                ["--can-query", "true"]   ✅

                ["--can-query=true"]      ❌ (joined with `=`)

                ["--can-query"]           ❌ (value missing)

                Value-taking flags such as `--term`, `--can-query`,

                `--data-source`, `--models`, `--export-format`, `--select`,

                `--value`, `--q` ALWAYS require the value as the next element.

                Boolean toggles such as `--compact`, `--agent`, `--stdin` stand

                alone (no value).'
              required: true
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: passthrough
            parameter: args
          timeout_secs: 60
          stdin: denied
    runtime_catalog:
      categories:
      - analytics
      - business_intelligence
      - data
      - metabase
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 60
---

# Metabase

Tool name: `metabase`

Drives `metabase-pp-cli`, a Printing-Press-generated Metabase CLI. Auth comes
from `METABASE_BASE_URL` and `METABASE_API_KEY` in the scope's `.env`. JSON and
non-interactive flags are baked into the pack command prefix.

This skill is about what Metabase itself can do. It should finish with
Metabase evidence: ids, SQL, query outputs, export paths, card/dashboard
changes, or a precise reason Metabase cannot satisfy the requested operation.

## Native Flow

Use the smallest Metabase action that advances the task:

1. Search existing assets before writing SQL.
2. Inspect candidate cards, dashboards, models, snippets, tables, and fields.
3. Run saved questions when they match the requested definition.
4. Use dashboard-context query actions when dashboard filters or dashcard ids
   matter.
5. Use ad-hoc SQL only after choosing a database and inspecting schema.
6. Validate with cheap queries before broad exports or saved asset changes.
7. Create or update cards/dashboards only when the user wants durable
   Metabase-native output and the query has been validated in Metabase.

Do not create a card merely because Metabase was the data source. If the final
answer needs transformations, external data, spreadsheet formulas, or local
processing outside Metabase, export or report the Metabase evidence and stop at
the Metabase boundary.

## Inline vs Export — Pick The Right Mode

Two modes return the same data. Choose based on what your task does next:

| Use case | Tool | Returns |
|----------|------|---------|
| Aggregations, totals, small top-N (≲ 50 rows × ≲ 10 cols) | `card_run`, `dataset_query` | Inline JSON (slim shape with `sql + columns + rows`) |
| Row-heavy results — bulk lists, detail tables, anything you'd inspect outside the agent's context | `card_export`, `dataset_export` (with `--deliver file:<path>`) | File artifact (CSV/JSON/XLSX); the path is captured as a `tool_output_file` artifact |
| You only need to look at the SQL definition | `card_get`, `dataset_to_native` | The card body / compiled native SQL — no execution |

**Rule of thumb**: if your follow-up step is "read every row and sum them up
myself" or "render this in a chat bubble", you wanted an aggregation —
rewrite the SQL to produce that aggregation and use `card_run` /
`dataset_query`. If your follow-up step is "filter, pivot, join with other
data, or look at outliers", you want a file — use `card_export` /
`dataset_export` with `--deliver file:<path>` and reach for downstream
skills.

Downstream skills for an exported CSV / JSON / XLSX:

- **`csvkit`** — `csvcut`, `csvgrep`, `csvjoin`, `csvstat`, `csvsort`,
  `csvlook`. Per-column statistics, joins across multiple exports,
  filtering. Lives at `skillshub/csvkit/`.
- **`marimo`** — reactive Python notebook with pandas. Use when you need
  multi-step transformation, plotting, or to compare exports from
  multiple cards. Lives at `skillshub/marimo/`.
- **`sheets`** — Google Sheets integration for sharing the slice with a
  human collaborator.

Stuffing a 2,000-row export into the agent's context is the wrong move —
it blows the per-call tool-result cap, and even partial truncation leaves
the agent reasoning about a non-representative slice. Pick the right
mode up front.

## How To Think About Data

These are reasoning patterns, not mandatory steps. Skip what doesn't
apply. The shortest path to a correct answer is usually best.

### Decompose The Ask Before Searching

A user asking for "total sales yesterday" is asking about a *business
metric*, not a *column*. Sales pre-tax or post-tax? GMV at order or
NMV after discount? Booked sales or delivered value? Including platform
fee + delivery charges, or excluding?

Find the canonical definition before writing SQL. The fastest path is
usually to look at 2-3 candidate cards that *already* compute the
metric and read their SQL (`card_get` + `dataset_to_native`). If they
agree on the formula, copy it. If they disagree, *the disagreement is
the information* — surface it to the user as part of the answer.

### Triangulate Definitions Across Cards

When the same metric appears in multiple cards, compare the SQL.
Differences usually fall into a few buckets:

- **Filter set**: one card excludes preferred-brand orders, the other
  doesn't — pick the one matching the user's intent.
- **Date grain / timezone**: one uses `DATE(order_date)`, another uses
  `order_date >= now() - INTERVAL 1 DAY` — these compute different
  things at midnight boundaries.
- **Join order**: one aggregates before joining auxiliary tables, the
  other joins first — the second has fan-out risk (see SQL traps).

If you can't pick between two definitions, do the cheap thing: run
*both* (each is one `dataset_query`), report the two numbers with the
SQL excerpt that distinguishes them, and let the user choose. Honest
ambiguity beats a confidently wrong single number.

### Schema-Introspection Cascade (When No Card Exists)

The data analyst's fallback chain when the answer isn't already in a
saved card:

1. `database_list` → which database holds the data?
2. `database_metadata` → tables in that database, named with hints
   about content.
3. `table_query_metadata` on candidate tables → column list with types
   and semantic-type hints (PK / FK / Email / Currency / Timestamp).
4. `table_data` with a small LIMIT → eyeball 5-10 sample rows to see
   what values actually look like.
5. `field_summary` on the columns you'd aggregate / filter on →
   distinct counts, min/max, null fraction.
6. Only THEN write your aggregation.

This costs 5-6 cheap tool calls but prevents writing a 100-line SQL
against the wrong table.

### Pre-Flight Sanity Queries

Before any "real" query, do the cheap probes:

```sql
-- Timezone: what does `today()` actually mean here?
SELECT timezone() AS db_tz, today() AS db_today,
       yesterday() AS db_yesterday, now() AS db_now;

-- Row count for the time window — sanity check the filter
SELECT COUNT(*) FROM <table> WHERE DATE(<date_col>) = yesterday();

-- Distinct vs total — catches fan-out before the aggregate runs
SELECT COUNT(*) AS rows, COUNT(DISTINCT <join_key>) AS distinct_keys
FROM <table_with_join> WHERE <filter>;
```

If `rows > distinct_keys` for a key you intend to GROUP BY, you have
duplication (and your `SUM(...)` will be wrong by exactly that ratio).

### Common SQL-Correctness Traps

**Join fan-out**

`LEFT JOIN` against a 1-to-many table multiplies the rows on the left
side. Classic symptom: SUMs look ~1.5×-2× too high.

```sql
-- ❌ Wrong: sm × po fans out
SELECT SUM(sm.app_price * sm.delivered_qty) AS total
FROM sku_margins sm
LEFT JOIN public_orders po ON po.order_id = sm.order_id;

-- ✅ Fix: aggregate po first, then join
WITH po_per_order AS (
  SELECT order_id,
         SUM(platform_fee) AS platform_fee,
         SUM(delivery_charges) AS delivery_charges
  FROM public_orders GROUP BY order_id
)
SELECT SUM(sm.app_price * sm.delivered_qty)
       + SUM(COALESCE(po.platform_fee + po.delivery_charges, 0)) AS total
FROM sku_margins sm
LEFT JOIN po_per_order po ON po.order_id = sm.order_id;
```

Before any JOIN, ask: "is the join key unique on at least one side?" If
the answer is *no on both sides*, you need a pre-aggregation CTE.

**ClickHouse `FINAL`**

ReplacingMergeTree / CollapsingMergeTree tables in ClickHouse store
versioned rows that get merged in the background. Querying without
`FINAL` returns the un-merged rows — meaning the same logical row
appears multiple times.

```sql
-- ❌ May double-count after a recent write
SELECT SUM(delivered_qty * app_price) FROM sku_margins
WHERE date(delivered_date) = yesterday();

-- ✅ Use FINAL on ReplacingMergeTree tables
SELECT SUM(delivered_qty * app_price) FROM sku_margins FINAL
WHERE date(delivered_date) = yesterday();
```

When in doubt, check `SHOW CREATE TABLE <name>` for the engine. If it's
`ReplacingMergeTree(...)` or similar, you need `FINAL`.

**Timezone vs date filter**

`today() - 1` evaluates in the DB's session timezone, not the user's.
Different DBs return different things for "yesterday" depending on
when the query runs. Always confirm timezone alignment with the user's
expectation.

```sql
SELECT timezone() AS db_tz, today() AS db_today;
-- If db_tz != user's TZ, you may need to cast:
--   WHERE toDate(toTimeZone(order_date, 'Asia/Calcutta')) = today() - 1
```

**Aggregation pre-join vs post-join**

When attaching a dimension to a metric, decide whether to aggregate
the metric first. Aggregating after the join means your SUM iterates
over duplicated rows.

```sql
-- ❌ N rows of orders × M rows of order_lines = N*M
SELECT o.warehouse, SUM(ol.qty * ol.price)
FROM orders o LEFT JOIN order_lines ol ON ol.order_id = o.id
GROUP BY o.warehouse;
-- This is correct only if order_id is unique in `orders`.

-- ✅ Aggregate the metric first, then join the dimension
WITH per_order AS (
  SELECT order_id, SUM(qty * price) AS line_total
  FROM order_lines GROUP BY order_id
)
SELECT o.warehouse, SUM(per_order.line_total)
FROM orders o LEFT JOIN per_order ON per_order.order_id = o.id
GROUP BY o.warehouse;
```

**Bottom-N by ratio with tiny denominators**

`SUM(margin) / SUM(revenue) ORDER BY ratio ASC` returns mostly SKUs
that sold one unit and got refunded. If the user wants "lowest-margin
products people actually buy", add a volume threshold:

```sql
HAVING SUM(qty) > <threshold>   -- exclude one-off / refund-only SKUs
```

## Worked Patterns

These sketch *shapes* of analyst flows, not your specific tables or
metrics. Names like `<fact_table>` / `<metric_col>` / `<card_id_a>`
are placeholders — your schema will be different. The point is the
*order of operations*, not the SQL verbatim.

### Pattern A — "A single metric for a time window" (aggregation, inline)

Applies whenever the user wants 1–N numbers (totals, averages, counts)
for a fixed period.

```
1. find_search --q "<metric name>"
2. card_get on the top 1-3 hits → read native_form SQL.
   If a card already computes it cleanly, run it (card_run).
3. If no card matches: pre-flight the database
   - dataset_query: SELECT timezone(), today(), now();
   - dataset_query: SELECT count(*), count(DISTINCT <key>) FROM
                    <fact_table> WHERE <date_col> = <date>;
     (rows > distinct keys ⇒ fan-out risk if you join)
4. Write the smallest aggregation that lives on a single fact table.
   Use FINAL on ReplacingMergeTree (ClickHouse). Run inline:
     SELECT sum(<metric_col>) AS <metric>
     FROM <fact_table> FINAL
     WHERE <date_col> = <date>;
5. If a card exists AND you wrote ad-hoc SQL, cross-check both. A gap
   is information — surface it with the SQL diff, don't silently pick.
```

### Pattern B — "Top / Bottom N by some ratio" (small result, inline)

Applies for "5 lowest / 10 best / worst customers / hottest products".

```
1. find_search for an existing ranking card. Run it if available.
2. Else, ad-hoc:
     SELECT <dimension>,
            sum(<numerator>)   / nullif(sum(<denominator>), 0) AS ratio
     FROM <fact_table> FINAL
     WHERE <date_filter>
     GROUP BY <dimension>
     HAVING sum(<volume_col>) > <threshold>   -- filter tiny denominators
     ORDER BY ratio ASC
     LIMIT <N>;
3. The threshold matters: without it, ratio rankings are dominated by
   single-unit / refund-only rows. Pick a threshold that excludes the
   noise floor for your data.
4. N rows × small col-count fit inline — no export needed.
```

### Pattern C — "Wide / row-heavy result for downstream processing" (export)

Applies when the user wants to inspect, pivot, plot, or join across
periods — anything where you'd want a spreadsheet.

```
1. find_search → if an existing card matches, prefer card_export.
2. Else, dataset_export with --deliver file:<path>:
     dataset_export --export-format csv --deliver file:/tmp/<topic>.csv
     stdin: {"database":<id>,"type":"native","native":{"query":
       "SELECT <columns> FROM <fact_table> FINAL WHERE <filters>
        GROUP BY <dimensions> ORDER BY <key>"
     }}
3. Reach for the right downstream skill:
   - csvkit: per-column stats, filtering, joining multiple exports
   - marimo: pandas notebook for multi-step transforms / plotting
   - sheets: hand off to a human collaborator
4. Summarize back: total rows, file artifact id, one-line description.
   Don't paste the rows into the chat.
```

### Pattern D — "Two cards / sources give different answers"

Applies whenever you discover that two saved cards or two SQL paths
both claim to compute the same metric and don't agree.

```
1. card_get <card_id_a>; card_get <card_id_b>
   → read both native_form SQL bodies, find the difference. Typical
     differences:
     - filter set (one excludes a brand / status / refund)
     - date grain or timezone (DATE() vs raw timestamp)
     - source table (booking-time vs delivery-time fact)
     - join pattern (fan-out vs aggregated CTE)
2. dataset_to_native on each if either is MBQL — compare compiled SQL.
3. Run BOTH via card_run. Report both numbers plus a one-sentence
   reason for the gap. Honest ambiguity beats a confidently wrong
   single number.
4. Either ask the user which definition they want, or pick the one
   that best matches the *verb* in the question ("delivered" matches
   delivery-time fact; "booked" matches placement-time fact).
```

### Pattern E — "No existing card; build from scratch via schema introspection"

Applies when find_search returns nothing useful for the requested metric.

```
1. database_list → which database?
2. database_metadata <db_id> → tables in the database.
3. For each candidate table (start with the 2-3 most-named-like-the-
   metric ones):
   - table_query_metadata → column list + semantic types
   - table_data with LIMIT 5 → eyeball sample rows
   - field_summary on date / amount / id columns → ranges, null %,
     distinct counts
4. Decide grain: one row per <X>? Per <X> per <Y>?
5. Identify the metric columns (often `*_amount`, `*_gmv`, `*_value`)
   and the date column (often `*_date`, `*_at`, `*_timestamp`).
6. Write a small aggregation. Run it. Compare against any related
   card you found earlier — if numbers agree, you've validated the
   columns. If they disagree, you've found a definition mismatch.
```

The discipline: *cheap probes first, big query last*. A 5-call
introspection cascade costs ~30 seconds and prevents a 500-line CTE
against the wrong table.

## Output Shapes

- Discovery/list/get commands usually return:
  `{"meta":{"source":"live"},"results":<api-response>}`.
- Query commands `card_run`, `card_run_with_body`, and `dataset_query` use
  `--compact` by default, which slims the Metabase response to a stable
  agent-friendly envelope:
  ```
  {
    "action": "post", "success": true, "status": 202,
    "data": {
      "data": {
        "sql":     "<exact SQL Metabase executed>",
        "columns": [{"name":"<col>", "type":"type/Float", ...}, ...],
        "rows":    [[v, v, ...], ...],
        "row_count": <int or absent>,
        "requested_timezone": "Asia/Calcutta"
      },
      "database_id": 236,
      "status": "completed"
    }
  }
  ```
  Compared to the raw Metabase API response, this drops `cols[i].fingerprint`
  (sync-time column stats, often stale), `cols[i].field_ref` (MBQL drill-
  through tags), `cols[i].lib/*` (MLv2 alias plumbing), `insights` (dashboard
  sparkline computations), `results_metadata` (a duplicate of `cols` for
  cache invalidation), `pivot-export-options`, `json_query`, and execution
  bookkeeping. The `sql` field is kept — it's the exact native SQL Metabase
  ran, useful for cribbing correct join patterns from existing cards.
- `dataset_to_native` returns the compiled native SQL without executing —
  use it to inspect what a card would run.
- Export commands such as `card_export`, `dataset_export`, and
  `dashboard_dashcard_export` return raw CSV/JSON/XLSX bytes on stdout
  unless `--deliver file:<path>` is used. **Always pair exports with
  `--deliver file:<path>`** so the result lands as a `tool_output_file`
  artifact you (or downstream skills like csvkit / marimo) can reference
  by id, instead of being streamed into the next tool-call prompt.
- `doctor` returns flat status fields.
- `find_search` returns Metabase search results under `.results`.

Errors land on stderr with typed exit codes: 3 not found, 4 auth, 5 upstream,
7 rate-limited, 10 config.

## Capability Map

- Auth/setup: `doctor`.
- Search: `find_search`.
- Cards/questions: `card_list`, `card_get`, `card_query_metadata`, `card_run`,
  `card_run_with_body`, `card_export`, `card_export_with_body`,
  `card_create`, `card_update`, `card_copy`, `card_dashboards`, `card_series`.
- Card parameters: `card_param_values`, `card_param_search`,
  `card_param_remapping`.
- Dashboards: `dashboard_list`, `dashboard_get`, `dashboard_items`,
  `dashboard_query_metadata`, `dashboard_related`, `dashboard_create`,
  `dashboard_update`, `dashboard_update_cards`, `dashboard_copy`.
- Dashboard-context execution: `dashboard_dashcard_query`,
  `dashboard_dashcard_query_with_body`, `dashboard_dashcard_export`,
  `dashboard_dashcard_export_with_body`.
- Dashboard parameters: `dashboard_param_values`, `dashboard_param_search`,
  `dashboard_param_remapping`, `dashboard_valid_filter_fields`.
- Databases/schema: `database_list`, `database_get`, `database_metadata`,
  `database_schemas`, `database_schema`, `database_schema_list`,
  `database_fields`, `database_idfields`, `database_usage_info`,
  `database_autocomplete_suggestions`, `database_card_autocomplete_suggestions`.
- Tables/fields: `table_list`, `table_get`, `table_query_metadata`,
  `table_fks`, `table_related`, `table_data`, `field_get`, `field_values`,
  `field_summary`, `field_search`, `field_related`, `field_remapping`,
  `field_table_ids`.
- Ad-hoc SQL/MBQL: `dataset_query`, `dataset_export`,
  `dataset_query_metadata`, `dataset_parameter_values`,
  `dataset_parameter_search`, `dataset_parameter_remapping`, `dataset_pivot`,
  `dataset_to_native`.
- SQL snippets: `snippet_list`, `snippet_get`, `snippet_create`,
  `snippet_update`.
- Metrics: `metric_list`, `metric_get`, `metric_dimension_values`,
  `metric_dimension_search`, `metric_dimension_remapping`.
- Collections/placement: `collection_tree`, `collection_items`,
  `collection_create`, `collection_update`, `collection_dashboard_question_candidates`,
  `collection_move_dashboard_question_candidates`, `card_collections`,
  `cards_dashboards`, `cards_move`.
- Escape hatch: `raw` only when the generated CLI supports a command not yet
  modeled as a native action.

## Search And Inspection

Start with search when the user names a metric, business concept, dashboard,
table, or report:

    find_search {"args": ["--q", "refund revenue"]}
    find_search {"args": ["--q", "refund revenue", "--models", "card,dataset,dashboard,table"]}
    find_search {"args": ["--q", "orders", "--models", "card,dataset", "--search-native-query", "true"]}

Classify candidates before using them:

- `direct_match`: likely answers the request as-is.
- `partial_match`: close but needs filters, date range, grain, or fields changed.
- `source_of_logic`: useful joins, formulas, or business rules.
- `schema_clue`: useful table/field names only.
- `irrelevant`: ignore.

For dashboards, inspect the dashboard and then inspect cards on it:

    dashboard_get {"args": ["987"]}
    dashboard_items {"args": ["987"]}
    card_get {"args": ["12345"]}
    card_query_metadata {"args": ["12345"]}

For a candidate saved question, inspect before running:

    card_get {"args": ["12345"]}
    card_query_metadata {"args": ["12345"]}
    card_run {"args": ["12345"]}

When dashboard filters matter, use dashboard-context actions:

    dashboard_param_values {"args": ["987", "region"]}
    dashboard_dashcard_query {"args": ["987", "456", "12345"]}
    dashboard_dashcard_export {"args": ["987", "456", "12345", "--export-format", "csv"]}

## Schema Discovery

When writing SQL from scratch, choose the database and inspect metadata first:

    database_list {"args": ["--select", "id,name,engine,is_full_sync"]}
    database_get {"args": ["236", "--include", "tables"]}
    database_metadata {"args": ["236"]}

For large databases, narrow the search:

    database_schemas {"args": ["236"]}
    database_schema {"args": ["236", "public"]}
    database_fields {"args": ["236", "--select", "table_id,name,base_type,semantic_type"]}

For precise table and field behavior:

    table_list {"args": ["--term", "orders", "--can-query", "true"]}
    table_get {"args": ["98765"]}
    table_query_metadata {"args": ["98765"]}
    table_fks {"args": ["98765"]}
    field_get {"args": ["54321"]}
    field_values {"args": ["54321"]}
    field_summary {"args": ["54321"]}

Use snippets and metrics to discover reusable definitions:

    snippet_list {}
    snippet_get {"args": ["42"]}
    metric_list {}
    metric_get {"args": ["77"]}

## Querying

Metabase `/api/dataset` expects a JSON body. Pass it through the `stdin`
argument; do not use shell pipes, heredocs, redirection, or manual quoting.

    dataset_query {
      "stdin": "{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT count(*) AS rows FROM orders\"}}"
    }

Use cheap validation before large runs:

- `SELECT count(*) ...` for row counts and filters.
- `SELECT min(date_col), max(date_col) ...` for date windows.
- `SELECT key, count(*) ... GROUP BY key HAVING count(*) > 1 LIMIT 20` for
  uniqueness before joins.
- `SELECT ... LIMIT 20` for columns, types, and sample values.
- Compare aggregates with existing saved cards/dashboards when possible.

Use `dataset_query_metadata` when building or validating MBQL/query metadata:

    dataset_query_metadata {
      "stdin": "{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT * FROM orders LIMIT 20\"}}"
    }

Use parameter helpers before parameterized saved-card/dashboard execution:

    card_param_values {"args": ["12345", "start_date"]}
    dashboard_param_search {"args": ["987", "region", "west"]}

## Exports

Use exports when result sets are too large for stdout or when the user asked
for a file. Prefer explicit file delivery:

    card_export {"args": ["12345", "--export-format", "csv", "--deliver", "file:/tmp/metabase-card-12345.csv"]}
    dataset_export {
      "args": ["--export-format", "csv", "--deliver", "file:/tmp/metabase-query.csv"],
      "stdin": "{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT * FROM orders\"}}"
    }
    dashboard_dashcard_export {"args": ["987", "456", "12345", "--export-format", "csv", "--deliver", "file:/tmp/metabase-dashboard-card.csv"]}

Report the output path, source id/query, columns when known, and row count when
known. If the export is a fallback because Metabase cannot finish the whole
request, state the limitation plainly and include enough evidence for the
caller to continue elsewhere.

## Saved Cards And Dashboards

Use `card_create`/`card_update` only when:

- the final logic is expressible as Metabase SQL/MBQL,
- all required data is available to Metabase,
- the query has been validated in Metabase,
- the saved asset has repeat value,
- the user asked for persistence or the task clearly requires it.

Create a SQL card:

    card_create {
      "stdin": "{\"name\":\"Weekly orders\",\"display\":\"table\",\"type\":\"question\",\"dataset_query\":{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT date_trunc('week', created_at) AS week, count(*) AS orders FROM orders GROUP BY 1 ORDER BY 1 DESC\"}}}"
    }

Update a card:

    card_update {
      "args": ["12345"],
      "stdin": "{\"dataset_query\":{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT 1\"}},\"display\":\"table\"}"
    }

For dashboard persistence, inspect the current dashboard first:

    dashboard_get {"args": ["987"]}
    dashboard_update_cards {
      "args": ["987"],
      "stdin": "{\"cards\":[{\"id\":12345,\"row\":0,\"col\":0,\"size_x\":12,\"size_y\":8}]}"
    }

Save/update checklist:

- Confirm database id, collection id, dashboard id, and card id.
- Preserve parameters, visualization settings, collection placement, and
  display type unless intentionally changing them.
- Inspect the old card/dashboard before modifying business-critical assets.
- Run or export after saving to verify the persisted asset.

## Response Discipline

When finishing a Metabase task, include the relevant concrete evidence:

- asset ids and names inspected,
- candidate classification and why,
- database/table/field ids used,
- SQL or MBQL used,
- validation checks and results,
- output shape, row count, or export path,
- created/updated card/dashboard ids, if any,
- explicit Metabase limitation, if any.

## Argv Shape

Each native action prepends a fixed argv prefix. Pass extra positional args,
ids, or flags as elements of `args`: one element per shell token, no manual
quoting.

    card_run {"args": ["12345"]}
    card_run_with_body {"args": ["12345"], "stdin": "{\"parameters\":[]}"}
    card_export {"args": ["12345", "--export-format", "csv"]}
    database_metadata {"args": ["236"]}
    find_search {"args": ["--q", "wallet"]}
    dataset_query {"stdin": "{\"database\":236,\"type\":\"native\",\"native\":{\"query\":\"SELECT 1\"}}"}

Use `help` for CLI help only when this guide does not cover the needed command.
