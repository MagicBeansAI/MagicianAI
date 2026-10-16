---
name: web-scraping-playbook
version: 0.1.0
description: Procedure skill — decide HOW to pull data off a website before you reach for any tool. Hierarchy from cheapest (API → curl → htmltotext → browser headless → browser headed → cdp with profile → cloak-browser for bot-detect) and the signals that say which tier you need. Pull this whenever the task involves getting structured info off a public web page, an SPA, a flight aggregator, a news article, a docs site, a logged-in dashboard, or anything that smells like scraping. Covers pagination, rate limits, anti-detection, JSON-LD/microdata, hidden APIs, robots.txt, and output discipline. No tool calls — pure decision-making.
metadata:
  magician:
    skill_type: procedure
---

# Web Scraping Decision Playbook

You're about to fetch data off a website. Before you reach for a tool, decide *which tool*. Scraping is a hierarchy of costs — each tier slower, more brittle, more expensive than the one above it. Most agents skip straight to "open the browser" when a 200ms `curl + jq` would have done the job. Don't be that agent.

This playbook is decision-making only. No tool calls in here. Activate it, work through the decision tree, then run.

## The 60-second framing

Two activities, often confused:

- **Scraping** = programmatic *extraction of data*. Goal: clean records. Pages are a means to that end. Cheap tools first.
- **Browsing** = navigation + interaction (clicks, forms, login). Goal: perform an action or render JS the server won't serve raw. Expensive, brittle.

For a research task, the answer is almost always scraping — and almost always with a cheaper tool than your first instinct.

The single most useful question is: **"Where does this data actually live, and what's the smallest possible thing that returns it?"**

## The hierarchy (cheapest → most expensive)

Pick the highest tier that works. Don't fall further than necessary.

```
┌──────────────────────────────────────────────────────────────┐
│  1. OFFICIAL API                                              │
│     If the site has a documented API, use it. ALWAYS check.   │
│     Search for "<site> API", "<site> developer", or look in   │
│     the page's footer / dev tools for hidden endpoints.       │
├──────────────────────────────────────────────────────────────┤
│  2. SEARCH-ENGINE SNIPPETS                                    │
│     content_search and its configured providers already return   │
│     inline snippets. If your answer fits in 1-2 sentences,    │
│     you might not need to fetch the page at all.              │
├──────────────────────────────────────────────────────────────┤
│  3. RAW HTTP (curl / http tool / fetch)                       │
│     For JSON endpoints, RSS feeds, sitemaps, plain-text       │
│     responses, or static HTML you'll parse yourself with jq   │
│     / regex / a single sed.                                   │
├──────────────────────────────────────────────────────────────┤
│  4. htmltotext (trafilatura / beautifulsoup)                  │
│     For server-rendered HTML pages where you want the         │
│     readable article body. THE workhorse for news,            │
│     documentation, blog posts, Wikipedia, most public pages.  │
├──────────────────────────────────────────────────────────────┤
│  5. document-to-markdown / pdftotext / ocr                    │
│     Parse ordinary documents locally with AnyDoc. Keep        │
│     pdftotext for PDF controls and OCR scans/image-only PDFs. │
├──────────────────────────────────────────────────────────────┤
│  6. BROWSER — headless                                        │
│     For SPAs (React/Vue/Angular), JS-rendered content, sites  │
│     that need cookies/fingerprint to serve content. With      │
│     cloak-browser engine: stealth fingerprint patches active. │
├──────────────────────────────────────────────────────────────┤
│  7. BROWSER — headed (visible window)                         │
│     When CAPTCHAs may appear, OAuth flows, or any time a      │
│     human might need to intervene mid-flow.                   │
├──────────────────────────────────────────────────────────────┤
│  8. BROWSER — cdp (attach to user's actual Chrome)            │
│     For logged-in scraping: Gmail, Slack, internal company    │
│     dashboards, banking. The user's cookies/auth survive      │
│     because we're driving their real Chrome profile via       │
│     Magicutor's CDP proxy. NEVER use for adversarial scrapes  │
│     against sites that ban accounts — it's the user's account.│
└──────────────────────────────────────────────────────────────┘
```

