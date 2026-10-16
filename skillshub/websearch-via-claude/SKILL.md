---
name: "websearch-via-claude"
version: 0.2.3
description: "Quick LLM-curated web search via Claude's built-in `web_search` tool. Pick when the user wants an answered question with sources and benefits from Claude's longer reasoning chains or context coherence. Pick over `websearch-via-openai` when the answer must integrate prior chat context."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.anthropic.com
    requires:
      bins: ["claude-websearch"]
      env: ["ANTHROPIC_API_KEY"]
    install_hint:
      docs: "requires env: ANTHROPIC_API_KEY — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        query: "What is the Rust programming language?"
      expect:
        min_items: 1
        items_pointer: "/sources"
        require_pointers: ["/answer"]
        max_latency_ms: 120000
        max_cost_microunits: 50000
        # Ceilings are commodity-scoped. This one bounds real money; the
        # runner compares it only against a package that reports `usd`, so
        # it can never be read against a provider-credit figure.
        max_cost_commodity: usd
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [claude-websearch]
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
          # bin/claude-websearch declares PROVIDER_TIMEOUT_SECS 60 and
          # MAX_PAUSE_CONTINUATIONS 4, so its own deadline budget is
          # 60 * (4 + 1) = 300, plus a 5s margin for interpreter start, stdin
          # read, and normalization => 305. This was 60, below the 300 it was
          # meant to back stop, which made every pause-turn and timeout error
          # path in the adapter unreachable. YAML cannot reference Python, so
          # the adapter mirrors this number as GOVERNED_KILL_CEILING_SECS and
          # phase7_migration.rs pins the two together.
          timeout_secs: 305
          stdin_bytes: 65536
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: anthropic_api_key
            secret_ref: ANTHROPIC_API_KEY
        injections:
          - source: {kind: secret, binding: anthropic_api_key}
            target: {kind: environment, name: ANTHROPIC_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded Claude Messages API web search.
          parameters:
            query:
              type: string
              description: Research query in natural language.
              required: true
              min_length: 1
              max_length: 4096
            model:
              type: string
              description: Claude model identifier.
              default: claude-haiku-4-5-20251001
              min_length: 1
              max_length: 256
            max_searches:
              type: integer
              description: Maximum web-search uses for this quick-search request.
              default: 3
              minimum: 1
              maximum: 20
            allowed_domains:
              type: string
              description: Optional comma-separated domain/path allowlist (maximum 100 entries).
              max_length: 4096
    runtime_catalog:
      categories: [web, search, research, llm]
      composition_category: web_operations
      expose_timeout_control: true
---

# Websearch Via Claude

Quick web search with Claude's web_search tool.

The active runtime resolves `ANTHROPIC_API_KEY` from the canonical scoped secret
authority only after execution authorization. Existing private scoped `.env`
installations remain a migration bridge when no vault record exists; a vault record
always wins. The key is injected into the self-contained `claude-websearch` adapter
and is never accepted from model input.

CAPABILITY:
- Single Anthropic call with internal `web_search` tool use.
- Reads search results and synthesizes a cited answer.

OUTPUT:
- `{ answer, sources: [{ url, title, cited_text? }], model, usage }`.
- Sources combine search results with Claude's final citation metadata.

LIMITS & COST:
- Low-medium latency (~3–10s).
- Anthropic API quota.
- Not for multi-step reports (deep-research-with-claude) or breaking news (news-search-via-tavily).

EXAMPLE PROMPTS:
- "What's the best way to set up a SwiftUI app today?"
- "Explain the difference between WASM and WebGPU for ML inference"
- "Find recent benchmarks for LLM inference on Apple Silicon"
