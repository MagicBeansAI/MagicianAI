---
name: whatsgoingon2
version: 0.2.0
description: 'Catch-up and situation-awareness research — what''s happening now,

  what happened recently, where a topic stands, what people are

  saying, what''s new across community / dev / product / market /

  news surfaces. The agent collects envelopes from per-source

  search tools (reddit-search / hackernews-search / github-search /

  producthunt-search / polymarket-search / youtube-search /

  news-search-via-tavily / browser) and passes them here. This

  tool dedups by URL, fuses per-source rankings via RRF, scores

  each item on RRF+engagement+freshness, optionally clusters

  near-duplicates by title similarity, and returns a unified

  ranked envelope. No LLM, no network — pure data transformation.

  Decisions about WHICH sources to call stay with the agent''s

  LLM; this tool just brings determinism + reproducibility to the

  cross-source merge.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - whatsgoingon2
    install_hint:
      docs: 'Requires Python 3.9+. No credentials. Pure data-transform tool;

        no external API calls. Companion to the source-search skills

        (reddit-search / hackernews-search / github-search /

        producthunt-search / polymarket-search / youtube-search /

        news-search-via-tavily) which the agent calls first.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it merges envelopes produced by other skills, so it has no
          standalone input; it is exercised through the skills that feed it.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - whatsgoingon2
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
          description: "Catch-up and situation-awareness research — what's happening\nnow, what happened\
            \ recently, where a topic stands, what people\nare saying, what's new across community / dev\
            \ / product /\nmarket / news surfaces. Deterministic merge of source-search\nenvelopes. This\
            \ tool does NOT fetch anything — it processes\nthe envelopes the agent has already collected\
            \ by calling the\nper-source tools (reddit-search / hackernews-search /\ngithub-search / producthunt-search\
            \ / polymarket-search /\nyoutube-search / news-search-via-tavily / browser, etc.).\n\nWhat\
            \ it does (in order):\n  1. Loads envelopes from the file path you supplied\n  2. Dedups items\
            \ by URL (normalized — trailing slash + scheme\n     tolerance)\n  3. RRF-fuses per-source\
            \ rankings into a unified pool\n     (Reciprocal Rank Fusion with per-source weight multipliers)\n\
            \  4. Scores each item: 0.5 * RRF + 0.3 * engagement_normalized\n     + 0.2 * freshness\n\
            \  5. Sorts descending by final_score\n  6. If cluster=true, groups near-duplicate items by\
            \ Jaccard\n     similarity on title tokens (threshold 0.5)\n  7. Truncates to `limit` items\n\
            \  8. Returns a unified JSON envelope on stdout\n\nNo LLM. No network. Pure data transformation.\
            \ The \"smart\"\ndecisions (which sources to call, which items to highlight in\nsynthesis)\
            \ stay with the agent's LLM. This tool just brings\ndeterminism + reproducibility to the cross-source\
            \ merge.\n\nSee SKILL.md for the full orchestration recipe (when to invoke,\nper-topic-shape\
            \ adjustments, browser-augmentation guidance).\n"
          parameters:
            query:
              type: string
              description: 'Merge mode: original research topic (required for merge mode)'
              max_length: 4096
            envelopes:
              type: string
              description: 'Merge mode: absolute path to JSON file containing source envelopes (required
                for merge mode)'
              max_length: 4096
            cluster:
              type: boolean
              description: Value for cluster.
              default: true
            limit:
              type: integer
              description: Value for limit.
              default: 60
            source_weights:
              type: string
              description: Optional JSON-encoded per-source weight overrides
              default: ''
              max_length: 4096
            normalize_urls:
              type: string
              description: 'Normalize-mode: absolute path to JSON list of URLs; outputs normalized URLs
                and exits'
              default: ''
              max_length: 4096
          timeout_secs: 60
    runtime_catalog:
      categories:
      - research
      - merge
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 60
---

# whatsgoingon2 — deterministic cross-source merge

## What this is

