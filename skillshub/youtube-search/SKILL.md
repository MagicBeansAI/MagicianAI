---
name: youtube-search
version: 0.3.1
description: 'Search YouTube for recent videos matching a query. This is the

  `youtube-search` tool; it is backed by `yt-dlp` and needs no API key.

  Returns normalized items with view + like counts, duration, and the

  video description as snippet. Use for video / podcast / talk /

  interview coverage of a topic — surfaces stuff that wouldn''t show up

  on text-first sources like Reddit or HN.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    skill_type: tool
    # Plan 2.4: the publication block marks this pack as platform-publishable
    # app content. Name, description and version above remain the reviewed
    # identity; the block owns only the display label. Absent, the pack
    # behaves exactly as before.
    app_publication:
      display_name: YouTube Search
    user_invocable: true
    # Apps reach only this host through the call's broker (`app_egress`,
    # data only). A flat `yt-dlp` search was observed to contact only
    # www.youtube.com; thumbnails and media hosts are not fetched.
    app_egress:
      schema_version: 1
      destinations:
      - www.youtube.com
    requires:
      bins:
      - youtube-search
      # `python3` and `yt-dlp` are PATH companions, not entry points. The
      # governed child gets a cleared environment whose PATH is built only
      # from the directories that resolve this list. Without them the adapter
      # ran under the OS interpreter and searched a PATH that never contained
      # `skillshub/.venv/bin`, so BOTH routes to the backend were closed:
      # `shutil.which("yt-dlp")` found nothing and `find_spec("yt_dlp")` found
      # nothing either, because the module lives in the venv the interpreter
      # was not. That is the "backend unavailable" the lane reported — the
      # error contract working correctly over a backend the runtime had made
      # unreachable. Declaring them puts the venv's bin on the governed PATH,
      # which resolves the interpreter and the backend together.
      - python3
      - yt-dlp
    install_hint:
      docs: 'Requires Python 3.9+ and the skillshub Python environment

        (`make -C skillshub setup-python`), which installs the `yt-dlp`

        package used by this tool. When that dependency is missing,

        `youtube-search` returns status=failed with a clear message;

        the agent can fall back to browser against

        https://www.youtube.com/results?search_query=<query>.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: run
      input:
        query: "rust programming"
        limit: 3
        days: 365
      expect:
        # `error_pointer` is the line this skill was missing. It declares no
        # `content_source` block, so the runner had no error pointer to borrow,
        # and a failed run — a missing yt-dlp, a blocked search page — was
        # reported as "0 items, no error". The adapter now also refuses to call
        # an unparseable search a success, so the two halves meet.
        min_items: 1
        items_pointer: "/items"
        error_pointer: "/error"
        max_latency_ms: 30000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - youtube-search
        - python3
        - yt-dlp
        # Required, not decorative: a multi-binary CLI contract that omits the
        # entrypoint fails `validate_requirements` and the loader drops the
        # whole pack (`unknown inner-loop pack`). `bins` is a set, so the
        # fallback the validator refuses to apply would have picked `python3`
        # here — the lexicographically first name, not the adapter.
        entrypoint: youtube-search
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
          timeout_secs: 90
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
          description: 'Search YouTube for recent videos matching a query via the

            `youtube-search` skill. The skill is keyless and backed by the

            skillshub-installed `yt-dlp` package. Returns normalized items

            with view + like counts and (when available) a short snippet from

            the video description. Use for video / podcast / talk coverage of

            a topic that wouldn''t surface on text-first sources.

            '
          parameters:
            query:
              type: string
              description: Search query
              required: true
              max_length: 4096
            limit:
              type: integer
              description: Value for limit.
              default: 10
            days:
              type: integer
              description: Value for days.
              default: 30
          timeout_secs: 90
    runtime_catalog:
      categories:
      - research
      - search
      - video
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 90
---

# youtube-search — recent YouTube videos matching a query

## When to use

- "Any good videos / talks / podcasts about X?"
- Topics where the best explanation is a video rather than a blog
- Conference talk coverage (DevCon, ML conferences, etc.)
- Tutorial / hands-on / live-coding searches

## Parameters

| Param | Type | Default |
|---|---|---|
| `query` | string | (required) |
| `limit` | int | 10 |
| `days` | int | 30 — advisory, see below |

## Output

Envelope shape matches other source-search skills. Per-item:
- `engagement: {duration_s, views}` — `views` only when YouTube exposed it,
  and `likes` only on the rare entry that carries one. Flat search results have
  no like count, and reporting zero for every video would be a measurement
  nobody made.
- `container`: channel name (where the video lives)
- `author`: uploader / channel name

Envelope-level fields that make a zero-item answer legible:
- `provider_entries` — how many results the search returned before local
  filtering. Zero items with a non-zero count here is a filter outcome.
- `date_filter` — `"applied"` or `"unavailable"`. A flat YouTube search almost
  never carries a publication time, so `days` usually cannot be enforced. It is
  reported as unavailable rather than pretended; use it as a hint, not a filter.
- `status` / `reason` / `error` — `error` is `{kind, message}` when the search
  could not be run or parsed.

## Reaching the provider

The backend asks yt-dlp for the search as a single playlist object (`-J`)
rather than a stream of per-video JSON lines. That is a correctness
requirement, not a preference: yt-dlp exits 0 with empty stdout *and* empty
stderr both when a search legitimately matches nothing and when the search page
is blocked, so the line-stream form could not tell an outage from an empty
result and reported `status=ok, count=0` for both. Receiving a playlist object
proves the search ran; `entries: []` inside it means YouTube genuinely had
nothing. No object at all is now `status=failed` with an `error`.

## CLI

```bash
youtube-search "claude code agents" --limit 5 --pretty
```
