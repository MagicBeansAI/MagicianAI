---
name: media-fetch
version: 0.1.0
description: 'Turn a media URL into metadata, a transcript, native audio, or a

  merged video file. This is the `media-fetch` tool; it is backed by `yt-dlp`

  and needs no API key. Covers 1,747 extractors — YouTube plus nearly every

  other video/audio host yt-dlp supports, not a YouTube-only tool. Use after

  a search primitive (e.g. `youtube-search`) has found the URL; this pulls

  the full artifact from one known URL rather than discovering candidates.

  '
homepage: https://github.com/magicbeansai/magician
license: MIT
metadata:
  magician:
    skill_type: tool
    user_invocable: true
    requires:
      bins:
      - media-fetch
      - python3
      - yt-dlp
      - ffmpeg
      - ffprobe
      # ffmpeg/ffprobe are declared here (not just left to PATH) so the
      # governed runtime's sandboxed PATH is built from directories that
      # resolve them. They are reached through the pack-local symlinks
      # `make -C skillshub setup-media-fetch` installs at
      # `media-fetch/bin/{ffmpeg,ffprobe}`, not by putting the whole
      # Homebrew install on the governed PATH — that symlink resolves the
      # binary the sandbox needs without widening what the sandbox can see.
      #
      # Only `video` needs ffmpeg. Measured with ffmpeg hidden from PATH:
      # `metadata`, `transcript` and `audio` all completed successfully —
      # `audio` pulls a standalone native stream (`-f bestaudio`) that never
      # needs a merger. `video` requests separate video+audio streams
      # (`bv*+ba`) and needs ffmpeg to mux them into one file.
    install_hint:
      docs: 'Requires Python 3.9+ and the skillshub Python environment

        (`make -C skillshub setup-python`), which installs the `yt-dlp`

        package used by this tool, plus `make -C skillshub setup-media-fetch`,

        which symlinks the host `ffmpeg`/`ffprobe` into `media-fetch/bin/` for

        the `video` action. When ffmpeg is missing, `metadata`, `transcript`

        and `audio` still work; `video` returns status=failed with a clear

        remediation message rather than silently downgrading to `audio`.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: free
      action: metadata
      input:
        url: "https://www.youtube.com/watch?v=jNQXAC9IVRw"
      expect:
        # error_pointer is mandatory, not decorative: youtube-search shipped
        # without one and a failed run reported as "0 items, no error"
        # because the runner had no error pointer to check. Every action
        # here returns the same envelope shape (action/url/result/status/
        # reason/error/duration_ms), so both pointers are always present.
        items_pointer: "/result"
        error_pointer: "/error"
        # `require_pointers` is what makes this canary non-vacuous.
        # `canary.rs` accepts only `min_items`, `require_pointers` or
        # `stdout_contains` as a positive assertion, and rejects anything
        # else with `VacuousExpectation`: "a canary whose only assertion is
        # that the process exited zero proves nothing that a broken adapter
        # would not also satisfy." `items_pointer` and `error_pointer` alone
        # do NOT count. `min_items` does not fit here either — `/result` is
        # an object, not a list — so these three fields are the assertion.
        # They are the ones a `metadata` answer cannot be useful without.
        require_pointers: ["/result/title", "/result/extractor", "/result/duration_s"]
        max_latency_ms: 20000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - media-fetch
        - python3
        - yt-dlp
        - ffmpeg
        - ffprobe
        # Mandatory: `bins` is a set, so a multi-binary CLI contract without
        # an explicit entrypoint fails `validate_requirements` and the
        # loader drops the whole pack as "unknown inner-loop pack". Without
        # this, the fallback would pick `ffmpeg` — lexicographically first
        # among the five names — not the adapter that actually dispatches.
        entrypoint: media-fetch
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
          # video is the slow one: probe + download + ffmpeg merge of two
          # streams. This ceiling is the pack's real budget, not a bound on a
          # larger internal one — the adapter mirrors these per-action numbers
          # in `ACTION_BUDGET_S`, computes one deadline per invocation, and
          # draws every subprocess timeout down from it, holding back
          # `ENVELOPE_MARGIN_S` to serialise the result.
          #
          # It used to be the other way around: each action's internal
          # timeouts were larger than the governed ceiling (video 90s probe +
          # 1200s download against 900s here), so the runtime's own deadline
          # always fired first and force-killed the process group with
          # `GovernedExecutionTerminal::TimedOut` — a shape outside this
          # pack's error taxonomy, which meant a slow fetch returned a bare
          # kill instead of the structured envelope every action promises.
          # `ActionBudgetMatchesSkillMdTestCase` reads these numbers straight
          # out of this file, so the two cannot drift apart again in silence.
          timeout_secs: 900
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
        metadata:
          description: >-
            Probe a media URL and return normalized metadata: title, real
            upload date, duration, author, view/like counts, media container
            format (e.g. `webm`/`mp4`, from yt-dlp's `ext`), and available
            caption languages. The only action allowed to answer for an
            in-progress live stream — describing one is cheap and useful,
            while the other three actions refuse it because a live item has
            no fixed duration to download against.
          fixed_args: [metadata]
          timeout_secs: 90
          parameters:
            url:
              type: string
              description: The media URL to probe.
              required: true
              max_length: 8192
            max_duration_s:
              type: integer
              description: Refuse items longer than this. Default 7200 (2h).
            max_filesize_mb:
              type: integer
              description: Refuse items larger than this. Default 500.
        transcript:
          description: >-
            Fetch the caption track for a media URL and write it to disk as
            timestamped ~30s paragraphs. Never inline: one long video
            flattens to roughly 118k tokens of text, and nothing is
            truncated — the file is complete, with a bounded preview
            returned alongside it. `kind` in the result is `manual` or
            `auto` depending on which track was found. A source with no
            caption track fails with `NoCaptionsAvailable`; the caller's
            next move is the `audio` action plus local transcription, not a
            retry.
          fixed_args: [transcript]
          timeout_secs: 120
          parameters:
            url:
              type: string
              description: The media URL to fetch a transcript for.
              required: true
              max_length: 8192
            language:
              type: string
              description: Caption language code. Default "en".
              default: en
            output_dir:
              type: string
              description: >-
                Directory to write the transcript file into. Must resolve
                within the workspace, cwd, or a system temp root. Defaults
                to a fresh isolated subdirectory under the system temp dir.
              max_length: 4096
            max_duration_s:
              type: integer
              description: Refuse items longer than this. Default 7200 (2h).
            max_filesize_mb:
              type: integer
              description: Refuse items larger than this. Default 500.
        audio:
          description: >-
            Download the best native audio stream for a media URL, with no
            re-encoding — acquisition only. Downstream transcoding (format
            conversion) is the compiled `media_edit` provider's job, never
            this pack's. Returns the path and byte size of the downloaded
            file.
          fixed_args: [audio]
          timeout_secs: 300
          parameters:
            url:
              type: string
              description: The media URL to download audio from.
              required: true
              max_length: 8192
            output_dir:
              type: string
              description: >-
                Directory to write the audio file into. Same allowlist as
                `transcript`. Defaults to a fresh isolated subdirectory
                under the system temp dir.
              max_length: 4096
            max_duration_s:
              type: integer
              description: Refuse items longer than this. Default 7200 (2h).
            max_filesize_mb:
              type: integer
              description: Refuse items larger than this. Default 500.
        video:
          description: >-
            Download and merge separate video and audio streams into one
            .mp4 for a media URL. The only action that needs ffmpeg. Fails
            loudly with a remediation message when no merger is available
            rather than silently handing back audio-only or two unmerged
            files — the caller asked for a specific artifact. Returns the
            path and byte size of the single merged file.
          fixed_args: [video]
          timeout_secs: 900
          parameters:
            url:
              type: string
              description: The media URL to download video from.
              required: true
              max_length: 8192
            output_dir:
              type: string
              description: >-
                Directory to write the video file into. Same allowlist as
                `transcript`. Defaults to a fresh isolated subdirectory
                under the system temp dir.
              max_length: 4096
            max_duration_s:
              type: integer
              description: Refuse items longer than this. Default 7200 (2h).
            max_filesize_mb:
              type: integer
              description: Refuse items larger than this. Default 500.
    runtime_catalog:
      categories:
      - research
      - media
      - video
      - audio
      composition_category: research
      expose_timeout_control: true
      timeout_default_secs: 300
---

# media-fetch — pull metadata, a transcript, audio, or video from one URL

## When to use

- You already have a URL (from `youtube-search`, a browser fetch, or the
  user) and need the actual artifact, not another list of candidates.
- `youtube-search` **discovers** — it turns a query into a list of matching
  video URLs. `media-fetch` **fetches** — it turns one known URL into a
  usable artifact. They compose: search first, fetch the one you picked.
- Works across 1,747 yt-dlp extractors, not just YouTube.

## Actions

| Action | Returns | Needs ffmpeg |
|---|---|---|
| `metadata` | title, real upload date, duration, author, counts, container format, caption languages | no |
| `transcript` | a written `.txt` file of timestamped paragraphs + preview | no |
| `audio` | a native audio file (no re-encoding) | no |
| `video` | one merged `.mp4` (video + audio) | **yes** |

## Parameters

| Param | Type | Actions | Default |
|---|---|---|---|
| `url` | string | all | (required) |
| `language` | string | `transcript` | `en` |
| `output_dir` | string | `transcript`, `audio`, `video` | isolated temp subdir |
| `max_duration_s` | integer | all | 7200 |
| `max_filesize_mb` | integer | all | 500 |

## Why every action requires a positive artifact signal

yt-dlp exits 0 in every failure mode this pack cares about: an empty
extraction, no caption track, two unmerged video/audio streams left on disk,
and a `--max-filesize` abort. The exit code alone cannot distinguish any of
these from success, so nothing here trusts it. Every action instead checks
for a real artifact — a parsed info object for `probe()`, an actual caption
file for `transcript`, a non-empty file for `audio`, exactly one non-empty
merged file for `video` — and raises a typed error (`ProviderUnavailable`,
`NoCaptionsAvailable`, `CapExceeded`) when that signal is missing.

## Guards

- **Playlists refused** — a playlist/channel URL raises `CapExceeded` naming
  the entry count; fetch each item individually.
- **Live streams refused** — everywhere except `metadata`, since an
  in-progress broadcast has no fixed duration to enforce a cap against.
- **Duration/filesize caps** — enforced twice: once against the probed
  `duration`/`filesize_approx` before any bytes move, and again at download
  time via `--max-filesize`, because a generic-extractor URL often reports
  neither figure up front.

## Output

Envelope: `{action, url, result, status, reason, error, duration_ms}`.
`error` is `{kind, message}` — `kind` is one of `CapExceeded`,
`NoCaptionsAvailable`, `ProviderUnavailable`, `ValueError`, or an unexpected
exception's class name.

`metadata`'s `result.container` is the media container format (e.g. `webm`,
`mp4`), sourced from yt-dlp's `ext` field — not a second spelling of
`author`/`uploader`/`channel`.
