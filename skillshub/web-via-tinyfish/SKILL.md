---
name: web-via-tinyfish
version: 0.1.1
description: "Fast public web search and multi-URL extraction via TinyFish. Prefer it for quick public lookup/extraction on agents without `content_search`; ordinary research uses `content_search`, where TinyFish is the first free discovery adapter."
metadata:
  magician:
    # The HTTPS hosts this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted these destinations.
    app_egress:
      schema_version: 1
      destinations:
      - api.fetch.tinyfish.ai
      - api.search.tinyfish.ai
    content_source:
      schema_version: 1

      adapter:
        id: tinyfish
        display_name: TinyFish web search
        class: web_search
        execution: remote_endpoint
        auth: required
        sends_user_intent: true
        metered: false
        cursor: false
        privacy: public
        max_results: 10
        retrieval:
          action_id: tinyfish.discover
          operation: discover
          rung: public_search
          # Lower priorities run first. TinyFish is free and is the ordinary
          # first attempt; Exa (100) and Tavily (200) remain fallbacks.
          priority: 25
          outputs: [candidates]
          authority: public_remote_read
          parallel_safe: true

      capability:
        name: web-via-tinyfish
        action: search

      input:
        query_argument: query
        max_query_chars: 4096
        options:
          location:
            argument: location
            value_type: string
          language:
            argument: language
            value_type: string
          include_domains:
            argument: include_domains
            value_type: string_list
            join: ","
          exclude_domains:
            argument: exclude_domains
            value_type: string_list
            join: ","

      output:
        mode: mapped
        error_pointer: /error
        items_pointer: /results
        item:
          title_pointer: /title
          url_pointer: /url
          cheap_text_pointers: [/snippet]
          published_at_pointer: /date
          source_label_pointer: /site_name
          metadata:
            position: /position
            publisher: /publisher
            authors: /authors
            venue: /venue
            year: /year
            cited_by_count: /cited_by_count
            pdf_url: /pdf_url
        response_metadata:
          page: /page
          total_results: /total_results
    requires:
      # The official launcher uses `#!/usr/bin/env node`; declaring the
      # interpreter lets the governed runtime add its reviewed directory to
      # the child PATH without a wrapper.
      bins: [tinyfish, node]
      env: [TINYFISH_API_KEY]
    install_hint:
      docs: "Run `make setup-all` to install the pinned official CLI, then set TINYFISH_API_KEY in the scoped secret authority."
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: search
      input:
        query: "Rust programming language official"
      expect:
        min_items: 1
        items_pointer: /results
        error_pointer: /error
        # Match the controller's per-discovery-attempt ceiling. A canary that
        # passes slower than production can use the provider is a false green.
        max_latency_ms: 15000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        # Execute the reviewed interpreter and keep the official multi-file ESM
        # package in place. Snapshotting its index.js alone would detach the
        # sibling imports that comprise the CLI. Both binaries that land in
        # bin/ are declared here: `node` is the interpreter, `tinyfish` is the
        # launcher named by command_prefix below. Every file a governed skill
        # materializes into bin/ must be declared (see higgsfield, metabase).
        bins: [node, tinyfish]
        entrypoint: node
      runtime:
        protocol: cli
        command_prefix: [skills/web-via-tinyfish/bin/tinyfish]
        interaction: batch
        stdin: {mode: denied, sensitivity: public}
        working_directory:
          # The materializer installs the official launcher at this stable
          # scope-relative path; Node follows it into the pinned npm package.
          mode: workspace
        limits:
          # Fetch permits a provider-owned per-URL deadline up to 110 seconds;
          # leave five seconds for CLI startup and response serialization.
          timeout_secs: 115
          stdout_bytes: 16777216
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        provider: tinyfish
        secret_bindings:
          - name: tinyfish_api_key
            secret_ref: TINYFISH_API_KEY
        injections:
          - source: {kind: secret, binding: tinyfish_api_key}
            target: {kind: environment, name: TINYFISH_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        search:
          fixed_args: [search, query]
          description: Search the public web through TinyFish and return bounded result records. Ordinary research should call content_search, which selects this adapter first and preserves discovery receipts.
          parameters:
            query:
              type: string
              description: Search query.
              required: true
              min_length: 1
              max_length: 4096
            location:
              type: string
              description: Optional provider location hint.
              min_length: 1
              max_length: 256
            language:
              type: string
              description: Optional provider language hint.
              min_length: 1
              max_length: 128
            include_domains:
              type: string
              description: Optional comma-separated domain allowlist.
              max_length: 4096
            exclude_domains:
              type: string
              description: Optional comma-separated domain denylist.
              max_length: 4096
          mappings:
            - type: flag
              flag: --location
              parameter: location
            - type: flag
              flag: --language
              parameter: language
            - type: flag
              flag: --include-domains
              parameter: include_domains
            - type: flag
              flag: --exclude-domains
              parameter: exclude_domains
            # Everything after this trusted separator is inert positional
            # input even when the query begins with `--`.
            - type: literal
              arguments: [--]
            - type: positional
              parameter: query
        fetch:
          fixed_args: [fetch, content, get]
          description: Fetch and extract up to ten public HTTP(S) URLs through TinyFish. Prefer Markdown for agent use. This provider-direct action is for explicit TinyFish or bulk extraction; claim-bearing research should retain content_read's evidence and policy envelope.
          parameters:
            urls:
              type: string_array
              description: One to ten HTTP(S) URLs for the remote TinyFish Fetch service.
              required: true
              min_items: 1
              max_items: 10
              max_item_bytes: 4096
            format:
              type: string
              description: Extraction representation; Markdown is best for agents.
              default: markdown
              enum_values: [markdown, html, json]
            links:
              type: boolean
              description: Include page hyperlinks.
              default: false
            image_links:
              type: boolean
              description: Include page image URLs.
              default: false
            per_url_timeout_ms:
              type: integer
              description: Per-URL timeout; provider maximum is 110 seconds.
              default: 45000
              minimum: 1
              maximum: 110000
            if_none_match:
              type: string
              description: Optional ETag validator for a single URL.
              max_length: 4096
            if_modified_since:
              type: string
              description: Optional Last-Modified validator for a single URL.
              max_length: 4096
            include_etag_and_last_modified:
              type: boolean
              description: Include response validators for later conditional fetches.
              default: false
          mappings:
            - type: flag
              flag: --format
              parameter: format
            - type: bool_flag
              flag: --links
              parameter: links
            - type: bool_flag
              flag: --image-links
              parameter: image_links
            - type: flag
              flag: --per-url-timeout-ms
              parameter: per_url_timeout_ms
            - type: flag
              flag: --if-none-match
              parameter: if_none_match
            - type: flag
              flag: --if-modified-since
              parameter: if_modified_since
            - type: bool_flag
              flag: --include-etag-and-last-modified
              parameter: include_etag_and_last_modified
            # Prevent a URL-shaped model value from becoming a CLI option.
            - type: literal
              arguments: [--]
            - type: passthrough
              parameter: urls
    runtime_catalog:
      categories: [web, search, fetch, extraction, research]
      composition_category: web_operations
      expose_timeout_control: true
---

# Web Via TinyFish

Use `content_search` for ordinary discovery. The embedded source declaration
places free TinyFish search before Exa and Tavily within the public-search rung;
the controller still owns policy, fallback, receipts, and evidence selection.

Agents that do not own the research controller may use direct `search` for a
quick public lookup and `fetch` for bounded multi-URL extraction. Its default
Markdown output is suitable for agent input. Explicit provider requests and
adapter diagnosis also use these direct actions.

`fetch` is deliberately not registered as a `content_reader`: the current
reader extension gives Rust ownership of URL validation, retrieval, redirects,
cache, and provenance and passes only a local file to an extractor. A remote
provider fetch cannot truthfully claim that contract. For cited or otherwise
claim-bearing research, continue through `content_read`; its configured static
and browser rungs preserve the evidence boundary.

Authentication is the scoped `TINYFISH_API_KEY` secret. The governed runtime
injects it only for execution; model input cannot supply or override it.

This skill invokes the official `tinyfish` CLI directly. Skill materialization
installs the declaration, generated CLI symlink, and scoped config. The normal
`setup-deps` path installs the pinned official package under Skillshub's Node 24
runtime; there is no TinyFish-specific installer or protocol wrapper.

Provider limits currently used by this package:

- Search returns one page; `content_search` applies the caller's result limit
  while direct use returns TinyFish's page.
- Fetch accepts 1–10 public HTTP(S) URLs per call.
- Search and Fetch are free but still rate-limited by TinyFish.