A pure data-transform tool that brings v1's deterministic processing back to the agent-orchestrated pipeline. The five source-search skills + `news-search-via-tavily` + `browser` are the data primitives; this is the merge engine that turns N source envelopes into one ranked + clustered pool.

What this tool does, in order:

1. **Dedup by normalized URL** — host lowercased, scheme stripped, trailing slash dropped, tracking params (`utm_*`, `fbclid`, `gclid`, `ref`) removed. Same article from Reddit + HN dedups to one item.
2. **Reciprocal Rank Fusion (RRF)** — sums `1 / (k + rank)` across sources, with optional per-source weight multipliers. Standard IR fusion technique; deterministic; no LLM needed.
3. **Score each survivor** — `0.5 × rrf_normalized + 0.3 × engagement_normalized + 0.2 × freshness_normalized`. Engagement uses log-compression (1k upvotes ≠ 1000x weight). Freshness linear-decays over 30 days.
4. **Sort descending** by final_score.
5. **Cluster near-duplicates** (optional, default on) — Jaccard similarity on title tokens, threshold 0.5. Returns cluster groupings alongside the flat list so the agent can see "these 4 items are about the same launch".
6. **Truncate to `limit`** — default 60.
7. **Emit JSON** envelope on stdout.

## What this is NOT

- Not a fetcher. It doesn't call source tools. The agent calls them first, then passes results here.
- Not a synthesizer. It doesn't write the user-facing prose. The agent does that after.
- Not an LLM call. Pure mathematical merge.

## Parameters