## The decision tree — work top to bottom

```
1.  Does the site have an official API?
    ├── YES → use it. Stop here.
    │        Examples: GitHub (gh CLI or REST), Google APIs,
    │        Stripe, Spotify, Reddit (old API), Wikipedia
    │        (action=query), OpenStreetMap Nominatim, NWS,
    │        most public-data sites.
    └── NO → continue

2.  Can I answer from a search-engine snippet alone?
    ├── YES → content_search.
    │        Many "what's the capital of X" / "when was Y founded"
    │        / "current CEO of Z" answers don't need the page.
    └── NO → continue

3.  View source. What do you see?
    ├── data sits in a <script type="application/ld+json">
    │   or <meta property="og:..."> or <script id="__NEXT_DATA__">
    │   → use curl + jq. Structured data, free.
    │
    ├── data sits in <article> / <div class="content"> / <main>
    │   visible in raw HTML
    │   → htmltotext. Done.
    │
    ├── only <div id="root"></div> + <script src="bundle.js">
    │   → SPA. Skip to step 5.
    │
    └── return is JSON / RSS / sitemap
        → curl + jq / fetch + parse

4.  Network tab inspection (open DevTools, reload, watch XHR/fetch)
    BEFORE committing to a browser session, scan the network calls
    for hidden APIs the site uses to populate itself. Often a site
    that *renders* with JS still *fetches* clean JSON from a
    backend you can hit directly with curl. Examples found in the
    wild: Yelp restaurant data, Twitter (X) GraphQL, many e-com sites,
    most flight aggregators, almost every infinite-scroll feed.
    If a hidden API exists → curl it. Skip the browser entirely.

5.  Browser headless (with cloak-browser engine if installed).
    Used when:
    - SPAs that won't render without JS
    - Sites whose data only appears after window.fetch()
    - Bot-protected pages where the C++-patched Chromium passes

6.  Browser headed.
    Used when:
    - You need to watch the interaction happen
    - CAPTCHAs may appear and need human-solve
    - OAuth flows or interactive auth
    - Debugging "why doesn't this load"

7.  Browser cdp (attach to user's real Chrome).
    Used when:
    - Auth required (Gmail, Slack, banking, internal dashboards)
    - The user's session is the source of legitimacy
    NEVER use to scrape public sites — it's slower than headless
    and ties the user's identity to the activity.
```

## Read-the-network-tab specifically — the underused trick

A site renders with React. Your instinct: open the browser, snapshot the DOM, parse with selectors. **Stop.**

In Chrome DevTools:
1. Open Network tab, filter "Fetch/XHR"
2. Reload the page
3. Look for calls to `*api*`, `*graphql*`, `/data/*`, `/_next/data/*`, `/v1/*`, `/v2/*`
4. Click one — does it return clean JSON?
5. Copy as curl, strip auth if it works without (often does)

You just turned a 30-second JS-rendered scrape into a 200ms JSON fetch. This works on far more sites than people expect — including TikTok, X, Reddit (new), Yelp, Best Buy, most e-commerce, most flight aggregators, most news sites' "infinite scroll" feeds.

When you're driving a browser session and notice you're parsing rendered DOM, ask: "is there an API call powering this?". If yes, abandon the DOM, hit the API.

## Common targets — what actually works

