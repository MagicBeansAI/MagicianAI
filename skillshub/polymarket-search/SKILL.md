---
name: polymarket-search
version: 0.2.0
description: 'Search active prediction markets on Polymarket matching a topic.

  Uses Polymarket''s Gamma API — keyless. Returns normalized items

  with volume + liquidity (engagement). Use for market sentiment /

  probability estimates on event-shaped questions (elections,

  product launches, geopolitical events). Returns ACTIVE markets

  only.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: gamma-api.polymarket.com
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - polymarket-search
    install_hint:
      docs: Requires Python 3.9+. Zero credentials. Gamma API is public.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "election"
        limit: 3
      expect:
        min_items: 1
        items_pointer: "/items"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - polymarket-search
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
          description: 'Search Polymarket for active prediction markets matching a query.

            Uses Polymarket''s Gamma API — keyless. Returns normalized items

            with volume + liquidity signals. Use to find market sentiment /

            probability estimates on event-shaped questions (elections,

            product launches, geopolitical events, etc.). Returns ACTIVE

            markets only (no closed / resolved markets).

            '
          parameters:
            query:
              type: string
              description: Topic / keywords
              required: true
              max_length: 4096
            limit:
              type: integer
              description: Value for limit.
              default: 10
          timeout_secs: 60
    runtime_catalog:
      categories:
      - research
      - search
      - prediction-markets
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 60
---

# polymarket-search — active markets matching a topic

## When to use

- "What's Polymarket pricing X event at?"
- Probability estimates on real-world events
- Market sentiment for binary-outcome questions

## Parameters

| Param | Type | Default |
|---|---|---|
| `query` | string | (required) |
| `limit` | int | 10 |

(no `days` param — Polymarket's relevant markets are inherently forward-looking, not date-bounded.)

## Output

Envelope: `{query, source, to_date, items, count, status, reason, duration_ms}`.
Per-item engagement: `{volume, liquidity}` (both USD-denominated).

## CLI

```bash
polymarket-search "presidential election 2028" --limit 5 --pretty
```
