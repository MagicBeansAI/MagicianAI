---
name: sed
version: 0.2.0
description: Stream editor for text transformations — find/replace, delete lines, extract ranges
metadata:
  magician:
    requires:
      bins:
      - sed
    install_hint:
      docs: 'requires binary on PATH: sed'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        expression: "s/alpha/omega/"
        input_file: "canary-fixtures/sed-input.txt"
      fixtures:
        "canary-fixtures/sed-input.txt": "alpha\n"
      expect:
        stdout_contains: "omega"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - sed
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
          description: Run the sed capability with the arguments selected from this capability guide.
            Inspect stdout/stderr, update the runtime ledger when useful, and call goal_reached only after
            the requested result is present.
          parameters:
            expression:
              type: string
              description: sed expression (e.g. 's/old/new/g', '/pattern/d', '3,10p')
              required: true
              max_length: 4096
            input_file:
              type: string
              description: File to process
              required: true
              max_length: 4096
            flags:
              type: string
              description: Optional sed flags (e.g. '-E', '-n', '-i ""', '-E -n'). See guide for available
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
      - text_processing
      - editing
      - transformation
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Sed

Tool name: `sed`
Primary parameter: `expression`
Use for: find-and-replace in files, deleting lines by pattern, extracting line ranges,
in-place file editing, text reformatting.

The `flags` parameter accepts any combination of sed flags:
  -E  extended regex (recommended for complex patterns)
  -n  suppress default output (use with /p to print matches only)
  -i ""  edit file in place (WARNING: modifies original file)
Combine as needed, e.g. flags="-E -n"

CAUTION: -i "" modifies the file directly. Omit it for preview/dry-run (default).

For inline text processing, use the shell tool directly:
  {"tool":"shell","parameters":{"command":"echo 'text' | sed 's/old/new/g'"}}

Examples:
- {"expression":"s/old_value/new_value/g","input_file":"config.txt"}
- {"expression":"/^#/d","input_file":"data.csv","flags":"-i \"\""}
- {"expression":"3,10p","input_file":"log.txt","flags":"-n"}
- {"expression":"s/[0-9]+/NUM/g","input_file":"data.txt","flags":"-E"}