| Target | Best tier | Notes |
|---|---|---|
| News article | htmltotext | Trafilatura is excellent; UA spoofing rarely needed |
| Wikipedia | API | `action=query`, `action=parse` — free, structured |
| GitHub repo / issues / PR | `gh` CLI or REST API | Don't scrape github.com directly |
| Flight status | aggregator htmltotext or API | FlightAware, FlightRadar24, Trip.com. Airline-direct sites are usually Cloudflare-fronted; aggregators are easier |
| Stock prices | API (Yahoo Finance scraper, Alpha Vantage) | Avoid scraping Yahoo Finance UI directly — they rotate detection |
| E-commerce prices | aggregator + htmltotext | Most direct sites (Amazon, BestBuy, Walmart) heavily detect. Use price-tracking aggregators or RSS feeds where possible. |
| Reddit | old.reddit.com (htmltotext) or `.json` suffix | `https://reddit.com/r/X/.json` returns clean JSON for any public listing |
| Twitter/X | login required → browser cdp | Or paid X API; scraping unauth is heavily blocked |
| Hacker News | API at hacker-news.firebaseio.com | Free, fast, structured |
| Search results | search engine APIs (Tavily, Exa, Brave) | NEVER scrape Google directly — it'll captcha you instantly |
| Documentation sites | htmltotext | Most are server-rendered |
| Maps / geocoding | OpenStreetMap Nominatim API | Free, polite rate limit (1 req/sec) |
| LinkedIn | login required, ToS-restricted | Use the official API if you have it; scraping LinkedIn unauth violates ToS |
| Government data (FBI, census, etc.) | data.gov / agency-specific APIs | Almost always available; check first |
| Academic papers | arxiv.org API, semantic scholar API | Both have proper APIs |

## JSON-LD and microdata — the structured-data gift

Many sites embed structured data for SEO. Inspect for:

```html
<script type="application/ld+json">{...}</script>
<meta property="og:title" content="...">
<meta property="article:published_time" content="...">
<div itemprop="price" content="49.99">
```

If you see JSON-LD, parse it. The data is already structured: schema.org types for Product, Article, Recipe, Event, Organization, etc. Way better than parsing the DOM.

```bash
# Quick JSON-LD extraction
curl -s "$URL" | python3 -c '
import sys, re, json
html = sys.stdin.read()
for m in re.findall(r"<script[^>]+application/ld\+json[^>]*>(.*?)</script>", html, re.DOTALL):
    try: print(json.dumps(json.loads(m), indent=2))
    except: pass
'
```

## Pagination patterns

- **URL-based** (`?page=2`, `?offset=100`): just iterate, easy
- **Cursor-based** (`?next=abc123`): chase the cursor until missing
- **Infinite scroll**: almost always backed by a hidden API — check the Network tab. If no API, browser + `scroll down` loop
- **"Load more" button**: usually click triggers a hidden API call — find it first
- **Hash-based**: SPA routing, hidden API behind it

Always cap the loop: max N pages, max M results, max T minutes. Otherwise a misconfigured site can run you forever.

## Rate limiting — be polite or be banned

Default protocol when scraping a site you don't own:
- 1 request / second to same domain (slower for small sites)
- Exponential back-off on 429 (1s → 2s → 4s → 8s → give up)
- Honor `Retry-After` header if present
- Use `If-Modified-Since` / `ETag` to avoid re-fetching unchanged pages
- Cache aggressively — if you scraped the page yesterday, use what you have

For high-volume jobs, prefer official APIs (rate limits there are usually documented and generous).

## robots.txt and Terms of Service

Always GET `https://<site>/robots.txt` for the first scrape of any new domain. Disallow rules apply to crawlers including you. If a path is disallowed for `User-agent: *`, don't scrape it.

ToS is fuzzier. Read the relevant section before high-volume / commercial scraping. For one-off lookups for the user's own benefit, ToS friction is usually low. For ongoing data extraction that resells or republishes — read carefully.

Useful heuristic: if the site has a public API, the API's ToS supersedes the general scraping rules. Always.

## Anti-patterns — what NOT to do

- **Open the browser as the first move.** 80% of scraping jobs don't need a browser. Browser is slow, brittle, leaves footprints. Get to tier 6 only when tiers 1–5 fail.
- **Scrape Google search directly.** Google captchas you in seconds. Use the
  `content_search` controller and let its configured provider ladder choose a
  public search transport.
