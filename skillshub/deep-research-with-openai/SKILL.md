---
name: "deep-research-with-openai"
version: 0.2.1
description: "Long-form autonomous research powered by OpenAI's deep-research models. Use only when the caller explicitly requests a multi-source, multi-step investigation producing a comprehensive cited report. Not for ordinary Q&A (`content_search`), today's news lookup, or single-page extraction."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.openai.com
    requires:
      bins: ["openai-deep-research"]
      env: ["OPENAI_API_KEY"]
    install_hint:
      docs: "requires env: OPENAI_API_KEY — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: expensive
      action: run
      input:
        query: "What is the Rust programming language?"
      expect:
        min_items: 1
        items_pointer: "/sources"
        require_pointers: ["/answer"]
        max_latency_ms: 600000
        max_cost_microunits: 2000000
        # Ceilings are commodity-scoped. This one bounds real money; the
        # runner compares it only against a package that reports `usd`, so
        # it can never be read against a provider-credit figure.
        max_cost_commodity: usd
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [openai-deep-research]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: denied}
        limits:
          timeout_secs: 600
          stdin_bytes: 65536
          stdout_bytes: 20971520
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - {name: openai_api_key, secret_ref: OPENAI_API_KEY}
        injections:
          - source: {kind: secret, binding: openai_api_key}
            target: {kind: environment, name: OPENAI_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded asynchronous OpenAI deep-research investigation.
          parameters:
            query:
              type: string
              description: Research query (natural language, complex questions work best).
              required: true
              min_length: 1
              max_length: 4096
            model:
              type: string
              description: OpenAI deep-research model identifier.
              default: o4-mini-deep-research
              max_length: 256
            poll_interval_secs:
              type: integer
              description: Seconds between polling attempts.
              default: 10
              minimum: 1
              maximum: 60
            max_poll_attempts:
              type: integer
              description: Maximum number of poll attempts before giving up.
              default: 60
              minimum: 1
              maximum: 600
            max_tool_calls:
              type: integer
              description: Optional cap on total tool calls made by the research model.
              minimum: 1
              maximum: 10000
    runtime_catalog:
      categories: [web, search, research, llm, deep_research]
      composition_category: web_operations
      expose_timeout_control: true
---

# Deep Research With Openai

Long-form autonomous research via OpenAI deep-research models.

CAPABILITY:
- Async — runs autonomously for minutes-to-hours, polled.
- Performs many searches, reads full pages, cross-references, structures findings.
- Most thorough research path in the system.
- Accepts custom instructions / system prompts.

OUTPUT:
- Structured report `{ summary, sections: [{ heading, body, sources }], all_sources: [...], duration_s }`.

LIMITS & COST:
- Highest cost research path.
- Long latency (minutes-to-hours).
- Inherits OpenAI deep-research API quota.
- Not for quick Q&A (`content_search`) or a bounded current-events lookup.

EXAMPLE PROMPTS:
- "Comprehensive report on the AI agent ecosystem 2026"
- "Research the regulatory landscape for stablecoins across US/EU/Asia"
- "Build a competitive analysis of vector DB vendors"
