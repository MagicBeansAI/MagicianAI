---
name: htmltotext
# The version is part of the governed contract the content cache is keyed
# on (the scope's capability revision digests SKILL.md and nothing else), so
# a change to what `bin/htmltotext` extracts MUST bump it or every page
# already read keeps its old extraction until the origin changes. 0.4.x:
# tables the article extractor drops are recovered as rows, and a table's
# unit caption (<small>, <caption>, <figcaption>) travels with it, placed
# directly above the table it qualifies; a table counts as kept only when a
# line renders its row (cells in order, nothing but whitespace or pipes
# between), never because the prose uses the same words. 0.5.0: the
# extractor reports a client-rendered shell (`/client_rendered`), which the
# reader fails as a JavaScript shell for the browser handoff.
version: 0.5.0
description: Local readable-text extractor used by the governed `content_read` static-reader stage. It
  can also process explicit raw HTML for diagnostics. Ordinary agents should call `content_read`, which
  owns transport, SSRF policy, cache, validation, and fallback.
metadata:
  magician:
    content_reader:
      schema_version: 1

      reader:
        id: static-http
        display_name: Static web page
        class: web_page
        cache_ttl_secs: 900
        max_response_bytes: 4194304
        max_text_chars: 4194304
        min_gist_chars: 80
        min_full_text_chars: 200
        fetch_timeout_secs: 20
        max_redirects: 5
        accepted_media_types:
          - text/html
          - application/xhtml+xml
          - text/plain
        retrieval:
          action_id: static_http.read
          operation: read
          rung: public_static
          priority: 100
          outputs: [gist, full_text]
          authority: public_remote_read
          parallel_safe: false

      capability:
        name: htmltotext
        action: run

      input:
        input_file_argument: input_file
        max_chars_argument: max_chars
        fixed_arguments:
          include_links: true

      output:
        text_pointer: /content
        method_pointer: /method
        truncated_pointer: /truncated
        error_pointer: /error
        # The extractor's client-rendered verdict: content shipped for
        # hydration that the server never rendered, or an empty app root.
        # True fails the read as a JavaScript shell so the ladder hands off
        # to a browser instead of serving a blurb as the page.
        shell_pointer: /client_rendered/shell
        shell_reason_pointer: /client_rendered/reason
    requires:
      bins:
      - htmltotext
      # `python3` is a PATH companion, not the entry point. The governed child
      # gets a cleared environment whose PATH is built only from the
      # directories that resolve this list, so without it `#!/usr/bin/env
      # python3` resolved to the OS interpreter — which carries neither
      # trafilatura nor beautifulsoup4. Both extraction paths then raised, the
      # adapter returned an empty string, and the reader reported
      # `chars_extracted: 0` with no error at all. Declaring it puts
      # `skillshub/.venv/bin` on the governed PATH, which is where the
      # extraction libraries actually live.
      - python3
    install_hint:
      docs: 'requires binary on PATH: python3, with trafilatura and beautifulsoup4
        installed in it (`make -C skillshub setup-python`)'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        url: "https://example.com"
        max_chars: 4000
      expect:
        # This canary exists to prove governed TLS trust reaches a Python
        # adapter — the exact failure that took twenty-one skills dark. So it
        # asserts that extraction produced something, not that the page says a
        # particular sentence: without a CA bundle this returns an SSL error and
        # chars_extracted 0, and asserting on copy would also turn the lane red
        # the next time example.com is reworded.
        #
        # It proves a second thing the TLS framing did not anticipate. The
        # fetch succeeded under the OS interpreter while extraction was dead,
        # so exit status, latency and the absence of an error envelope all
        # looked healthy; only the positive character count saw it. This is
        # why the count may never be relaxed to "the pointer exists".
        require_pointers: ["/chars_extracted"]
        error_pointer: "/error"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - htmltotext
        - python3
        # A CLI contract naming more than one binary MUST say which one the
        # runtime execs (`validate_requirements`, tool-runtime-core
        # `manifest_validation.rs`). Omitting it is not a soft default: the
        # contract fails validation, `project_runtime_package_to_pack` returns
        # an error, the loader logs it and drops the pack, and the tool then
        # reports `unknown inner-loop pack` at dispatch — strictly worse than
        # the un-declared state, because the skill no longer loads at all.
        # `bins` is a set, so nothing about declaration order implies this.
        entrypoint: htmltotext
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
          timeout_secs: 30
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
          description: Fetch and extract readable text from a URL or local HTML file, using the arguments
            selected from this capability guide.
          parameters:
            url:
              type: string
              description: URL to fetch and extract text from. Mutually exclusive with input_file.
              max_length: 4096
            input_file:
              type: workspace_path
              access: read_file
              description: Read-only path to a local HTML file to extract text from. Mutually exclusive with url.
              max_length: 4096
            max_chars:
              type: integer
              description: Maximum characters to return (truncates with indicator if exceeded)
              default: 12000
            include_links:
              type: boolean
              description: Preserve markdown-style links in extracted text
              default: true
          timeout_secs: 30
    runtime_catalog:
      categories:
      - web
      - text_extraction
      - html
      - research
      composition_category: web_operations
      expose_timeout_control: true
      timeout_default_secs: 15
