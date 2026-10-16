---
name: reddit-search
version: 0.3.2
description: 'Search Reddit for recent posts matching a query. Returns normalized

  items with title, body text, author, subreddit, and real engagement metrics

  when optional Reddit application credentials are configured. Authenticated

  OAuth JSON is preferred; the public Atom feed remains the keyless fallback

  and explicitly marks engagement unavailable instead of inventing zeroes.

  Use for community sentiment, discussion threads, and grassroots

  reactions to a topic. If Reddit rate-limits the feed, fall back to the

  `browser` skill in the runtime''s default connection mode. For broader

  web discovery use the unified `content_search` controller.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    # The HTTPS hosts this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted these destinations.
    app_egress:
      schema_version: 1
      destinations:
      - oauth.reddit.com
      - www.reddit.com
    content_source:
      schema_version: 1

      adapter:
        id: reddit
        display_name: Reddit search
        class: community
        execution: remote_endpoint
        auth: optional
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 100
        retrieval:
          action_id: reddit.discover
          operation: discover
          rung: source_native
          priority: 200
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: reddit-search
        action: run

      input:
        query_argument: query
        limit_argument: limit
        max_query_chars: 8192
        options:
          days:
            argument: days
            value_type: positive_integer
          subreddit:
            argument: subreddit
            value_type: string

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
      bins:
      - reddit-search
      env:
      - REDDIT_CLIENT_ID
      - REDDIT_CLIENT_SECRET
    install_hint:
      docs: 'Requires Python 3.9+. Works keyless through Reddit''s public Atom

        search feed, but that transport has no engagement counts. For full

        results, create a Reddit application and set REDDIT_CLIENT_ID plus

        REDDIT_CLIENT_SECRET in the scoped secret vault. The governed runtime

        injects the pair only for this process; the adapter exchanges it for a

        short-lived application token and never persists that token.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "rust"
        limit: 3
        days: 30
      expect:
        # `error_pointer` is inherited from content_source.output (/reason),
        # which is what surfaced the HTTP 403 that took this skill dark rather
        # than letting it read as an empty result set. Left implicit so the
        # canary keeps asserting against the pointer the product consumes.
        min_items: 1
        items_pointer: "/items"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - reddit-search
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
        requirement: optional
        provider: reddit
        secret_bindings:
        - name: reddit_client_id
          secret_ref: REDDIT_CLIENT_ID
        - name: reddit_client_secret
          secret_ref: REDDIT_CLIENT_SECRET
        injections:
        - source:
            kind: secret
            binding: reddit_client_id
          target:
            kind: environment
            name: REDDIT_CLIENT_ID
        - source:
            kind: secret
            binding: reddit_client_secret
          target:
            kind: environment
            name: REDDIT_CLIENT_SECRET
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: "Search Reddit for recent posts matching a query. Returns normalized content and real score, upvote, ratio, and comment metrics when optional Reddit application credentials are available. Authenticated OAuth JSON is preferred. The keyless Atom fallback remains usable but marks each engagement object unavailable and reports its transport/degraded reason; missing metrics are never represented as zero."
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
              minimum: 1
              maximum: 3650
            limit:
              type: integer
              description: Value for limit.
              default: 10
              minimum: 1
              maximum: 100
            subreddit:
              type: string
              description: Value for subreddit.
              default: ''
              max_length: 4096
          timeout_secs: 60
    runtime_catalog:
      categories:
      - research
      - search
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 60
---

# reddit-search — recent Reddit posts matching a query

## When to use

- "What are people saying on Reddit about X?"
- Community sentiment / first-impression reactions
- Subreddit-targeted research (`subreddit` param)

## Parameters

| Param | Type | Default | Notes |
|---|---|---|---|
| `query` | string | (required) | Search query. Reddit's native search runs against this. |
| `days` | int | 3 | Lookback window. Reddit's date filter granularity is coarse (day/week/month); items are post-filtered to exact range. |
| `limit` | int | 10 | Max items returned. |
| `subreddit` | string | `""` | Optional: restrict to a single subreddit (e.g. `rust`, `MachineLearning`). |

## Transport

