---
name: "news-search-via-tavily"
version: 0.2.3
description: "Time-sensitive web/news search via Tavily AI. This is a provider adapter used by the `content_search` controller and a direct diagnostic tool when Tavily itself is explicitly requested. Not for ordinary stable factual Q&A or long autonomous reports (`deep-research-with-openai`)."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.tavily.com
    content_source:
      schema_version: 1

      adapter:
        id: tavily
        display_name: Tavily web search
        class: web_search
        execution: remote_endpoint
        auth: required
        sends_user_intent: true
        metered: true
        cursor: false
        privacy: public
        max_results: 20
        retrieval:
          action_id: tavily.discover
          operation: discover
          rung: public_search
          priority: 200
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true
          estimated_cost:
            commodity: tavily_credit
            amount_microunits: 2000000

      capability:
        name: news-search-via-tavily
        action: run

      input:
        query_argument: query
        limit_argument: max_results
        max_query_chars: 4096
        fixed_arguments:
          include_raw_content: "false"
          include_answer: "false"
        options:
          search_depth:
            argument: search_depth
            value_type: string
            allowed: [ultra-fast, fast, basic, advanced]
          topic:
            argument: topic
            value_type: string
            allowed: [general, news, finance]
          time_range:
            argument: time_range
            value_type: string
            allowed: [day, week, month, year]
          start_date:
            argument: start_date
            value_type: string
          end_date:
            argument: end_date
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
          cheap_text_pointers: [/content]
          published_at_pointer: /published_date
          metadata:
            provider_score: /score
        cost:
          pointer: /cost_microunits
          commodity: tavily_credit
          encoding: microunits
    requires:
      bins: ["tavily-search"]
      env: ["TAVILY_API_KEY"]
    install_hint:
      docs: "requires env: TAVILY_API_KEY — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        query: "technology news"
        max_results: 3
      expect:
        min_items: 1
        items_pointer: "/results"
        max_latency_ms: 45000
        # This package does not price in money and cannot: Tavily's response
        # carries no cost field and no cost header, and the dollar value of a
        # credit is a property of the operator's plan, not of the call. So the
        # adapter reports what it can actually observe — credits consumed, one
        # for a basic search and two for an advanced one — under
        # `content_source.output.cost.commodity: tavily_credit`, which is the
        # form the product's own retrieval budgets are keyed by.
        #
        # The ceiling therefore has to be written in the same commodity.
        # 1000000 microunits is exactly one credit: the cost of the basic
        # search this canary declares, and a bound that trips the moment the
        # probe drifts to `search_depth: advanced` and starts costing double.
        #
        # It previously read 20000, copied from exa, whose commodity is `usd`.
        # Nothing in either declaration was wrong; the ceiling simply counted
        # something else, and one credit — a real cost near $0.008 — was
        # reported as a dollar of spend. `max_cost_commodity` is now required
        # so that mistake cannot be made silently again.
        max_cost_microunits: 1000000
        max_cost_commodity: tavily_credit
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [tavily-search]
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
          # bin/tavily-search issues one request at PROVIDER_TIMEOUT_SECS 30,
          # so its own deadline budget is 30, plus a 5s margin for interpreter
          # start, stdin read, and normalization => 35. This was 30: an exact
          # tie the kill always won, because the socket deadline only starts
          # after process start, so the adapter's timeout handling could never
          # run. YAML cannot reference Python, so the adapter mirrors this
          # number as GOVERNED_KILL_CEILING_SECS and phase7_migration.rs pins
          # the two together.
          timeout_secs: 35
          stdin_bytes: 65536
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: tavily_api_key
            secret_ref: TAVILY_API_KEY
        injections:
          - source: {kind: secret, binding: tavily_api_key}
            target: {kind: environment, name: TAVILY_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded Tavily news or time-sensitive web search.
          parameters:
            query:
              type: string
              description: Search query.
              required: true
              min_length: 1
              max_length: 4096
            search_depth:
              type: string
              description: Retrieval depth and latency/cost tier.
              default: basic
            topic:
              type: string
              description: Provider search corpus.
              default: general
            time_range:
              type: string
              description: Optional recency filter.
            start_date:
              type: string
              description: Optional inclusive lower date bound (YYYY-MM-DD).
              max_length: 10
            end_date:
              type: string
              description: Optional inclusive upper date bound (YYYY-MM-DD).
              max_length: 10
            max_results:
              type: integer
              description: Maximum result count.
              default: 5
              minimum: 1
              maximum: 20
            include_raw_content:
              type: string
              description: Include no raw content, or request boolean/Markdown/text content.
              default: "false"
            include_answer:
              type: string
              description: Request no answer, a basic answer, or an advanced answer.
              default: "false"
            chunks_per_source:
              type: integer
              description: Optional chunks per source for advanced search.
              minimum: 1
              maximum: 10
            auto_parameters:
              type: boolean
              description: Allow Tavily to tune retrieval parameters.
              default: false
            exact_match:
              type: boolean
              description: Use restrictive exact-match retrieval.
              default: false
            include_domains:
              type: string
              description: Optional comma-separated domain allowlist.
              max_length: 4096
            exclude_domains:
              type: string
              description: Optional comma-separated domain denylist.
              max_length: 4096
    runtime_catalog:
      categories: [web, search, research, news]
      composition_category: web_operations
      expose_timeout_control: true
---

# News Search Via Tavily

News / time-sensitive web search via Tavily AI.

The active runtime resolves `TAVILY_API_KEY` from the canonical scoped secret
authority only after execution authorization. Existing private scoped `.env`
installations remain a migration bridge when no vault record exists; a vault
record always wins. The key is injected into the self-contained
`tavily-search` adapter and is never accepted from model input.

CAPABILITY:
- Tunable depth: ultra-fast / fast / basic / advanced.
- RAG-optimized: results include extracted page content inline, ready for LLM consumption.
- Time-range filters (1d / 7d / 30d / year).
- Domain include/exclude lists.
- Advanced mode also returns a synthesized `answer`.

OUTPUT:
- `{ query, answer?, results: [{ title, url, content, score, published_date }] }`.
- `answer` only on advanced mode.

LIMITS & COST:
- Latency scales with depth (ultra-fast ~1s, advanced ~10s).
- Tavily API quota.
- Ordinary agents should use `content_search`; call this directly only for an explicit Tavily request or controller diagnosis.
- Not for semantic discovery (semantic-websearch-via-exa) or long autonomous reports (deep-research-with-openai).

EXAMPLE PROMPTS:
- "What's happening with the Fed rate decision today?"
- "Latest news on the Anthropic-OpenAI partnership rumor"
- "Recent updates on the SF housing law in the last 7 days"
