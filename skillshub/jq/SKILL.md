---
name: jq
version: 0.2.0
description: Query and transform JSON data using jq expressions
metadata:
  magician:
    requires:
      bins:
      - jq
    install_hint:
      docs: 'requires binary on PATH: jq'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        expression: ".items[0]"
        input_file: "canary-fixtures/jq-input.json"
      fixtures:
        "canary-fixtures/jq-input.json": "{\"items\":[\"alpha\"]}\n"
      expect:
        stdout_contains: "alpha"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - jq
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
        kind: none
        requirement: none
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        run:
          description: Run the jq capability with the arguments selected from this capability guide. Inspect
            stdout/stderr, update the runtime ledger when useful, and call goal_reached only after the
            requested result is present.
          parameters:
            expression:
              type: string
              description: jq filter expression (e.g. '.items[].name', 'select(.age > 30)', '.[] | {name,
                id}')
              required: true
              max_length: 4096
            input_file:
              type: workspace_path
              access: read_file
              description: Path to JSON file to process
              required: true
              max_length: 4096
            flags:
              type: string
              description: Optional jq flags (e.g. '-r', '-c', '-s', '-r -c'). See guide for available
                flags.
              default: ''
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: flags
            max_items: 64
            max_item_bytes: 4096
          - type: positional
            parameter: expression
          - type: positional
            parameter: input_file
          timeout_secs: 30
    runtime_catalog:
      categories:
      - data_processing
      - json
      - text_processing
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Jq

Tool name: `jq`
Primary parameter: `expression`
Requires: jq installed on host PATH
Use for: extracting fields from JSON, filtering arrays, transforming structures,
counting elements, reshaping API responses.

The `flags` parameter accepts any combination of jq flags:
  -r  raw output (no JSON quotes on strings)
  -c  compact single-line output
  -s  slurp entire input into single array
  -e  exit with error on false/null
  -S  sort object keys
Combine as needed, e.g. flags="-r -c"

For inline JSON input, use the shell tool directly:
  {"tool":"shell","parameters":{"command":"echo '{json}' | jq '{expression}'"}}

Examples:
- {"expression":".items | length","input_file":"data.json"}
- {"expression":".users[] | select(.active == true) | .email","input_file":"users.json","flags":"-r"}
- {"expression":".results | sort_by(.score) | reverse | .[0:5]","input_file":"scores.json"}
- {"expression":".","input_file":"pretty.json","flags":"-c"}
- {"expression":".name","input_file":"config.json","flags":"-r -e"}