- **Loop without rate limiting.** Don't hammer. Most server bans come from naïve loops.
- **Ignore the official API.** Always check first. Even half-baked APIs are usually faster + cleaner than parsing HTML.
- **Re-scrape what you already have.** Cache. Especially for slow-changing data (company info, articles, flight schedules a day out).
- **Brute-force CAPTCHA.** You won't win. Either get a real session (browser cdp), use a different source, or escalate to the user.
- **Trust scraped data without source attribution.** Every record needs `source_url + scraped_at`. Otherwise audits are impossible.
- **Mix scrapes and live actions in one session.** Scraping = read-only. If you start clicking buttons, you're browsing, not scraping. Different mindset, different tool tier.

## Output discipline

Scraped data should ALWAYS land as structured records, not raw text dumps. For every scrape:

```json
{
  "records": [
    {
      "name": "...",
      "price": 49.99,
      "available": true,
      "url": "https://...",
      "fetched_at": "2026-05-15T07:42:00Z"
    }
  ],
  "source": "https://...",
  "method": "htmltotext",     // tier used: api / curl / htmltotext / browser / etc.
  "pages_fetched": 3,
  "warnings": []              // 429s, parse errors, anything fishy
}
```

Why this matters:
- Auditable: anyone can verify a record by URL+timestamp
- Idempotent: re-running the scrape upserts cleanly (key on `url` or natural id)
- Cache-friendly: TTL based on `fetched_at`
- Dashboardable: structured rows go straight into DuckDB / sheets / treasurer

For freeform text scrapes (e.g. an article's body), still capture: title, author, published_at, url, body. Don't return raw HTML in your final answer — extract and clean.

## Failure modes catalog

| Symptom | Likely cause | Next step |
|---|---|---|
| 403 Forbidden | Bot blocked at edge | Try cloak-browser engine; or accept the site says no |
| 429 Too Many Requests | Rate limit | Back off, check `Retry-After` |
| 200 OK but no data in HTML | SPA, JS-rendered | Network tab → find the JSON API; if none, browser headless |
| 200 OK with login page returned | Cookie required | browser cdp with user's Chrome |
| CAPTCHA appears | Bot fingerprint detected | Try cloak-browser; if that fails, escalate to user via `task_state` |
| Page loads but data missing | Lazy-loaded after interaction | Browser + wait for selector / scroll trigger |
| Schema changed | Site redesign | Update extractor; flag to user in warnings |
| SSL error | Self-signed / expired cert | Don't blindly disable verification — verify the site is what you think |
| Redirect loop | Cookie expectations, bot suspicion | curl with `-L`, then `--max-redirs 5` and inspect |
| Garbled encoding | Wrong charset detection | Force `--data-binary` curl, decode explicitly |

## When to delegate / escalate

- High-volume long-running scrape (>100 pages, >5 min) → `delegate_to_agent`
  to a specialist or, when the user explicitly requests exhaustive research,
  use `deep-research-with-openai`.
- Specialized niches (academic papers, government datasets, financial filings) → check for an open dataset first; `data.gov`, Kaggle, FRED, SEC EDGAR all publish bulk data
- Anything that requires user identity → `browser cdp` with their Chrome
- Anything you can't justify ethically/legally → ask via `task_state`, don't proceed

## A 30-second self-check before any scrape

Ask yourself in this order:
1. Is there an API? (always check first)
2. Is the answer in a snippet already?
3. View source — what's actually in the HTML?
4. Network tab — is there a hidden API?
5. What's the cheapest tier that returns the data?
6. Have I scraped this URL already? (cache check)
7. robots.txt — am I allowed?
8. Rate limit set?
9. Output schema clear?
10. Failure modes mapped?

If you answered any of 1–4 with "yes, that works", you don't need a browser. Use the cheaper tool.

## What this skill is NOT for

- Real-time monitoring (use cron + persistent scripts, not agent loops)
- Adversarial scraping (sites that actively ban you — that's a different game and outside this playbook)
- Scraping behind paid auth walls (use the user's session via `browser cdp`, or buy the API access)
- Replacing the `web-researcher` agent for synthesis tasks — this playbook helps you get the bytes; synthesis is its own thing

Activate this skill once you've decided "I need data off a webpage." Then deactivate when you've picked your tier and you're executing.
