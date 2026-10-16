---
name: arxiv-search
description: Search arXiv's public Atom API for recent research papers without a browser.
version: 0.2.0
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: export.arxiv.org
    content_source:
      schema_version: 1

      adapter:
        id: arxiv
        display_name: arXiv
        class: syndication
        execution: remote_endpoint
        auth: none
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 50
        retrieval:
          action_id: arxiv.discover
          operation: discover
          rung: source_native
          priority: 100
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: arxiv-search
        action: run

      input:
        query_argument: query
        limit_argument: limit
        max_query_chars: 8192
        options:
          category:
            argument: category
            value_type: string

      output:
        mode: canonical_v1
        error_pointer: /error
    observe_source:
      schema_version: 1

      source:
        id: arxiv-ai
        display_name: arXiv AI
        category: research
        description: New papers from the public arXiv artificial intelligence feed

      profiles:
        - id: observe-rss
          surfaces: [observe]
          discoverable: true
          unattended: true
          read_only: true
          operation: discover
          acquisition:
            allowed_actions: [rss.discover]
            escalation: none
            targets:
              - https://rss.arxiv.org/rss/cs.AI
          schedule:
            default: twice_daily
            allowed: [hourly, twice_daily, daily]
          limits:
            max_candidates_per_run: 50
            max_selected_per_run: 10

        - id: interactive-research
          surfaces: [research]
          discoverable: false
          unattended: false
          read_only: true
          operation: discover
          acquisition:
            allowed_actions: []
            escalation: ladder
            targets: []
            ladder: default_discover
            maximum_authority: public_browser_read
          schedule:
            default: daily
            allowed: [daily]
          limits:
            max_candidates_per_run: 50
            max_selected_per_run: 10
    requires:
      bins:
      - arxiv-search
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "transformer architecture"
        limit: 3
      expect:
        min_items: 1
        items_pointer: "/items"
        error_pointer: "/error"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - arxiv-search
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
          timeout_secs: 30
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
          description: Search arXiv through its public Atom API and return normalized paper candidates.
          parameters:
            query:
              type: string
              description: arXiv search query
              required: true
              max_length: 4096
            limit:
              type: integer
              description: Value for limit.
              default: 10
            category:
              type: string
              description: Value for category.
              default: ''
              max_length: 4096
          timeout_secs: 30
    runtime_catalog:
      categories:
      - research
      - search
      - papers
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 30
---

# arXiv Search

Use this capability for source-native discovery of public arXiv papers. Supply a concise
research query, a bounded result limit, and optionally an arXiv category. The capability
returns normalized titles, abstracts, canonical paper URLs, publication timestamps, and
author/category metadata.