Prefer authenticated Reddit JSON. Configure `REDDIT_CLIENT_ID` and
`REDDIT_CLIENT_SECRET` as one pair. The adapter uses the application-only
`client_credentials` grant, keeps the resulting bearer in memory for this call
only, and reads search results from `oauth.reddit.com`. This transport supplies
score, upvotes, upvote ratio, and comment count.

Unauthenticated `search.json` on `www.reddit.com`, `old.reddit.com`,
`api.reddit.com`, and `oauth.reddit.com` answers

```
HTTP 403 Blocked  (text/html block page)
```

for every User-Agent tried — the descriptive agent Reddit's own API rules ask
for, and a full browser string alike. This is not a rate limit and not a
User-Agent problem.

The Atom search feed for the same query remains the keyless fallback:

```
https://www.reddit.com/search.rss?q=<query>&sort=new&t=<window>&type=link
```

`type=link` is required. Without it Reddit's search feed returns matching
*subreddits* (`t5_…`), not posts, and every one would normalize into something
that looks like a post and is not.

Atom carries no score, upvote, ratio, or comment count. Those fields are
explicitly unavailable, not zero. A temporary OAuth/provider failure also
falls back to Atom while preserving `transport: atom_fallback` and a bounded,
non-secret `degraded_reason`.

## Output shape

```json
{
  "query": "...",
  "source": "reddit",
  "from_date": "ISO date",
  "to_date":   "ISO date",
  "items": [
    {
      "title":            "string",
      "url":              "https://www.reddit.com/r/.../comments/...",
      "snippet":          "post body, feed attribution footer stripped",
      "published_at":     "ISO 8601 with timezone",
      "source":           "reddit",
      "source_native_id": "post id",
      "engagement": {
        "available": true,
        "upvotes": 421,
        "score": 417,
        "comments": 83,
        "upvote_ratio": 0.96,
        "observed_at": "ISO 8601"
      },
      "author":           "string",
      "container":        "subreddit name"
    }
  ],
  "count": N,
  "provider_entries": N,
  "transport": "oauth_json" | "atom_keyless" | "atom_fallback",
  "engagement_status": "available" | "unavailable",
  "degraded_reason": null | "oauth_credentials_not_configured" | "...",
  "status": "ok" | "failed",
  "reason": null | "...",
  "error": null | {"kind": "...", "message": "..."},
  "duration_ms": N
}
```

On Atom results, `engagement` instead contains
`{"available": false, "reason": "atom_feed_omits_metrics", "observed_at": "..."}`.
Do not rank unavailable engagement as zero. `observed_at` matters because Reddit
scores change after discovery.

Stderr: one structured progress line per call:
```
[reddit_search] status=ok count=10 provider_entries=20 transport=oauth_json engagement=available duration_ms=1240
```

## Failure modes

- OAuth HTTP/provider failure → one Atom fallback attempt; successful content
  remains `status=ok` with `engagement_status=unavailable` and a bounded
  `degraded_reason`.
- Only one OAuth secret configured → Atom fallback with
  `oauth_credentials_incomplete`; the partial pair is never sent.
- HTTP 429 → the selected transport is rate-limited. The adapter retries once
  after a short backoff; Atom remains the final availability fallback.
- HTTP 503 → transient. Retry.
- Body does not parse as Atom → the request was intercepted. Reported as
  `status=failed` with an `error`, never as an empty result set.
- Empty `items` with `status=ok` and `provider_entries: 0` → Reddit matched
  nothing. Broaden the query.
- Empty `items` with `status=ok` and `provider_entries > 0` → the `days` window
  dropped everything Reddit matched. Increase `days`.

## Browser fallback

When Atom is also rate-limited and no OAuth transport is available, call the
browser tool without a
`connection_mode` argument so the runtime uses its default profile-reusing mode:

```
browser(url="https://www.reddit.com/search/?q=<URL_ENCODED_QUERY>&sort=new")
```

## CLI use

```bash
printf '%s\n' '{"query":"AI video tools","days":7,"limit":15}' | reddit-search
printf '%s\n' '{"query":"rust async","subreddit":"rust","days":14}' | reddit-search
```
