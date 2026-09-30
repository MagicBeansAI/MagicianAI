---
name: websearch
version: 0.3.0
description: Search the web and return structured results (titles, URLs, snippets). Uses DuckDuckGo under
  the hood because Google blocks curl requests and requires a full browser session to render results.
  For Google-specific results, use the browser tool to navigate to google.com instead. On success this
  returns a JSON array. When DuckDuckGo cannot be reached, serves its bot challenge, or answers with a
  page carrying no results container, it returns an error object with kind and message fields instead
  of an empty array, so "nothing matched" and "the provider was unreachable" are never the same answer.
metadata:
  magician:
    requires:
      bins:
      - websearch
    install_hint:
      docs: 'requires binary on PATH: python3'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "rust programming language"
        num_results: 3
      expect:
        # A bare array is the success shape, so the whole document is the item
        # list. `error_pointer` is what makes the failure shape legible to the
        # lane: this skill has no `content_source` block for the runner to
        # borrow an error pointer from, so without this line an adapter that
        # answered with an error envelope would be reported as "0 items, no
        # error" — which is exactly how it went dark scraping a CAPTCHA page.
        min_items: 1
        items_pointer: ""
        error_pointer: "/error"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - websearch
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
          timeout_secs: 15
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
          description: Run the websearch capability with the arguments selected from this capability guide.
            Inspect stdout/stderr, update the runtime ledger when useful, and call goal_reached only after
            the requested result is present.
          parameters:
            query:
              type: string
              description: Search query (same as what you'd type into a search engine)
              required: true
              max_length: 4096
            num_results:
              type: integer
              description: Maximum number of results to return
              default: 10
          timeout_secs: 15
    runtime_catalog:
      categories:
      - web
      - search
      - research
      composition_category: web_operations
      expose_timeout_control: true
      timeout_default_secs: 15
---

# Websearch

Tool name: `websearch`
Primary parameter: `query`
Requires: curl and python3 installed on host PATH
Use for: researching topics, finding documentation, discovering products,
competitive analysis, finding tutorials, checking current information.

Returns JSON array of results, each with: title, url, snippet.

On failure it returns an object instead:

```json
{"error": {"kind": "HTTPError", "message": "HTTP 503: Service Unavailable"}}
```

The two shapes differ deliberately. An empty array means DuckDuckGo answered
and had nothing; an object means DuckDuckGo was not reached, served its bot
challenge, or returned a page with no results container. Treat the object as a
failure and fall back to `browser`; do not read it as "no matches".

NOTE: This tool uses DuckDuckGo because Google blocks non-browser HTTP
requests (returns JavaScript-only pages that cannot be parsed). For cases
where Google-specific results are needed, use the `browser` tool to
navigate to google.com and search interactively.

DuckDuckGo serves its "bots use DuckDuckGo too" challenge — HTTP 202, a CAPTCHA
page, zero results — to any client whose User-Agent is not a complete browser
string. This adapter sends one, detects the challenge page explicitly, retries
once with backoff, and reports an error if it persists rather than returning an
empty array.

Results are extracted structurally rather than by a single ordered regex: the
parser keys off the `//duckduckgo.com/l/?uddg=` redirect wrapper that every
organic result carries, plus the `result__a` / `result__snippet` class hooks,
and collects title and snippet independently. A result shipped without a
snippet is still returned, and sponsored blocks — which use a `y.js` link
rather than the redirect wrapper — are skipped without a separate ad rule.

The `num_results` parameter controls how many results to return (default 10).

For ordinary agent research, prefer the unified `content_search` controller.
To read the full content of a returned result, pass its candidate and receipt
to `content_read`; use `browser` only after a typed browser handoff.

Examples:
- {"query":"best rust web framework 2025"}
- {"query":"stripe api create subscription","num_results":"5"}
- {"query":"how to deploy static site cloudflare pages"}
- {"query":"micro saas ideas profitable 2026","num_results":"15"}
