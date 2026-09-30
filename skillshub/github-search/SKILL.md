---
name: github-search
version: 0.4.0
description: |
  Search GitHub for recent issues/PRs and repositories matching a
  query. Uses GitHub's REST search-issues + search-repositories APIs.
  Returns normalized items with engagement signals (comments +
  reactions for issues; stars + forks for repos). Works keyless
  (10 req/min anonymous limit); set `GITHUB_TOKEN` to raise to
  30 req/min. Use for developer activity, project launches, library
  trends, and "is anyone building this?" research.
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.github.com
    content_source:
      schema_version: 1

      adapter:
        id: github
        display_name: GitHub search
        class: community
        execution: remote_endpoint
        auth: optional
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 100
        retrieval:
          action_id: github.discover
          operation: discover
          rung: source_native
          priority: 300
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: github-search
        action: run

      input:
        query_argument: query
        limit_argument: limit
        max_query_chars: 8192
        options:
          days:
            argument: days
            value_type: positive_integer
          mode:
            argument: mode
            value_type: string
            allowed: [mixed, issues, repos]

      output:
        mode: mapped
        error_pointer: /reason
        items_pointer: /items
        item:
          source_item_id_pointer: /source_native_id
          title_pointer: /title
          url_pointer: /url
          cheap_text_pointers: [/snippet]
          published_at_pointer: /published_at
          source_label_pointer: /source
          metadata:
            engagement: /engagement
            author: /author
            container: /container
    skill_type: tool
    user_invocable: true
    requires:
      bins: ["github-search"]
      env: ["GITHUB_TOKEN"]
    install_hint:
      docs: |
        Requires Python 3.9+. Works keyless with 10 req/min anonymous
        limit. Set optional `GITHUB_TOKEN` in operator-config.yaml
        (`secrets.GITHUB_TOKEN`) and run `make -C skillshub setup-env`
        for 30 req/min.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "rust"
        limit: 3
        days: 30
      expect:
        min_items: 1
        items_pointer: "/items"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [github-search]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: denied}
        limits:
          timeout_secs: 60
          stdin_bytes: 32768
          stdout_bytes: 8388608
          stderr_bytes: 1048576
      auth:
        kind: secrets
        requirement: optional
        secret_bindings:
          - {name: github_token, secret_ref: GITHUB_TOKEN}
        injections:
          - source: {kind: secret, binding: github_token}
            target: {kind: environment, name: GITHUB_TOKEN}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Search recent GitHub issues, pull requests, and repositories.
          parameters:
            query:
              type: string
              description: Search query.
              required: true
              min_length: 1
              max_length: 4096
            days:
              type: integer
              description: Lookback window in days.
              default: 3
              minimum: 1
              maximum: 3650
            limit:
              type: integer
              description: Maximum combined result count.
              default: 10
              minimum: 1
              maximum: 30
            mode:
              type: string
              description: Search issues and repositories, issues only, or repositories only.
              default: mixed
              enum_values: [mixed, issues, repos]
    runtime_catalog:
      categories: [research, search, code]
      composition_category: research
      expose_timeout_control: true
---

# github-search — recent GitHub activity matching a query

## When to use

- "What developers are building around X this week"
- New library / project discovery
- Issue/PR activity on existing repos

## Parameters

| Param | Type | Default | Notes |
|---|---|---|---|
| `query` | string | (required) | Search query. |
| `days` | int | 3 | Lookback window. |
| `limit` | int | 10 | Total items across modes. In `mixed` mode, 60% issues / 40% repos. |
| `mode` | enum | `mixed` | `mixed` (default), `issues` (only issues/PRs), `repos` (only repositories). |

## Output

Envelope: `{query, source, mode, from_date, to_date, items, count, status, reason, duration_ms}`.

Per-item engagement varies by what was matched:
- Issues/PRs: `{comments, reactions_total}`
- Repos: `{stars, forks, open_issues}`

Stderr: `[github-search] status=ok count=N duration_ms=M mode=mixed`.

## Failure modes

- HTTP 403 with `X-RateLimit-Remaining: 0` → anonymous rate limit hit. Set `secrets.GITHUB_TOKEN` in `$MAGICIAN_ROOT_DIR/operator-config.yaml`, then run `make -C skillshub setup-env` to triple your budget.
- HTTP 422 → query syntax issue (rare; GitHub's query syntax is forgiving).

## CLI

```bash
github-search "agent framework" --days 7 --limit 15 --pretty
github-search "claude code" --mode repos --limit 20
```
