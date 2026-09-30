---
name: hackernews-search
version: 0.2.0
description: 'Search Hacker News for recent stories and comments matching a query.

  Returns normalized items with points + comment counts. Uses HN''s

  Algolia search-by-date API — keyless. Use for developer / tech

  industry sentiment, launch announcements, and "Show HN" discoveries.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: hn.algolia.com
    content_source:
      schema_version: 1

      adapter:
        id: hacker-news
        display_name: Hacker News
        class: community
        execution: remote_endpoint
        auth: none
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 100
        retrieval:
          action_id: hacker_news.discover
          operation: discover
          rung: source_native
          priority: 500
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: hackernews-search
        action: run

      input:
        query_argument: query
        limit_argument: limit
        max_query_chars: 8192
        options:
          days:
            argument: days
            value_type: positive_integer

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
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - hackernews-search
    install_hint:
      docs: Requires Python 3.9+. Zero credentials. Algolia HN endpoint is keyless.
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
        bins:
        - hackernews-search
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
          description: 'Search Hacker News for recent stories + comments matching a query.

            Uses HN''s Algolia search-by-date API — keyless. Returns

            normalized items with points + comment counts.

            '
          parameters:
            query:
              type: string
              description: Search query
              required: true
              max_length: 4096
            days:
              type: integer
              description: Value for days.
              default: 3
            limit:
              type: integer
              description: Value for limit.
              default: 10
          timeout_secs: 60
    runtime_catalog:
      categories:
      - research
      - search
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 60
---

# hackernews-search — recent HN stories matching a query

Backed by `https://hn.algolia.com/api/v1/search_by_date`. Always sorts by date, applies an exact `created_at_i` range from `numericFilters` so the time window is honored at the source.

## Parameters

| Param | Type | Default |
|---|---|---|
| `query` | string | (required) |
| `days` | int | 3 |
| `limit` | int | 10 |

## Output

Same envelope shape as `reddit-search` etc. — `{query, source, from_date, to_date, items, count, status, reason, duration_ms}`. Each item: `{title, url, snippet, published_at, source, source_native_id, engagement: {points, comments}, author, container: null}`.

Stderr: `[hackernews-search] status=ok count=N duration_ms=M`.

## CLI

```bash
hackernews-search "AI agents" --days 7 --limit 20 --pretty
```
