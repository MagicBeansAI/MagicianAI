---
name: "deep-research-with-openai"
version: 0.2.4
description: "Long-form autonomous research powered by OpenAI (`gpt-6.1-sol` with web search, run in the background). Use only when the caller explicitly requests a multi-source, multi-step investigation producing a comprehensive cited report. Not for ordinary Q&A (`content_search`), today's news lookup, or single-page extraction. Each call waits at most 35 minutes; research that runs longer keeps going at OpenAI, and the call returns a `response_id` to collect it with a later call."
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
        timeout_secs: 600
      expect:
        min_items: 1
        items_pointer: "/sources"
        require_pointers: ["/answer"]
        # This short health probe is separate from the 35-minute workload cap.
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
          timeout_secs: 2100
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
          description: Start an OpenAI background research run and wait up to 35 minutes for it, or collect an earlier run by its `response_id`. A run still going when the call stops returns its `response_id`; call again with it to collect the result.
          parameters:
            query:
              type: string
              description: Research query (natural language, complex questions work best). Required unless `response_id` is given.
              min_length: 1
              max_length: 4096
            response_id:
              type: string
              description: The `resp_...` id an earlier call returned for research still running at OpenAI. Waits for that run instead of starting new research; `query` and `model` are then ignored.
              min_length: 6
              max_length: 256
            model:
              type: string
              description: OpenAI model identifier (`o3-deep-research` and `o4-mini-deep-research` were retired on 2026-07-23).
              default: gpt-6.1-sol
              max_length: 256
            poll_interval_secs:
              type: integer
              description: Seconds between polling attempts.
              default: 10
              minimum: 1
              maximum: 60
            max_poll_attempts:
              type: integer
              description: Optional polling cap; the 35-minute runtime limit still applies.
              default: 2100
              minimum: 1
              maximum: 2100
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

Long-form autonomous research via OpenAI `gpt-6.1-sol` with web search, run in the background and polled.

CAPABILITY:
- Async — runs autonomously at OpenAI for minutes-to-hours, polled. One call waits at most 35 minutes; a run still going then returns its `response_id`, and a later call with `response_id` collects it without starting (or paying for) the research again.
- Performs many searches, reads full pages, cross-references, structures findings.
- Most thorough research path in the system.
- Accepts custom instructions / system prompts.

OUTPUT:
- `{ answer, sources: [{ title, url }], model, usage, response_id, poll_attempts, incomplete? }`.
- A run still going when the call stops returns `{ error, response_id, status }`; call `run` again with that `response_id`.
- A run OpenAI ended early with findings (status `incomplete`) returns them with an `incomplete` reason.

LIMITS & COST:
- Highest cost research path.
- Long latency (minutes-to-hours).
- Inherits OpenAI deep-research API quota.
- Not for quick Q&A (`content_search`) or a bounded current-events lookup.

EXAMPLE PROMPTS:
- "Comprehensive report on the AI agent ecosystem 2026"
- "Research the regulatory landscape for stablecoins across US/EU/Asia"
- "Build a competitive analysis of vector DB vendors"
