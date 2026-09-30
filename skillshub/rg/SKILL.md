---
name: rg
version: 0.2.0
description: Fast content search using ripgrep (rg) — regex-powered, respects .gitignore, structured output
metadata:
  magician:
    requires:
      bins:
      - rg
    install_hint:
      docs: 'requires binary on PATH: rg'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        pattern: "alpha"
        target: "canary-fixtures/rg-input.txt"
      fixtures:
        "canary-fixtures/rg-input.txt": "alpha beta\n"
      expect:
        stdout_contains: "alpha"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - rg
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
          description: Run the rg capability with the arguments selected from this capability guide. Inspect
            stdout/stderr, update the runtime ledger when useful, and call goal_reached only after the
            requested result is present.
          parameters:
            pattern:
              type: string
              description: Search pattern (regex by default, use -F for fixed/literal strings)
              required: true
              max_length: 4096
            target:
              type: string
              description: File or directory to search (defaults to current directory)
              default: .
              max_length: 4096
            flags:
              type: string
              description: Optional rg flags (e.g. '-i -n', '-c -t csv', '--json -g *.log'). See guide
                for available flags.
              default: -n
              max_length: 4096
          mappings:
          - type: split_positional
            parameter: flags
            max_items: 64
            max_item_bytes: 4096
          - type: positional
            parameter: pattern
          - type: positional
            parameter: target
          timeout_secs: 30
    runtime_catalog:
      categories:
      - text_processing
      - search
      - filtering
      - data
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 30
---

# Rg

Tool name: `rg`
Primary parameter: `pattern`
Use for: fast regex searches across files and directories. ripgrep is significantly
faster than grep for large codebases and data directories, respects .gitignore by
default, and supports rich output formatting.

Key advantages over grep:
- Much faster on large file trees (parallelized, memory-mapped)
- Respects .gitignore / .rgignore by default
- Smart case: lowercase pattern = case-insensitive, mixed case = case-sensitive
- JSON output for structured processing
- Built-in file type filters (--type csv, --type json, etc.)

The `flags` parameter accepts any rg flags:
  -i          case-insensitive
  -s          case-sensitive (override smart case)
  -w          whole word match
  -c          count matches per file
  -l          list matching filenames only
  -n          show line numbers (default)
  -N          suppress line numbers
  -v          invert match
  -m N        max matches per file
  -C N        context lines before and after
  -A N        context lines after
  -B N        context lines before
  -g GLOB     filter by glob (e.g. -g '*.csv')
  -t TYPE     filter by file type (e.g. -t json, -t csv, -t py)
  -T TYPE     exclude file type
  --json      output as JSON (one JSON object per line)
  --no-ignore  don't respect .gitignore
  --hidden    search hidden files
  --multiline search across line boundaries
  --replace STR  replace matches in output
  --stats     show search statistics
  --sort path sort results by file path

The `target` parameter is the file or directory to search.
If omitted, searches the current directory.

Examples:
- {"pattern": "ERROR|WARN", "target": "app.log", "flags": "-i -n"}
- {"pattern": "price.*\\d+\\.\\d{2}", "target": "data/", "flags": "-n -g '*.csv'"}
- {"pattern": "status", "target": "results/", "flags": "--json -t json"}
- {"pattern": "^2026-03", "target": "logs/", "flags": "-c --sort path"}
- {"pattern": "email", "target": ".", "flags": "-l -t csv"}
- {"pattern": "TODO|FIXME", "target": "src/", "flags": "-n -C 2"}
- {"pattern": "\\bfailed\\b", "target": "output.log", "flags": "-i -m 20"}
