# Public-site metadata and discovery

`src/lib/landing/publicSite.json` owns production URLs, public titles,
descriptions and the shared 1200×630 PNG. The root layout's `PublicSiteHead`
is the single metadata owner for `/`, `/manifesto`, `/privacy` and `/terms`:
canonical, description, robots, Open Graph, Twitter and accurate WebSite/WebPage
JSON-LD. Normal app/development builds use noindex; private app routes receive
no public canonical or social metadata.

`make build-marketing-site` sets `VITE_MARKETING_BUILD=true` to prerender the
public root as well as the legal pages. Its initial HTML includes content and
setup mailto links without probing a backend. The ordinary app remains an SPA;
leave this flag unset for `make run-ui-dev`. Packaging removes the marketing
SPA fallback and supplies a real noindex 404 instead of indexing unknown paths.

Explain's generated head uses the same configuration. Its `/overview`
provides architecture and catalog descriptions without WebGL or JavaScript;
the interactive header and noscript fallback link to it. Catalog maturity is
preserved, rather than presenting every capability as complete.

`scripts/public-site-assets.mjs` writes robots, canonical-only sitemaps and an
optional `llms.txt` directory for both sites. Public search crawlers are allowed;
this change does not introduce a separate training-permission policy. Preview
branches remain public but use noindex HTML and `X-Robots-Tag: noindex, follow`,
while retaining production canonicals and social image URLs.

Publish with `make publish-marketing-site` and `make publish-showcase`.
`make publish-marketing-preview` and `make publish-showcase-preview` target
the respective `preview.<project>.pages.dev` aliases without attaching custom
domains. Marketing accepts `ARGS=--skip-build` to publish the already reviewed
artifact; preview packaging copies it before adding noindex. Both publishers
run `scripts/verify_public_site.py` before upload, checking unique head tags,
canonical/schema, text content, image dimensions, sitemap and preview headers.

Regressions: `node --test scripts/public-site-assets.test.mjs` exercises
escaping, production assets, preview isolation and readable catalog output.
`PublicSiteHead.component.test.ts` covers navigation cleanup and private/dev
noindex. Qualify actual deployments with HTTP checks of public pages, crawler
files, image content types, preview headers and unknown-path 404 responses.

Readable, indexable HTML and ordinary SEO also support AI search; no special
AI ranking tag or schema is required. `llms.txt` is a navigation aid, with no
indexing guarantee. Search crawlers and training crawlers are distinct:
[Google AI search guidance](https://developers.google.com/search/docs/appearance/ai-features),
[OpenAI bots](https://developers.openai.com/api/docs/bots),
[Anthropic crawlers](https://support.claude.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler).
Successful requests with a crawler user agent do not verify access from its
published IP ranges or guarantee indexing or answer citations.
