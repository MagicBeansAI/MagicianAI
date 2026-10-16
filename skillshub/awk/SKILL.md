---
name: awk
version: 0.2.0
description: Pattern-based text processing — column extraction, aggregation, report generation
metadata:
  magician:
    requires:
      bins:
      - awk
    install_hint:
      docs: 'requires binary on PATH: awk'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        program: "{print $2}"
        input_file: "canary-fixtures/awk-input.txt"
      fixtures:
        "canary-fixtures/awk-input.txt": "alpha beta gamma\n"
      expect:
        stdout_contains: "beta"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - awk
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
          description: Run the awk capability with the arguments selected from this capability guide.
            Inspect stdout/stderr, update the runtime ledger when useful, and call goal_reached only after
            the requested result is present.
          parameters:
            program:
              type: string
              description: awk program (e.g. '{print $1}', '/pattern/ {action}', 'BEGIN{} {} END{}')
              required: true
              max_length: 4096
            input_file:
              type: string
              description: File to process
              required: true
              max_length: 4096
            flags:
              type: string
              description: 'Optional awk flags (e.g. ''-F,'', ''-F: -v OFS=\t''). See guide for available
                flags.'
              default: ''
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: flags
            max_items: 64
            max_item_bytes: 4096
          - type: positional
            parameter: program
          - type: positional
            parameter: input_file
          timeout_secs: 30
    runtime_catalog:
      categories:
      - text_processing
      - data_processing
      - reporting
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Awk

Tool name: `awk`
Primary parameter: `program`
Use for: extracting columns from structured text, computing sums/averages,
reformatting delimited data, generating reports from logs or CSVs.

The `flags` parameter accepts any combination of awk flags:
  -F SEP  input field separator (e.g. -F, for CSV, -F: for colon-delimited)
  -v VAR=VAL  assign variable (e.g. -v OFS=, for comma output separator)
Combine as needed, e.g. flags="-F, -v OFS=\t"

For inline text processing, use the shell tool directly:
  {"tool":"shell","parameters":{"command":"echo 'text' | awk '{print $1}'"}}

Examples:
- {"program":"{print $1, $3}","input_file":"data.tsv"}
- {"program":"NR>1 {sum+=$3} END{print sum}","input_file":"sales.csv","flags":"-F,"}
- {"program":"/ERROR/ {count++} END {print count}","input_file":"app.log"}
- {"program":"{print $2}","input_file":"/etc/passwd","flags":"-F:"}
- {"program":"{print $1, $3}","input_file":"data.tsv","flags":"-v OFS=,"}
