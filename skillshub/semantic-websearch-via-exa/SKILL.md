---
name: "semantic-websearch-via-exa"
version: 0.2.1
description: "Neural / semantic web search via Exa AI. This is a provider adapter used by the `content_search` controller and a direct diagnostic tool when Exa itself is explicitly requested. Best for discovery by meaning, similar pages, academic, or niche research."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.exa.ai
    content_source:
      schema_version: 1

      adapter:
        id: exa
        display_name: Exa semantic search
        class: web_search
        execution: remote_endpoint
        auth: required
        sends_user_intent: true
        metered: true
        cursor: false
        privacy: public
        max_results: 100
        retrieval:
          action_id: exa.discover
          operation: discover
          rung: public_search
          priority: 100
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true
          estimated_cost:
            commodity: usd
            amount_microunits: 5000

      capability:
        name: semantic-websearch-via-exa
        action: run

      input:
        query_argument: query
        limit_argument: num_results
        max_query_chars: 4096
        fixed_arguments:
          contents: false
          highlights: true
          type: auto
        options:
          search_type:
            argument: type
            value_type: string
            allowed: [auto, fast, instant, deep]
          highlights:
            argument: highlights
            value_type: boolean
          category:
            argument: category
            value_type: string
          max_age_hours:
            argument: max_age_hours
            value_type: positive_integer
          start_published_date:
            argument: start_published_date
            value_type: string
          include_domains:
            argument: include_domains
            value_type: string_list
            join: ","
          exclude_domains:
            argument: exclude_domains
            value_type: string_list
            join: ","

      output:
        mode: mapped
        error_pointer: /error
        items_pointer: /results
        item:
          title_pointer: /title
          url_pointer: /url
          cheap_text_pointers: [/highlights, /text]
          published_at_pointer: /published_date
          metadata:
            provider_score: /score
        response_metadata:
          search_type: /search_type
        cost:
          pointer: /cost
          commodity: usd
          encoding: decimal_major_units
    requires:
      bins: ["exa-search"]
      env: ["EXA_API_KEY"]
    install_hint:
      docs: "requires env: EXA_API_KEY — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        query: "rust programming language"
        num_results: 3
      expect:
        min_items: 1
        items_pointer: "/results"
        error_pointer: "/error"
        max_latency_ms: 45000
        max_cost_microunits: 20000
        # Ceilings are commodity-scoped. This one bounds real money; the
        # runner compares it only against a package that reports `usd`, so
        # it can never be read against a provider-credit figure.
        max_cost_commodity: usd
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [exa-search]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory:
          mode: denied
        limits:
          # Governed wall-clock kill, and the backstop for the adapter below.
          # INVARIANT: timeout_secs >= INNER_WORST_CASE_SECS + GOVERNED_KILL_MARGIN_SECS.
          # bin/exa-search issues one request at PROVIDER_TIMEOUT_SECS 60, so
          # its own deadline budget is 60, plus a 5s margin for interpreter
          # start, stdin read, and normalization => 65. This was 60: an exact
          # tie the kill always won, because the socket deadline only starts
          # after process start, so the adapter's timeout handling could never
          # run. YAML cannot reference Python, so the adapter mirrors this
          # number as GOVERNED_KILL_CEILING_SECS and phase7_migration.rs pins
          # the two together.
          timeout_secs: 65
          stdin_bytes: 65536
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: exa_api_key
            secret_ref: EXA_API_KEY
        injections:
          - source: {kind: secret, binding: exa_api_key}
            target: {kind: environment, name: EXA_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded Exa semantic web search.
          parameters:
            query:
              type: string
              description: Search query; natural language works best for auto and deep modes.
              required: true
              min_length: 1
              max_length: 4096
            type:
              type: string
              description: Search quality and latency mode; legacy neural maps to auto.
              default: auto
            num_results:
              type: integer
              description: Number of results to return.
              default: 5
              minimum: 1
              maximum: 100
            contents:
              type: boolean
              description: Include bounded extracted text in results.
              default: true
            highlights:
              type: boolean
              description: Include condensed relevant passages.
              default: false
            category:
              type: string
              description: Optional provider content category.
            max_age_hours:
              type: integer
              description: Maximum age since Exa discovered or refreshed the page.
              minimum: 1
              maximum: 1000000
            start_published_date:
              type: string
              description: Optional inclusive lower publication date (YYYY-MM-DD).
              max_length: 10
            include_domains:
              type: string
              description: Optional comma-separated domain allowlist.
              max_length: 4096
            exclude_domains:
              type: string
              description: Optional comma-separated domain denylist.
              max_length: 4096
    runtime_catalog:
      categories: [web, search, research, semantic]
      composition_category: web_operations
      expose_timeout_control: true
---

# Semantic Websearch Via Exa

Semantic / neural web search via Exa AI.

The active runtime resolves `EXA_API_KEY` from the canonical scoped secret
authority only after execution authorization. Existing private scoped `.env`
installations remain a migration bridge when no vault record exists; a vault
record always wins. The key is injected into the self-contained `exa-search`
adapter and is never accepted from model input.

CAPABILITY:
- Search modes: fast / auto / instant / deep.
- Neural index trained for "find similar" — discovery by meaning, not keywords.
- Structured output with full text or summaries.
- Can include subpages, social posts, academic papers.
- Domain include/exclude lists supported.

OUTPUT:
- `{ results: [{ title, url, text, summary, score, published_date, author? }] }`.
- `text` is full extracted content when requested.

LIMITS & COST:
- Latency scales with mode (instant ~1s, deep ~30s+).
- Best for English content; non-English coverage thinner.
- Ordinary agents should use `content_search`; call this directly only for an explicit Exa request or controller diagnosis.
- Not for breaking news (news-search-via-tavily) or ordinary quick cited Q&A.

CONTENT SOURCE:
- `metadata.magician.content_source` exposes this same capability to feeds and recurring monitors.
- The generic capability adapter owns policy, canonicalization, provenance, limits, and cost validation; this skill remains the only Exa transport and credential implementation.

EXAMPLE PROMPTS:
- "Find blog posts similar to this Stripe engineering article"
- "Academic papers on retrieval-augmented generation from 2024 onward"
- "Companies doing what we do but in healthcare"
