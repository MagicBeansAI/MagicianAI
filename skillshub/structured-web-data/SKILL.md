---
name: structured-web-data
description: Extract valid public JSON-LD records from a downloaded HTML page without a browser.
version: 0.2.1
metadata:
  magician:
    content_reader:
      schema_version: 1

      reader:
        id: structured-schema
        display_name: Public structured web data
        class: web_page
        cache_ttl_secs: 900
        max_response_bytes: 4194304
        max_text_chars: 4194304
        min_gist_chars: 40
        min_full_text_chars: 80
        fetch_timeout_secs: 20
        max_redirects: 5
        accepted_media_types:
          - text/html
          - application/xhtml+xml
        retrieval:
          action_id: structured_schema.read
          operation: read
          rung: public_static
          priority: 400
          outputs: [gist, structured]
          authority: public_remote_read
          parallel_safe: false

      capability:
        name: structured-web-data
        action: run

      input:
        input_file_argument: input_file
        max_chars_argument: max_chars

      output:
        mode: json
        text_pointer: /content
        title_pointer: /title
        method_pointer: /method
        truncated_pointer: /truncated
        error_pointer: /error
    requires:
      bins:
      - structured-web-data
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      # The fixture carries one valid JSON-LD block plus body copy the parser
      # must ignore, so the probe reports on extraction rather than on the file
      # merely being readable.
      input:
        input_file: "canary-fixtures/structured-web-data-input.html"
        max_chars: 4194304
      fixtures:
        "canary-fixtures/structured-web-data-input.html": |
          <!doctype html>
          <html>
            <head>
              <title>Canary Structured Page</title>
              <script type="application/ld+json">
              {"@context":"https://schema.org","@type":"Article",
               "headline":"Tool canary fixture article",
               "datePublished":"2026-01-15",
               "author":{"@type":"Person","name":"Tool Canary Harness"}}
              </script>
            </head>
            <body><p>Body copy the extractor must ignore.</p></body>
          </html>
      expect:
        # The headline can only appear if the ld+json block was located, parsed,
        # and re-serialized; `/title` proves the HTML title pass ran too. The
        # error envelope at `/error` comes from content_source.output.
        stdout_contains: "Tool canary fixture article"
        require_pointers: ["/content", "/title", "/method"]
        max_latency_ms: 20000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - structured-web-data
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: required
          sensitivity: private
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 20
          stdin_bytes: 1048576
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
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Parse valid JSON-LD records from a local HTML file.
          parameters:
            input_file:
              type: string
              description: Local HTML file to inspect
              required: true
              max_length: 4096
            max_chars:
              type: integer
              description: Value for max_chars.
              default: 4194304
          timeout_secs: 20
    runtime_catalog:
      categories:
      - research
      - extraction
      - structured_data
      composition_category: data_operations
      expose_timeout_control: true
      timeout_default_secs: 20
---

# Structured Web Data

Use this capability to extract schema.org-style JSON-LD from a local HTML file. It parses
only syntactically valid JSON script blocks, returns bounded canonical JSON, and fails closed
when the page has no valid structured records.
