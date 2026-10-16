---
name: music-generation-via-minimax
version: 0.2.2
description: Generate songs and instrumentals via MiniMax `music-2.6` (default), `music-2.5+`, or `music-2.5`.
  Supports vocal songs (with lyrics or auto-generated lyrics), pure instrumentals, and fine-grained control
  over genre, mood, instruments, tempo, key, and song structure.
compatibility: Requires a vault-secret MiniMax API key and the `mmx` CLI vendored via skillshub's npm workspace.
  Network call to api.minimax.io.
metadata:
  magician:
    # Apps reach only this host through the call's broker, and the owner's
    # MiniMax key is scoped to it (`app_egress`, data only). The API host is
    # the one `mmx` was observed to contact; other hosts (file CDNs) are not
    # declared and stay unreachable from an app.
    app_egress:
      schema_version: 1
      destinations:
      - api.minimax.io
    requires:
      bins:
      - minimax-music
      - mmx
      # `mmx` is `#!/usr/bin/env node`, so the interpreter is part of what has
      # to be reachable. The governed child gets a cleared environment whose
      # PATH is built only from the directories that resolve this list, and
      # without `node` on it every call died before reaching the provider with
      # `env: node: No such file or directory` (exit 127). The runtime vendors
      # its own Node at `skillshub/.node/bin` (`make -C skillshub setup-node`),
      # which is one of the dependency roots the governed PATH resolves, so
      # naming it here binds the project-local interpreter rather than whatever
      # brew/nvm/fnm the operator happens to have. `whatsapp` declares it the
      # same way for the same reason.
      #
      # Like `mmx`, this is a PATH companion and not the entrypoint: it is
      # never bound as execution authority and never snapshotted, so a host
      # without it loses these calls with the CLI's own message rather than
      # losing the skill.
      - node
    install_hint:
      docs: ships with the runtime — set the MiniMax API secret in the vault before activating. The `mmx`
        CLI is vendored via skillshub's npm workspaces and runs under the Node the runtime
        vendors at `skillshub/.node/bin` (`make -C skillshub setup-node`).
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          media generation bills a substantial per-call amount and writes a
          large binary asset; the package offers no read-only action to
          substitute, so a live probe would spend real money every run.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - minimax-music
        - mmx
        - node
        entrypoint: minimax-music
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
          timeout_secs: 300
          stdin_bytes: 1048576
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        provider: minimax
        secret_bindings:
        - name: MINIMAX_API_KEY
          secret_ref: MINIMAX_API_KEY
        injections:
        - source:
            kind: secret
            binding: MINIMAX_API_KEY
          target:
            kind: config_directory
            name: MMX_CONFIG_DIR
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: 'Generate a song or instrumental via MiniMax. Exactly one of

            `lyrics`, `instrumental=true`, or `lyrics_optimizer=true` must be

            set. Pass structural and stylistic hints (genre, mood, vocals,

            etc.) to shape the result; pass `references` only when you have a

            specific artist/track to anchor on (do NOT name the user''s

            personal favorites without their consent).


            Output is a single audio file written to `output_path`.

            '
          parameters:
            prompt:
              type: string
              description: Style description. Max 2000 chars.
              required: true
              max_length: 4096
            output_path:
              type: string
              description: Absolute path where the audio file lands.
              required: true
              max_length: 4096
            lyrics:
              type: string
              description: Lyrics with [Verse]/[Chorus]/etc. tags. Max 3500 chars.
              default: ''
              max_length: 4096
            instrumental:
              type: boolean
              description: No vocals.
              default: false
            lyrics_optimizer:
              type: boolean
              description: Auto-generate lyrics from prompt.
              default: false
            vocals:
              type: string
              description: Vocal style hint.
              default: ''
              max_length: 4096
            genre:
              type: string
              description: Genre hint.
              default: ''
              max_length: 4096
            mood:
              type: string
              description: Mood hint.
              default: ''
              max_length: 4096
            instruments:
              type: string
              description: Instrument list.
              default: ''
              max_length: 4096
            tempo:
              type: string
              description: Tempo description.
              default: ''
              max_length: 4096
            bpm:
              type: integer
              description: Exact BPM.
            key:
              type: string
              description: Musical key.
              default: ''
              max_length: 4096
            structure:
              type: string
              description: Song structure.
              default: ''
              max_length: 4096
            references:
              type: string
              description: Reference tracks/artists.
              default: ''
              max_length: 4096
            avoid:
              type: string
              description: Elements to avoid.
              default: ''
              max_length: 4096
            use_case:
              type: string
              description: Use case context.
              default: ''
              max_length: 4096
            extra:
              type: string
              description: Additional fine-grained requirements.
              default: ''
              max_length: 4096
            model:
              type: string
              description: 'Model: music-2.6, music-2.5+, music-2.5.'
              default: music-2.6
              enum_values:
              - music-2.6
              - music-2.5+
              - music-2.5
              max_length: 4096
            format:
              type: string
              description: Audio format.
              default: mp3
              enum_values:
              - mp3
              - wav
              - pcm
              max_length: 4096
          timeout_secs: 300
    runtime_catalog:
      categories:
      - audio
      - music
      - generation
      - creative
      composition_category: media_operations
      expose_timeout_control: true
      timeout_default_secs: 300
