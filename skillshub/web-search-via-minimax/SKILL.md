---
name: web-search-via-minimax
version: 0.2.2
description: Web search via MiniMax's search API (`mmx search query`). A direct MiniMax diagnostic or
  explicitly requested provider path. Returns organic results (title, link, snippet, date) as normalized
  JSON; ordinary research should use `content_search` and `content_read`.
compatibility: Requires a vault-secret MiniMax API key. Network call to api.minimax.io.
metadata:
  magician:
    # Apps reach only this host through the call's broker, and the owner's
    # MiniMax key is scoped to it (`app_egress`, data only). The API host is
    # the one `mmx` was observed to contact; other hosts (file CDNs) are not
    # declared and stay unreachable from an app.
    app_egress:
      schema_version: 1
      destinations:
      - api.minimax.io
    requires:
      bins:
      - minimax-websearch
      - mmx
      # `mmx` is `#!/usr/bin/env node`, so the interpreter is part of what has
      # to be reachable. The governed child gets a cleared environment whose
      # PATH is built only from the directories that resolve this list, and
      # without `node` on it every call died before reaching the provider with
      # `env: node: No such file or directory` (exit 127). The runtime vendors
      # its own Node at `skillshub/.node/bin` (`make -C skillshub setup-node`),
      # which is one of the dependency roots the governed PATH resolves, so
      # naming it here binds the project-local interpreter rather than whatever
      # brew/nvm/fnm the operator happens to have. `whatsapp` declares it the
      # same way for the same reason.
      #
      # Like `mmx`, this is a PATH companion and not the entrypoint: it is
      # never bound as execution authority and never snapshotted, so a host
      # without it loses these calls with the CLI's own message rather than
      # losing the skill.
      - node
    install_hint:
      docs: ships with the runtime — set the MiniMax API secret in the vault before activating. The `mmx`
        CLI is vendored via `skillshub/web-search-via-minimax/package.json` (npm workspace), installed
        alongside the rest of skillshub's node deps. `mmx` runs under Node, which the
        runtime vendors at `skillshub/.node/bin` via `make -C skillshub setup-node`.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        query: "rust programming language"
      expect:
        min_items: 1
        items_pointer: "/results"
        error_pointer: "/error"
        max_latency_ms: 60000
        max_cost_microunits: 20000
        # Ceilings are commodity-scoped. This one bounds real money; the
        # runner compares it only against a package that reports `usd`, so
        # it can never be read against a provider-credit figure.
        max_cost_commodity: usd
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - minimax-websearch
        - mmx
        - node
        entrypoint: minimax-websearch
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
          timeout_secs: 60
          stdin_bytes: 1048576
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        provider: minimax
        secret_bindings:
        - name: MINIMAX_API_KEY
          secret_ref: MINIMAX_API_KEY
        injections:
        - source:
            kind: secret
            binding: MINIMAX_API_KEY
          target:
            kind: config_directory
            name: MMX_CONFIG_DIR
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: 'Web search via MiniMax''s search API (`mmx search query`). Returns

            organic results — title, link, snippet, and (when available)

            publication date — as a normalized JSON envelope.


            A general keyword/web-search tier alongside `websearch` (DuckDuckGo)

            and `news-search-via-tavily`: a MiniMax-indexed alternative for

            simple-fact / broad-keyword queries and a cross-engine second opinion

            when one engine''s snippets come back thin. Snippet-only — follow up

            with `htmltotext` on the result URLs to read the actual pages before

            citing. NOT a recency-filtered news tool (use `news-search-via-tavily`)

            or a semantic/academic tool (use `semantic-websearch-via-exa`).

            '
          parameters:
            query:
              type: string
              description: Web search query string.
              required: true
              max_length: 4096
            max_results:
              type: integer
              description: Maximum number of organic results to return (1-20). Default 10.
              default: 10
          timeout_secs: 60
    runtime_catalog:
      categories:
      - web
      - search
      - research
      composition_category: web_operations
      expose_timeout_control: true
      timeout_default_secs: 60
---

# Web search (MiniMax)

General web search via MiniMax's search API. Subprocess-wraps the `mmx`
CLI (`mmx search query --q "<query>" --output json`) — the same pattern as
the other `*-via-minimax` skills (image/video/music) — and normalizes the
result into a stable JSON envelope.

## When to use this skill

A keyword/web-search tier — same job as `websearch` (free DuckDuckGo) and
the discovery side of `news-search-via-tavily`, but backed by MiniMax's
index. Reach for it when:

1. **Simple fact / broad keyword** query where you want a MiniMax-indexed
   result set — it often surfaces slightly different sources than DuckDuckGo.
2. **Cross-engine second opinion** — one engine's snippets came back thin,
   stale, or off-topic; run the same query here against a different index.

Do NOT use this skill for:
- **News / time-sensitive / finance** with recency filters → use
  `news-search-via-tavily` (topic + time_range).
- **Conceptual / semantic / academic / niche-domain** queries → use
  `semantic-websearch-via-exa`.
- **A direct cited answer in one call** → use `websearch-via-openai` only
  when the caller explicitly wants provider-authored synthesis.

## Output

```json
{
  "query": "…",
  "count": 8,
  "results": [
    { "title": "…", "url": "https://…", "snippet": "…", "date": "2 days ago" }
  ]
}
```

Snippet-only — this tool finds URLs. For ordinary research, pass selected
results through `content_read` before citing them; never try to read the
search-result page itself.

## Parameters

- `query` (required) — the search string.
- `max_results` (default 10) — caps the organic results returned (1-20).
- `timeout_secs` (default 30) — upstream request timeout.

## Auth

The package consumes a vault secret through a governed
`ConfigDirectory` named `MMX_CONFIG_DIR`. The runtime writes
`{ "api_key": "<secret>" }` into
`<MMX_CONFIG_DIR>/config.json`, and `mmx` reads credentials from that
directory. The `mmx` CLI is vendored via this skill's `package.json`
(npm workspace), so the `mmx` bin lands on the dispatcher's PATH.
