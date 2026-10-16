---
name: "websearch-via-openai"
version: 0.2.3
description: "Quick LLM-curated web search via OpenAI's built-in `web_search` tool (Responses API). Use when the caller explicitly wants an OpenAI-authored answer with sources but not a long investigation. Ordinary agent research should use `content_search` and synthesize from controller evidence."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.openai.com
    requires:
      bins: ["openai-websearch"]
      env: ["OPENAI_API_KEY"]
    install_hint:
      docs: "requires env: OPENAI_API_KEY — set in vault before activating"
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
        bins: [openai-websearch]
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
          # bin/openai-websearch issues one request at PROVIDER_TIMEOUT_SECS 60,
          # so its own deadline budget is 60, plus a 5s margin for interpreter
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
          - name: openai_api_key
            secret_ref: OPENAI_API_KEY
        injections:
          - source: {kind: secret, binding: openai_api_key}
            target: {kind: environment, name: OPENAI_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Run a bounded OpenAI Responses API web search.
          parameters:
            query:
              type: string
              description: Search query in natural language.
              required: true
              min_length: 1
              max_length: 4096
            model:
              type: string
              description: OpenAI model identifier; common choices include gpt-4o-mini, gpt-4o, and o4-mini.
              default: gpt-4o-mini
              min_length: 1
              max_length: 256
            allowed_domains:
              type: string
              description: Optional comma-separated domain allowlist (maximum 100 bare domains or HTTP(S) URLs).
              max_length: 4096
    runtime_catalog:
      categories: [web, search, research, llm]
      composition_category: web_operations
      expose_timeout_control: true
---

# Websearch Via Openai

Quick web search via OpenAI Responses API web_search tool.

The active runtime resolves `OPENAI_API_KEY` from the canonical scoped secret
authority only after execution authorization. Existing private scoped `.env`
installations remain a migration bridge when no vault record exists; a vault
record always wins. The key is injected into the self-contained
`openai-websearch` adapter and is never accepted from model input.

CAPABILITY:
- Single Responses-API call with internal `web_search`.
- Returns synthesized answer + source annotations.
- Lower-latency than deep research; no multi-step reasoning loop.

OUTPUT:
- `{ answer, sources: [{ url, title }], model, usage }`.
- `sources` combines cited URLs and the bounded complete source list returned by the Responses API.

LIMITS & COST:
- Low-medium latency (~3–8s).
- OpenAI API quota.
- Recency = model freshness + live results.
- Not for multi-step reports (deep-research-with-openai) or breaking news (news-search-via-tavily).

EXAMPLE PROMPTS:
- "When did Apple announce M5?"
- "What are the key features of the new Python release?"
- "Summarize the latest WHO statement on bird flu"