---

# Htmltotext

Tool name: `htmltotext`
Primary parameter: `url` (fetch and extract) or `input_file` (extract from local HTML file)
Requires: curl, python3, trafilatura, beautifulsoup4 installed on host

Use for: the runtime-managed extraction stage or explicit raw-HTML diagnostics.
Ordinary web research should call `content_read` rather than invoking this URL
transport directly.

Returns JSON with: content (extracted text), chars_extracted (length),
truncated (boolean), method (trafilatura or beautifulsoup_fallback).

Within ordinary research, `content_read` is the bridge between discovery and
LLM reasoning; it invokes this extractor behind the governed transport. Raw HTML
wastes tokens on scripts, nav bars, and boilerplate.
This tool extracts just the article/main content, typically reducing 100KB+
of HTML to 2-10KB of clean text.

`metadata.magician.content_reader` also registers this capability as the local extraction
stage for the runtime's generic static HTTP reader. In that path Rust performs the
bounded public-network fetch, redirect validation, conditional request, and
scoped caching first; this capability receives only the resulting local HTML
file. The capability remains independently usable with `url` for explicit agent
tool calls, but content-source consumers do not bypass the shared fetch policy.

The `max_chars` parameter controls output size (default 12000). Set higher
for long articles, lower to save tokens when you only need a summary.

The `include_links` parameter preserves markdown-style links in output
(default true). Set false for pure text without URLs.

Extraction strategy:
1. trafilatura (primary) — purpose-built for article extraction, handles
   most news sites, blogs, docs, and wikis well
2. BeautifulSoup (fallback) — strips script/style/nav/footer tags, extracts
   remaining text. Used when trafilatura returns empty or very short content.
3. Dropped-table recovery (always, after trafilatura) — documentation sites
   wrap tables in scroll containers (Fern, Mintlify, Docusaurus) and the
   article extractor prunes the wrapper, keeping the prose and losing the
   table while reporting a confident page. Every content table outside site
   chrome (nav/header/footer/aside/form) whose rows are not already in the
   text is appended as pipe-separated rows, header first, bounded to 20 KB.
   A table's unit caption — a `<small>`, `<caption>` or `<figcaption>` the
   extractor pruned ("Prices per 1M tokens.") — is carried in as a
   `Table note:` line placed directly above the table it qualifies, whether
   the table itself was kept or recovered. A pricing comparison on
   2026-09-19 read a 620 KB pricing page as 1,330 characters of intro and
   reported the rates "not exposed"; the table was in the HTML all along.

The `version` in this file's frontmatter is part of the governed contract
the runtime's content cache is keyed on (the scope's capability revision
digests `SKILL.md` and nothing else). A change to what `bin/htmltotext`
extracts must bump it, or every page already read keeps its old extraction
until the origin's content changes.

NOTE: JavaScript-rendered pages (SPAs) will return minimal content since
curl fetches raw HTML without executing JS. For JS-heavy sites, use the
browser tool instead.

Search-engine result pages are not article content. If a Google or Bing
search-results URL is passed accidentally, this capability rewrites it to
DuckDuckGo's HTML results endpoint as a safer fallback, but you should
still prefer dedicated search capabilities to discover URLs first and only
use htmltotext on the actual destination pages.

Examples:
- {"url":"https://example.com/article"} → extract article text
- {"url":"https://en.wikipedia.org/wiki/Rust_(programming_language)","max_chars":"5000"}
- {"input_file":"/tmp/page.html"} → extract from local HTML file
- {"url":"https://example.com/post","include_links":"false"} → plain text only
