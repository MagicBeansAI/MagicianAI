---
name: producthunt-search
version: 0.3.0
description: 'Search Product Hunt for recent launches matching a query. Uses PH''s

  public Atom feed — keyless. Returns normalized items with title,

  tagline, and maker. PH''s v2 GraphQL API is now OAuth-only, so vote and

  comment counts are unavailable and `engagement` comes back empty rather

  than zeroed. Use for product launches, new tool discoveries, and

  "what''s new in X category". The feed is a fixed window of the 50 most

  recent launches; for deeper history fall back to the `browser` skill

  without passing `connection_mode` so the runtime uses its default

  profile-reusing mode against each daily leaderboard page.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: www.producthunt.com
    content_source:
      schema_version: 1

      adapter:
        id: product-hunt
        display_name: Product Hunt
        class: community
        execution: remote_endpoint
        auth: none
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 100
        retrieval:
          action_id: product_hunt.discover
          operation: discover
          rung: source_native
          priority: 400
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: producthunt-search
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
    observe_source:
      schema_version: 1

      source:
        id: product-hunt
        display_name: Product Hunt
        category: products
        description: New products and launches from the public Product Hunt feed

      profiles:
        - id: observe-rss
          surfaces: [observe]
          discoverable: true
          unattended: true
          read_only: true
          operation: discover
          acquisition:
            allowed_actions: [rss.discover]
            escalation: none
            targets:
              - https://www.producthunt.com/feed
          schedule:
            default: hourly
            allowed: [hourly, twice_daily, daily]
          limits:
            max_candidates_per_run: 50
            max_selected_per_run: 10

        - id: interactive-research
          surfaces: [research]
          discoverable: false
          unattended: false
          read_only: true
          operation: discover
          acquisition:
            allowed_actions: []
            escalation: ladder
            targets: []
            ladder: default_discover
            maximum_authority: public_browser_read
          schedule:
            default: daily
            allowed: [daily]
          limits:
            max_candidates_per_run: 50
            max_selected_per_run: 10
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - producthunt-search
    install_hint:
      docs: 'Requires Python 3.9+. Zero credentials. Product Hunt''s v2 GraphQL API
        is no longer public — every anonymous request returns HTTP 401
        invalid_oauth_token — so this rides the public Atom feed at
        https://www.producthunt.com/feed, which is still open and needs no key.
        The feed carries no vote or comment counts and only the 50 most recent
        launches; recovering counts, server-side search, or deeper history would
        need a Product Hunt developer application token (Bearer, from
        https://www.producthunt.com/v2/oauth/applications), which this package
        deliberately does not require. For deeper history without a token, fall
        back to the browser skill without passing connection_mode against each
        /leaderboard/daily/YYYY-MM-DD page in the requested days window.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "ai"
        limit: 3
        days: 30
      expect:
        # `error_pointer` is inherited from content_source.output (/reason),
        # which is what surfaced the HTTP 401 that took this skill dark rather
        # than letting it read as an empty result set. Left implicit so the
        # canary keeps asserting against the pointer the product consumes.
        min_items: 1
        items_pointer: "/items"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - producthunt-search
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
          description: 'Search Product Hunt for recent launches matching a query.

            Uses PH''s public Atom feed (keyless). Returns normalized items

            with title, tagline, maker, and launch time. PH''s v2 GraphQL API

            is OAuth-only now, so there are no vote or comment counts:

            `engagement` is an empty object and must not be read as zero

            engagement or used for ranking.

            The feed is a fixed window of the 50 most recent launches. The

            envelope reports `provider_entries` and `provider_window` so a

            zero-item answer is attributable: zero items with

            provider_entries > 0 means the query or the days window excluded

            everything, while status=failed with an `error` means the feed was

            not reached.

            For deeper history than the feed holds, or when vote counts

            matter, fall back to the `browser` skill without passing

            connection_mode so the runtime uses its default profile-reusing

            mode, scraping each daily leaderboard page in the requested UTC

            days window:

            https://www.producthunt.com/leaderboard/daily/YYYY-MM-DD.

            Use the same window as this tool: from today - days through today,

            inclusive, then dedupe by post URL and cap to limit.

            '
          parameters:
            query:
              type: string
              description: Topic / keywords
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
      - products
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 60
---

# producthunt-search — recent PH posts

## When to use

- "Newest AI video tools / dev tools / X category"
- Track product launches in a specific topic
- See what's hot on PH for the last few days

## Parameters

| Param | Type | Default |
|---|---|---|
| `query` | string | (required) |
| `days` | int | 3 |
| `limit` | int | 10 |

## Transport

Product Hunt's v2 GraphQL API is no longer public. Every anonymous request
answers:

```
HTTP 401
{"data":null,"errors":[{"error":"invalid_oauth_token",
 "error_description":"Please supply a valid access token. …"}]}
```

That is an authentication requirement, not the transient throttle this package
used to claim. The public Atom feed serves the same launches to anyone, so it
is the transport:

```
https://www.producthunt.com/feed?category=all
```

What that costs: no vote or comment counts, no topic labels, no server-side
search, and a fixed window of the 50 most recent launches. Query matching runs
locally over each launch's title and tagline — long terms match as substrings,
short terms as whole words, so a two-letter category does not match every word
containing it.

## Output

Envelope: `{query, source, from_date, to_date, items, count, provider_entries,
provider_window, status, reason, error, duration_ms}`.

- `engagement` is an empty object. The feed has no vote data, and
  `{"votes": 0, "comments": 0}` would be indistinguishable from a launch nobody
  upvoted. Do not rank on it.
- `container` is null. The feed carries no topic labels.
- `provider_entries` — launches the feed served before local filtering.
- `provider_window` — `{oldest, newest, entries}`, the span the feed actually
  covered. When `days` reaches further back than the feed does, this is how you
  see it rather than reading a short result as a thin week.

## Failure modes + fallback

Zero items is attributable, never bare:

- `status=ok`, `provider_entries > 0` → the query or the `days` window excluded
  everything the feed held. Broaden the query, or check `provider_window` to see
  whether `days` outran the feed.
- `status=ok`, `provider_entries: 0` → the feed itself was empty.
- `status=failed` with `error: {kind, message}` → the feed was not reached or did
  not parse. This is the only outcome that means the provider is down.

To recover vote counts or search deeper than the feed reaches, either supply a
Product Hunt developer token (see `install_hint`) or use the browser fallback:

```
res = producthunt-search(query="AI video", days=3)
if res["status"] == "failed":
    # Scrape via browser without connection_mode; the runtime uses its default profile-reusing mode.
    # Match the tool's UTC lookback window: from today - days through today, inclusive.
    ph_items = []
    for date_iso in utc_date_range(utc_today() - days, utc_today(), inclusive=True):
        ph_html = browser(url=f"https://www.producthunt.com/leaderboard/daily/{date_iso}")
        ph_items.extend(extract_producthunt_items(ph_html, query="AI video"))

    # Dedupe by Product Hunt post URL, sort by votes/comments + recency, then cap to limit.
    ph_items = rank_and_limit(dedupe_by_url(ph_items), limit=10)
```

## CLI

```bash
producthunt-search "AI video" --days 3 --limit 15 --pretty
```
