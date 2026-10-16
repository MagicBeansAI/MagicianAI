---
name: image-generation-via-minimax
version: 0.2.2
description: Generate still images via MiniMax `image-01` (Hailuo). Primarily used as a **fallback** when
  `image-generation` returns a safety/quota decline on a copyright-clean prompt — different lineage, different
  moderation calibration. Also useful when the agent wants a stylistic alternative to Gemini's Nano Banana
  family.
compatibility: Requires a vault-secret MiniMax API key. Network call to api.minimax.io.
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
      - minimax-image
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
        CLI is vendored via `skillshub/image-generation-via-minimax/package.json` (npm workspace), installed
        alongside the rest of skillshub's node deps. `mmx` runs under Node, which the
        runtime vendors at `skillshub/.node/bin` via `make -C skillshub setup-node`.
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
        - minimax-image
        - mmx
        - node
        entrypoint: minimax-image
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
          description: 'Generate still images via MiniMax `image-01` (Hailuo). Primary

            use is FALLBACK when `image-generation` returns

            `warning: Model returned no parts` (likely safety false-positive)

            OR a transient infra/quota error. Different model lineage from

            Gemini, different moderation calibration.


            Do NOT call this skill to bypass a copyright restriction — if the

            prompt names Spider-Man / Mario / Mickey / any copyrighted

            character or franchise, MiniMax will refuse for the same reason

            Gemini did. Rewrite the prompt with an original character first.


            Text-to-image only; no image editing (use nanobanana2 for that).

            '
          parameters:
            prompt:
              type: string
              description: Text prompt describing the image to generate. Max 1500 characters.
              required: true
              max_length: 4096
            output_path:
              type: string
              description: Output file path for the generated image (use .jpg or .png). Defaults to a
                temp file.
              max_length: 4096
            aspect_ratio:
              type: string
              description: Image aspect ratio. Default 1:1.
              default: '1:1'
              enum_values:
              - '1:1'
              - '16:9'
              - '4:3'
              - '3:2'
              - '2:3'
              - '3:4'
              - '9:16'
              - '21:9'
              max_length: 4096
            n:
              type: integer
              description: Number of images to generate (1-9). Default 1.
              default: 1
            prompt_optimizer:
              type: boolean
              description: Let MiniMax rewrite the prompt server-side for better adherence.
              default: true
            subject_ref_image:
              type: string
              description: Local path or URL to a single character-reference image. When set, image-01
                anchors the protagonist's look against it. Single ref only.
              default: ''
              max_length: 4096
            model:
              type: string
              description: Model override. Default image-01.
              default: image-01
              max_length: 4096
          timeout_secs: 300
    runtime_catalog:
      categories:
      - image
      - generation
      - creative
      - design
      - fallback
      composition_category: media_operations
      expose_timeout_control: true
      timeout_default_secs: 300
---

# Image generation (MiniMax Hailuo)

Text-to-image via MiniMax's `image-01` model. Different model family than
Gemini Nano Banana (the primary image gen) — use this when Gemini fails
for a non-content reason or when you specifically want MiniMax's style.

## When to use this skill

**Primary path is still `image-generation`.** Reach for
this skill in these cases:

1. **Nanobanana returned `warning: Model returned no parts`** AND the
   prompt does NOT name a copyrighted character/franchise. The decline
   is likely a false-positive safety trigger, and MiniMax's filter
   calibration may pass it. (If the prompt DOES name a copyrighted
   character — Spider-Man, Mario, Mickey, etc. — rewrite the prompt to
   use an original character instead of switching engines; MiniMax will
   probably refuse for the same reason.)
2. **Nanobanana hit a quota / 503 / transient infra error.** Same
   prompt, different provider, no quota collision.
3. **Stylistic preference** — the user wants MiniMax's specific look
   (slightly cinematic / poster-like) over Nano Banana's photoreal
   default.

Do NOT use this skill for:
- Image editing with reference images — `image-01` is text-to-image
  only. Use `image-generation` instead.
- Video → `video-generation-via-veo`.
- Memes with template overlays → `meme-generation-via-imgflip`.

## Inputs

- **`prompt`** (required) — text describing what to generate. Max 1500
  characters.
- **`output_path`** (optional, default temp file) — where to save the
  first generated image. Use `.jpg` or `.png` extension.
- **`aspect_ratio`** (optional, default `1:1`) — one of `1:1`, `16:9`,
  `4:3`, `3:2`, `2:3`, `3:4`, `9:16`, `21:9`.
- **`n`** (optional, default `1`) — number of images to generate, 1-9.
  When >1, additional images save next to `output_path` with `_1`,
  `_2`, ... suffixes.
- **`prompt_optimizer`** (optional, default `true`) — MiniMax-side
  prompt rewriting for better adherence. Disable when you've already
  hand-tuned the prompt and don't want MiniMax to second-guess it.
- **`subject_ref_image`** (optional) — local path or URL to a single
  character-reference image. When set, `image-01` anchors the
  protagonist's look against it (passed as
  `--subject-ref type=character,image=<path>`). Single reference only
  — unlike nanobanana's `input_images` which takes up to 4 refs.
  Useful when this skill is invoked as the comic-strip fallback and
  the agent wants to preserve the protagonist's face across panels.

## Output

JSON with the saved file path(s) and the upstream MiniMax response.
Each image is downloaded from the URL returned by the API and saved to
the configured `output_path`. If MiniMax returns multiple images, all
are saved with numeric suffixes.

## Latency

- Single 1K image: typically 10-30s.
- Higher `n` is roughly linear.

## Quota & cost

Inherits the user's MiniMax Token Plan. `image-01` is one of the cheaper
text-to-image models on the platform; cost is typically lower per image
than Nano Banana Pro at 4K. Watch for `429` / quota errors and back off
rather than retrying immediately.

## Authentication

The package consumes a vault secret through a governed
`ConfigDirectory` named `MMX_CONFIG_DIR`. The runtime writes
`{ "api_key": "<secret>" }` into
`<MMX_CONFIG_DIR>/config.json`, and `mmx` reads credentials from that
directory.

## Fallback chain example

A robust comic-generation loop typically looks like:

1. Try `image-generation` with `quality_tier=pro`.
2. If response includes `warning: Model returned no parts` and the
   prompt is copyright-clean, retry with
   `image-generation-via-minimax`.
3. If MiniMax also fails, surface the failure to the user with both
   provider reasons — do not loop indefinitely.