| Param | Type | Default | Notes |
|---|---|---|---|
| `query` | string | (required) | Original topic. Echoed in output envelope; used for context, not for ranking (RRF is rank-based, not query-relevance-based — relevance is the agent's job). |
| `envelopes` | string | (required) | Absolute path to a JSON file containing `[{source, items}, ...]`. Agent writes this file (typically via Bash) before calling. Absolute path because the backend process HOME and skill subprocess HOME differ. |
| `cluster` | bool-as-string | `"true"` | Whether to cluster the output by title similarity. |
| `limit` | int | 60 | Max items in output after dedup + ranking. |
| `source_weights` | JSON string | `""` | Optional per-source weight multipliers, e.g. `{"reddit": 1.2, "hackernews": 1.0}`. Higher = items from that source rank earlier when tied on RRF position. |

## Input envelope shape

The `envelopes` file is a JSON list. Each entry minimally needs `source` (string) and `items` (list). Items minimally need `title` + `url`; everything else is best-effort.

```json
[
  {
    "source": "reddit",
    "items": [
      {
        "title": "...", "url": "https://reddit.com/...", "snippet": "...",
        "published_at": "2026-05-15T10:00:00Z",
        "engagement": {"upvotes": 142, "comments": 12},
        "author": "user42", "container": "MachineLearning"
      },
      ...
    ]
  },
  {"source": "hackernews", "items": [...]},
  {"source": "github", "items": [...]}
]
```

This is what the source-search skills (reddit-search etc.) already return per-call — the agent's job is just to write them all to a single file before calling whatsgoingon2.

## Output envelope shape

```json
{
  "query": "...",
  "total_items": 47,
  "items": [
    {
      "title": "...", "url": "...", "snippet": "...",
      "published_at": "...", "source": "...", "source_native_id": "...",
      "engagement": {...}, "author": "...", "container": "...",
      "_sources": ["reddit", "hackernews"],      // sources this URL appeared in
      "_source_ranks": {"reddit": 1, "hackernews": 3},
      "_score_breakdown": {"rrf_norm": 0.92, "engagement_norm": 0.45,
                           "freshness_norm": 0.86, "final_score": 0.78}
    }, ...
  ],
  "source_summary": [
    {"source": "reddit", "input_count": 10, "contributed_to_final": 8},
    {"source": "hackernews", "input_count": 10, "contributed_to_final": 7},
    ...
  ],
  "dedup_summary": {
    "total_input_items": 50, "deduped_count": 3, "final_count": 47
  },
  "clusters": [
    {"representative_url": "...", "representative_title": "...",
     "member_count": 4, "member_urls": [...]},
    ...
  ],
  "duration_ms": 87
}
```

Stderr emits one structured line: `[whatsgoingon2] sources=N input_items=M deduped=D ranked=R returned=T clusters=C duration_ms=X`.

## Orchestration recipe (for the agent)

When the parent task is a "what's going on with X" research ask, follow this recipe:

### Step 1 — Decide topic shape

| Topic shape | Adjust from default |
|---|---|
| Pure code / library / framework | Drop producthunt-search; double github-search budget (`limit=20`) |
| Product launch / SaaS / consumer tool | Keep producthunt-search at top; drop polymarket-search; add `news-search-via-tavily(depth=advanced)` |
| Person / company / founder | Add browser in the runtime's default connection mode for LinkedIn + X live (see "Logged-in surfaces" below); drop polymarket-search |
| Event / election / launch date | Add polymarket-search; reduce others to `limit=5` if total context is the concern |
| Pure breaking news ("today's") | Lean on news-search-via-tavily(days=1, depth=advanced); reddit/hn at days=1 too |
| Niche technical topic | Drop producthunt-search + polymarket-search; double github-search and hackernews-search |
| Consumer cultural moment | Drop github-search; lean on reddit-search (multiple subreddit-targeted calls), news-search-via-tavily |

### Step 2 — Emit source-tool calls in parallel

Default recipe (5-6 tools, in parallel via multi-tool-use). **Default lookback is 3 days** across the catch-up flow — matches each per-source skill's own default and keeps signal hot. Widen explicitly only when the topic shape calls for it (Step 1 table covers the exceptions).

```
reddit-search(query=X, days=3, limit=10)
hackernews-search(query=X, days=3, limit=10)
github-search(query=X, days=3, limit=10, mode=mixed)
youtube-search(query=X, days=3, limit=10)
producthunt-search(query=X, days=3, limit=10)
news-search-via-tavily(query=X, days=3, depth=basic)
```

When to override the 3-day baseline (rule of thumb):
- Repo activity feels thin → bump `github-search` to 7 days
- Video / podcast / tutorial signal matters → keep `youtube-search`; bump to 7 days if the topic is slow-moving
- Product launch hunt feels thin → bump `producthunt-search` to 7 days
- News thread is multi-day → bump `news-search-via-tavily` to 7 days with `depth=advanced`
- Breaking news ("today's") → drop everything to `days=1`

### Step 3 — Browser augmentation (logged-in surfaces)

LinkedIn and X / Twitter both require the operator's signed-in session — anonymous access is blocked or heavily degraded. Both are accessed via the `browser` skill in the runtime's default connection mode. Omit `connection_mode` on the browser call; the runtime selects the profile-reusing default. There are no dedicated `linkedin_search` or `x_search` skills.

For person / company / current-event topics where these surfaces matter:

```
# LinkedIn — keep_browser_cdp_connection_alive=true if you're calling X next so
# the browser session stays warm between calls.
browser(url="https://www.linkedin.com/search/results/content/?keywords=<URL_ENCODED_TOPIC>&datePosted=%22past-week%22",
        keep_browser_cdp_connection_alive=true)

# X live search — last call of the chain so you can drop the keep-alive.
browser(url="https://x.com/search?q=<URL_ENCODED_TOPIC>&src=typed_query&f=live")
```

DOM extraction (paste into the browser's `evaluate` / `snapshot` step):
- **LinkedIn:** `div.feed-shared-update-v2, [data-id^="urn:li:activity:"]` → text via `.feed-shared-update-v2__description` / `.update-components-text`; engagement via `.social-details-social-counts__reactions-count` and `.social-details-social-counts__comments`; author via `.update-components-actor__name`.
- **X live:** `article[data-testid="tweet"]` → text via `div[data-testid="tweetText"]`; engagement via `div[data-testid="like"] span`, `div[data-testid="reply"] span`, `div[data-testid="retweet"] span`; author handle via `div[data-testid="User-Name"] a[href^="/"]`.

After extracting, wrap the items in the same envelope shape the source-search skills return, so whatsgoingon2 can merge them naturally:

```json
{
  "source": "linkedin_browser",
  "items": [
    {"title": "<first 140 chars of post>", "url": "<absolute LinkedIn post URL>",
     "snippet": "<full text>", "engagement": {"reactions": 142, "comments": 18},
     "author": "<author name>", "container": "linkedin"}
  ]
}
```

Same for X — use `"source": "x_browser"` so the agent (and downstream readers) can distinguish browser-extracted items from API-derived ones.

### Step 4 — Product Hunt fallback (when needed)

If `producthunt-search` returned `status=failed` (anon GraphQL 401-throttles under burst):

```
ph_items = []
for date_iso in utc_date_range(utc_today() - days, utc_today(), inclusive=True):
    ph_html = browser(url="https://www.producthunt.com/leaderboard/daily/" + date_iso)
    ph_items.extend(extract_producthunt_items(ph_html, query=X))

ph_items = rank_and_limit(dedupe_by_url(ph_items), limit=10)
```

Extract: `[data-test^="product-item-"]` → title via `a[href*="/posts/"]`; tagline via `p`; votes via `[data-test*="vote"]`.

### Step 5 — Merge the envelopes (preferred: `catchup_merge`)

The agent collected N envelopes (5 source-search outputs + maybe browser results). The new in-process Rust merge engine `catchup_merge` accepts envelopes inline as a JSON array — no `/tmp` file, no subprocess. Same algorithm as the legacy `whatsgoingon2` Python script (URL dedup → RRF → engagement+freshness scoring → optional Jaccard clustering); faster dispatch.

```
catchup_merge(
  query="AI video tools",
  envelopes=[
    {"source": "reddit", "items": [...]},
    {"source": "hackernews", "items": [...]},
    {"source": "github", "items": [...]},
    {"source": "youtube", "items": [...]},
    {"source": "producthunt", "items": [...]},
    {"source": "news_tavily", "items": [...]},
    {"source": "linkedin_browser", "items": [...]}
  ],
  cluster=true,
  limit=40
)
```

You get back a single envelope with: deduped + RRF-ranked + scored + clustered items, plus per-source contribution stats and dedup counts. Shape:

```json
{
  "query": "...",
  "total_items": 47,
  "items": [{title, url, snippet, source, _sources, _source_ranks, _score_breakdown, ...}, ...],
  "source_summary": [{source, input_count, contributed_to_final}, ...],
  "dedup_summary": {total_input_items, deduped_count, final_count},
  "clusters": [{representative_url, representative_title, member_count, member_urls}, ...],
  "duration_ms": 3
}
```

**Legacy path (subprocess, kept for back-compat):** the original Python skill is still granted as `whatsgoingon2`. Same algorithm but requires writing envelopes to a JSON file first and pays a Python subprocess + inner-LLM dispatch round-trip per call. Reach for it only if `catchup_merge` is unavailable.

### Step 6 — Synthesize from the merged output

Now write the user-facing response. Use the top-ranked items from `items[]` as the lead, group commentary by `clusters[]` (each cluster typically becomes one paragraph), cite items with inline `name`.

### Step 7 — Render

Default: **newspaper-style HTML brief** (serif typeface, single `<h1>` headline + dek, drop cap, classifieds-style section heads, inline anchors, `<style>` block inline). Unless the user / task asked for a specific format ("bullets", "memo", "Slack-friendly markdown") — then use that.

### Step 8 — Persist for cross-session reuse

After completing the synthesis, write to your memory tiers so the next call for the same / similar topic can short-circuit.

The `catchup_snapshots` tier is the primary cache + dedup ledger. Its `entries` field is a single array that you must read-modify-write as a whole (memory tier updates are flat field replacement, not append). Concrete recipe:

```
# 1. Retrieve the current entries array (returns null if first run for this topic).
search_memory(query="<normalized topic>", tier="catchup_snapshots")

# 2. Locally: find the existing entry with matching `key`, or append a new one. Shape per entry:
#    { key: "<normalized topic: lowercased, leading-articles stripped>",
#      captured_at: "<now ISO 8601>",
#      top_items: "<JSON-string of top 10 items: [{title, url, source, final_score}, ...]>",
#      seen_urls: "<JSON-string of ALL normalized URLs surfaced for this topic across sessions, capped at 500 FIFO>",
#      sources_called: "reddit,hackernews,github,...",
#      item_counts: {"reddit": 10, "hackernews": 10, ...} }
# Note: the field is named `key` (not `topic`) because the memory-tier indexer (see
# magician-vector-index/src/memory_candidates.rs::item_memory_key) recognizes `key`
# as the per-entry identifier — using `topic` would index entries by array position.

# 3. Write the FULL entries array back (this replaces the stored array; no append semantics).
update_memory_tier(tier="catchup_snapshots", fields={entries: [<full modified list>]})
```

To normalize URLs the same way whatsgoingon2 does (mandatory before populating `seen_urls` or comparing against it):

```
# Write the URLs you want to normalize to a JSON list file
echo '["https://Reddit.com/r/X/?utm_source=foo", "https://news.ycombinator.com/item?id=1/"]' > /tmp/urls.json

# Use whatsgoingon2's own normalizer (no merge, no envelope file needed)
whatsgoingon2(normalize_urls="/tmp/urls.json")
# → ["reddit.com/r/X", "news.ycombinator.com/item?id=1"]
```

Don't re-implement the normalization in shell — drift between your regex and the script's `normalize_url` (whatsgoingon2.py:118-137) breaks cross-session dedup silently.

Also expose the final merged envelope via your `task_state` capability so the delegating agent (or a downstream task in the same execution) can consume the full ranked pool, not just the synthesized prose.

Additional tiers to update in the same step:

- **episode memory** (14-day retention) — add the standard session record `{ key, timestamp, sources_used, item_count, summary }` for the per-session activity log.
- **semantic memory** (180-day retention) — for any new patterns noticed, add `{ key: "subreddit-for-<topic-class>", value: "...", last_seen: <now> }` or `{ key: "<source>-strong-for-<topic-class>", value: "...", last_seen: <now> }`. Lets future calls reuse "I know `r/MachineLearning` + `r/LocalLLaMA` are the right subs for AI/ML topics".
- **tool_usage memory** — record per-call effective-vs-not for each source so over time you learn that e.g. `producthunt-search` is high-hit on consumer-tool topics but low-hit on developer-framework topics.

### Before Step 2 — check catchup_snapshots first

Always run this BEFORE the per-source fan-out:

```
# Searches the agent's catchup_snapshots tier for entries matching the topic.
# Returns full entry JSON (not subject to the 4K prompt-budget that affects
# passive memory-tier rendering — see magician/src/magician_v2/agents/memory_prompt_blocks.rs:61-62).
search_memory(query="<normalized topic>", tier="catchup_snapshots")
```

If a matching entry comes back (token-overlap ≥ 0.6 against the requested topic), inspect its `captured_at` field:

| Age | Default behavior (override if user signals "fresh" / "latest" / "today") |
|---|---|
| < 6 hours | Surface the cached snapshot as-is. Skip the per-source fetch round entirely unless the user explicitly asked for freshness. |
| 6–24 hours | Run only a delta refresh: shrink each per-source tool's `days` param to `ceil(elapsed_hours / 24)` and merge with the cached snapshot before re-ranking. |
| 1–7 days | Treat cache as starting context; run a normal-window fetch but use the cached items to widen the dedup pool (URLs already in cache get a small RRF boost to surface continuing-coverage items). |
| > 7 days | Cache is stale on this kind of topic — ignore it and run a fresh full fetch. |

The cache IS the smart-window dedup. v1 had a separate SQLite store; v2 uses the dedicated `catchup_snapshots` memory tier.

### Cross-session dedup + novelty filtering

The `seen_urls` field on `catchup_snapshots` is the cross-session dedup ledger — it accumulates every normalized URL ever surfaced for the topic, across all prior sessions.

**Three dedup layers, in order, after the fetch round and before whatsgoingon2:**

1. **Within-call** (always, automatic): whatsgoingon2 normalizes URLs (lowercased host, scheme stripped, trailing slash dropped, tracking params removed) and collapses duplicates within the input envelopes. Same article from Reddit + HN ends up as one item with `_sources: ["reddit", "hackernews"]`.
2. **Cross-session rehydration** (delta-refresh case, 6-24h cache age): merge the cached `top_items` into the new envelope pool as a synthetic `{source: "cache", items: [...]}` entry before calling whatsgoingon2. The within-call dedup then collapses anything the new fetch re-surfaced.
3. **Cross-session novelty filter** (when the user asks "what's new since I last asked" / "anything new on X since yesterday" / similar): after whatsgoingon2 returns the merged envelope, filter items where the URL (normalized via `whatsgoingon2(normalize_urls=...)`) is already in `seen_urls`. The remainder is genuinely new since the last `captured_at`. If the result is empty, say so honestly — "no new items since [captured_at]" — instead of repeating yesterday's brief.

Concrete normalize-and-filter recipe:

```
# Extract URLs from the merged envelope
echo '["<url1>", "<url2>", ...]' > /tmp/fresh-urls.json

# Normalize via the same function the merge engine uses
whatsgoingon2(normalize_urls="/tmp/fresh-urls.json")
# → ["normalized_url_1", "normalized_url_2", ...]

# Locally: novel_urls = [u for u in normalized if u not in cached_seen_urls]
# Filter merged envelope items to only those whose normalized URL is in novel_urls
```

**Update `seen_urls` after every session.** Take all URLs from the final merged envelope (not just the top 10 you cached in `top_items`), normalize them via `whatsgoingon2(normalize_urls=...)`, union with the existing `seen_urls`, FIFO-evict at 500. This keeps the ledger bounded while preserving enough history to handle "what's new" asks within the 14-day retention window.

**vs-mode dedup:** each topic has its own `seen_urls` ledger — don't union them. An article that's new to topic A but already in topic B's ledger is still new to topic A.

### vs-mode — comparing two (or more) topics

When the user asks `X vs Y`, `compare X and Y`, `how does X stack up against Y`, `should I use X or Y` etc., run the standard catch-up flow **once per topic, independently**, then synthesize a comparison:

1. **Parse the topics.** Split into a list (e.g. `["langchain", "llamaindex"]`). Two-topic asks are the common case but the same pattern works for 3-4; beyond that, summarize-first is usually a better shape.
2. **Cache check per topic.** For each topic, run the Step 8 / pre-Step-2 cache logic above. A 12-hour-old snapshot for topic A + fresh-fetch for topic B is fine — the synthesis compares the two envelopes regardless of their fetch ages, just note recency per side in the output.
3. **Fan out the fetch round.** For each topic that needs fresh data, run Steps 1-2 (decide shape, fetch from per-source tools in parallel). Group calls by topic; do not interleave queries. Most-economical pattern: `parallel(topic_A_sources) + parallel(topic_B_sources)` as two parallel multi-tool batches.
4. **Merge per topic, separately.** Call `whatsgoingon2` twice — once per topic, with its own envelopes file. Do NOT pass mixed envelopes through a single call; the merge engine has no notion of "topic A vs topic B", it would dedup or cluster cross-topic items spuriously.
5. **Cache each topic's snapshot independently** to `catchup_snapshots` — both are useful as standalone catch-ups later, not just as one half of a comparison.
6. **Synthesize comparison** by reading both merged envelopes side-by-side. Pull out: shared themes (items where both topics get coverage), divergence (items unique to one side), momentum (per-side engagement totals + freshness skew), and the explicit comparison cues the user asked about (perf, features, community vibe, etc.). Render side-by-side: paired headlines, a comparison table for objective dimensions, separate "what people are saying" paragraphs per side.
7. **Result format.** Default to a two-column newspaper layout for two topics; a comparison table with per-row source-citations for objective specs. If the user asked for "which should I pick?", lead with the recommendation + one-line rationale, then back it up with the comparison.

The skill itself is unchanged in vs-mode — you're just running the same flow N times with independent state. No new tool, no new parameter.

### Optional layer — semantic rerank + dedupe via `vector`

When the `vector` capability is available (gated at runtime on Ollama health), you can layer semantic rerank and semantic dedupe on top of whatsgoingon2's deterministic merge. Adds 200-500ms per call but materially improves:

- **Rerank against the user query** — RRF only fuses per-source rankings; it doesn't know which items are most relevant to *what the user asked*. `vector.rank(output=ranked)` reorders the merged envelope by semantic similarity to the topic.
- **Catch paraphrases the Jaccard cluster misses** — Jaccard at 0.5 collapses items with overlapping title tokens. Two articles titled "OpenAI Sora 2 launches" and "Sora 2 hits public beta — what's new" share only "sora" so they stay separate; semantic cosine groups them.

Concrete recipe:

```
# After whatsgoingon2 returns the merged envelope, take its top-N items:
items = [{"id": item.url, "text": item.title + " — " + item.snippet,
          "metadata": {url, source, final_score}} for item in merged.items[:N]]

# Option A — semantic rerank against the user's actual question
vector(action="rank", output="ranked", query="<user's topic>",
       items=items, limit=20)

# Option B — semantic dedupe (catches paraphrased duplicates), with the
# query-aware representative selection so the surviving item in each
# cluster is the one most relevant to the topic.
vector(action="rank", output="deduped", query="<user's topic>",
       threshold=0.85, items=items)

# Combined (call rank twice — rerank first, then dedupe the ranked list)
ranked = vector(action="rank", output="ranked", query="...", items=items)
final  = vector(action="rank", output="deduped", threshold=0.9, items=ranked.items)
```

When `vector` is hidden from your tool catalog (Ollama down / not pulled), skip these calls and synthesize from the whatsgoingon2 envelope directly — RRF + Jaccard is the default fallback path. Don't fail the research because the embedder is offline.

For **cross-session "have I covered topics semantically near this one?"** retrieval, use `vector.search` against a long-lived namespace:

```
# At Step 8 (persist), also push the snapshot key + summary into the
# `catchup_index` namespace so future cache checks can find semantically-
# similar past topics.
vector(action="index", namespace="catchup_index",
       items=[{"id": <topic key>, "text": <topic + 1-line summary>, "metadata": {captured_at, ...}}])

# At the pre-Step-2 cache check, before the text-match against
# `catchup_snapshots`, do a semantic probe:
vector(action="search", namespace="catchup_index", query="<requested topic>",
       mode="hybrid", limit=3)
# If top hit score > 0.8 → likely the user is asking about a previously-
# covered topic. Inspect the matched snapshot key, then jump to the
# normal age-based cache logic on `catchup_snapshots`.
```

This catches asks like "what's happening with text-to-video models?" when last week we cached "ai-video-tools" — the text-overlap is low but semantic similarity is high.

## What NOT to do

- DON'T call this without first fetching envelopes via the source-search tools. It expects pre-fetched data.
- DON'T pass a relative path or `~` for `envelopes`. The skill subprocess's HOME differs from the agent's HOME — use absolute paths.
- DON'T re-call this multiple times for the same topic with slightly different envelope subsets. The agent should fetch once, merge once, synthesize once.
- DON'T return this tool's raw envelope to the user — always synthesize from it.
- DON'T pass huge envelopes (1000s of items per source). The dedup + RRF math scales fine but the synthesis context doesn't — stop at `limit=10-20` per source-search call so the merged total stays under 100.

## Direct CLI use

```bash
# Build a test envelopes file
cat > /tmp/test-envelopes.json <<'EOF'
[
  {"source": "reddit", "items": [{"title": "post A", "url": "https://reddit.com/a", "engagement": {"upvotes": 50}}]},
  {"source": "hackernews", "items": [{"title": "post A", "url": "https://reddit.com/a", "engagement": {"points": 12}}]}
]
EOF

whatsgoingon2 \
  --query "test merge" \
  --envelopes /tmp/test-envelopes.json \
  --pretty
```

You'll see one item out (the two inputs dedup to one), with `_sources: ["reddit", "hackernews"]` showing both contributed.
