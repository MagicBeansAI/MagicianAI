---
name: "deep-research-with-claude"
version: 0.2.1
description: "Long-form research via Claude with web search, web fetch, and extended thinking. Pick when the user wants a deep investigation but you prefer Claude's reasoning style — narrative answers with cited evidence and inline cross-referencing. Cheaper and faster than `deep-research-with-openai` but typically less exhaustive on very large topics."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.anthropic.com
    requires:
      bins: ["claude-deep-research"]
      env: ["ANTHROPIC_API_KEY"]
    install_hint:
      docs: "requires env: ANTHROPIC_API_KEY — set in vault before activating"
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
        bins: [claude-deep-research]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: denied}
        limits:
          timeout_secs: 120
          stdin_bytes: 65536
          stdout_bytes: 20971520
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - {name: anthropic_api_key, secret_ref: ANTHROPIC_API_KEY}
        injections:
          - source: {kind: secret, binding: anthropic_api_key}
            target: {kind: environment, name: ANTHROPIC_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded Claude web-research investigation.
          parameters:
            query:
              type: string
              description: Research query (natural language, can be complex).
              required: true
              min_length: 1
              max_length: 4096
            model:
              type: string
              description: Claude model to use.
              default: claude-sonnet-4-6
              max_length: 256
            max_searches:
              type: integer
              description: Maximum web searches Claude may make.
              default: 5
              minimum: 1
              maximum: 20
            max_fetches:
              type: integer
              description: Maximum full-page fetches Claude may make.
              default: 3
              minimum: 1
              maximum: 20
            thinking_budget:
              type: integer
              description: Extended-thinking token budget.
              default: 5000
              minimum: 1024
              maximum: 30000
            allowed_domains:
              type: string
              description: Optional comma-separated domain allowlist.
              max_length: 4096
    runtime_catalog:
      categories: [web, search, research, llm, deep_research]
      composition_category: web_operations
      expose_timeout_control: true
---

# Deep Research With Claude

Long-form research with Claude's web_search + web_fetch + extended thinking.

CAPABILITY:
- Synchronous — Claude runs internal `web_search` + `web_fetch` in a loop with extended thinking.
- Reads full pages, cross-references claims.
- Single Anthropic call (with internal tool use).

OUTPUT:
- `{ answer, citations: [{ url, title, snippet }], thinking? }`.
- Citations align with `[1]`-style markers in the answer.

LIMITS & COST:
- Medium-high latency (tens of seconds to a few minutes).
- Anthropic API quota; cheaper than OpenAI deep research.
- Bounded by Claude's max-tokens-per-call — for truly massive surveys prefer deep-research-with-openai.

EXAMPLE PROMPTS:
- "Deep dive on the recent advances in neural machine translation"
- "Investigate why this open-source project is losing maintainers"
- "Cross-reference these three claims and tell me which are supported"
