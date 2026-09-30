---
name: "gif-search-via-klipy"
version: 0.2.1
description: "Find expressive GIFs and reaction images via the Klipy API. Pick when the user wants a reaction, celebration, sympathy, humor, or emotion GIF using keyword lookup. Not for generating new GIFs (use video gen) or template-driven meme overlays (`meme-generation-via-imgflip`)."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.klipy.com
    requires:
      bins: ["klipy-gif-search"]
      env: ["KLIPY_API_KEY"]
    install_hint:
      docs: "requires env: KLIPY_API_KEY — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        query: "celebration"
      expect:
        min_items: 1
        items_pointer: "/gifs"
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
        bins: [klipy-gif-search]
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
          # bin/klipy-gif-search issues one request at PROVIDER_TIMEOUT_SECS 15,
          # so its own deadline budget is 15, plus a 5s margin for interpreter
          # start, stdin read, and normalization => 20. This skill already
          # satisfied the invariant and is unchanged; the comment records the
          # arithmetic that was previously implicit. YAML cannot reference
          # Python, so the adapter mirrors this number as
          # GOVERNED_KILL_CEILING_SECS and phase7_migration.rs pins the two
          # together.
          timeout_secs: 20
          stdin_bytes: 65536
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: klipy_api_key
            secret_ref: KLIPY_API_KEY
        injections:
          - source: {kind: secret, binding: klipy_api_key}
            target: {kind: environment, name: KLIPY_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Search KLIPY for a bounded list of reaction GIFs.
          parameters:
            query:
              type: string
              description: Search keywords for GIF lookup.
              required: true
              min_length: 1
              max_length: 4096
            limit:
              type: string
              description: Maximum number of GIFs to return (1-20).
              default: "5"
              min_length: 1
              max_length: 64
            content_filter:
              type: string
              description: Content safety filter level.
              enum_values: ["off", pg, pg-13, r]
              default: pg
    runtime_catalog:
      categories: [gif, search, expression, creative]
      composition_category: media_operations
---

# Gif Search Via Klipy

GIF / reaction lookup via Klipy API.

The active runtime resolves `KLIPY_API_KEY` from the canonical scoped secret
authority only after execution authorization. Existing private scoped `.env`
installations remain a migration bridge when no vault record exists; a vault record
always wins. The key is injected into the self-contained `klipy-gif-search` adapter
and is never accepted from model input. KLIPY requires the credential in the request
path, so the adapter fixes the provider origin and denies redirects.

CAPABILITY:
- Free-text keyword search; ranked URL list.
- Bounded result-count control.
- Klipy library is curated for reactions, expressions, internet culture.

OUTPUT:
- `{ query, count, gifs: [{ url, preview, title, slug }] }`.
- Caller surfaces inline or offers as a pickable list.

LIMITS & COST:
- Low latency.
- Library biased toward reactions; weak coverage for niche or technical subjects.
- Not for generating new GIFs (use video-generation-via-veo) or template memes (meme-generation-via-imgflip).

EXAMPLE PROMPTS:
- "Find a celebration GIF for a launch"
- "Get me a 'this is fine' reaction GIF"
- "Search for shrug reaction GIFs"