---

# Music generation (MiniMax)

Generate songs or instrumentals via MiniMax's music family. Wraps the
`mmx music generate` CLI. Output is an audio file (default `.mp3`)
saved to the requested path.

## When to use this skill

Pick this skill when the user wants a piece of music — a short song,
a jingle, instrumental BGM for a video, a parody version of a tune, or
an experimental track. Examples:

- "Write a 30-second upbeat indie folk song about Monday mornings."
- "Make instrumental background music for my product launch video."
- "Generate a cinematic orchestral piece, building tension."
- "Make a song about [topic] with chorus and verse structure."

Don't pick this skill when:
- The user wants to manipulate / clip / mix existing audio →
  no skill for that yet; use `shell` + `ffmpeg`.
- The user wants spoken voice (narration, voiceover) →
  use `speech` skill (TTS) when available.
- The user wants to detect / transcribe an existing track →
  use `vision` / `ocr` for visual lyrics, or speech-to-text.

## Three modes

1. **With lyrics**: pass `lyrics` (with structure tags like `[Verse]`,
   `[Chorus]`, `[Bridge]` — see lyrics format below) for a vocal song.
2. **Instrumental**: pass `instrumental=true` and omit `lyrics`. No
   vocals.
3. **Auto-lyrics**: pass `lyrics_optimizer=true` and omit `lyrics`.
   MiniMax generates lyrics from the prompt automatically.

Exactly one of `lyrics`, `instrumental`, or `lyrics_optimizer` must
be set.

## Lyrics format

When passing `lyrics`, use structure tags on their own lines (max 3500
chars total):

```
[Intro]
[Verse]
Walking down the street tonight
City lights are burning bright
[Chorus]
This is the place we call home
[Bridge]
[Outro]
```

Supported tags: `[Intro]`, `[Verse]`, `[Pre Chorus]`, `[Chorus]`,
`[Interlude]`, `[Bridge]`, `[Outro]`, `[Post Chorus]`, `[Transition]`,
`[Break]`, `[Hook]`, `[Build Up]`, `[Inst]`, `[Solo]`. Tags must be
clean — anything inside brackets that isn't a recognized tag will be
sung verbatim.

## Inputs

- **`prompt`** (required) — style description. Max 2000 chars when
  combined with other structured flags.
- **`output_path`** (required) — where to save the audio (`.mp3`
  by default).
- **`lyrics`** (optional) — explicit lyrics with structure tags.
  Mutually exclusive with `instrumental` and `lyrics_optimizer`.
- **`instrumental`** (optional, default false) — generate without
  vocals.
- **`lyrics_optimizer`** (optional, default false) — auto-generate
  lyrics from the prompt.
- **`vocals`** — optional vocal style hint (e.g. "warm male
  baritone", "bright female soprano").
- **`genre`** — optional genre hint (folk, pop, jazz, …).
- **`mood`** — optional mood hint (warm, melancholic, uplifting).
- **`instruments`** — optional instrument list ("acoustic guitar,
  piano, strings").
- **`tempo`** — optional tempo description (fast, slow, moderate).
- **`bpm`** — optional exact beats per minute.
- **`key`** — optional musical key (C major, A minor, …).
- **`structure`** — optional song structure ("verse-chorus-verse-
  bridge-chorus").
- **`references`** — optional reference tracks/artists ("similar to
  Ed Sheeran").
- **`avoid`** — optional elements to avoid.
- **`use_case`** — optional context ("background music for video").
- **`extra`** — optional fine-grained requirements not covered above.
- **`model`** — optional model override. Default `music-2.6`. Other
  options: `music-2.5+`, `music-2.5`.
- **`format`** — optional audio format. Default `mp3`. Other options:
  `wav`, `pcm`.

## Output

JSON with the saved file path, size in bytes, model, and elapsed
time.

## Latency

- Typical: 20s-90s depending on length and model.

## Quota & cost

Inherits the user's MiniMax Token Plan. Music generation is more
expensive per call than image generation but cheaper than video.
Watch for `429` / quota errors and back off rather than retrying.

## Authentication

The package consumes a vault secret through a governed
`ConfigDirectory` named `MMX_CONFIG_DIR`. The runtime writes
`{ "api_key": "<secret>" }` into
`<MMX_CONFIG_DIR>/config.json`, and `mmx` reads credentials from that
directory.
