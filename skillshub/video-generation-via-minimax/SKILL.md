---
name: video-generation-via-minimax
version: 0.2.2
description: Generate short video clips via MiniMax `Hailuo` family (T2V, I2V, SEF, S2V). Useful as a
  sibling/fallback to `video-generation-via-veo` — different lineage, different motion style, different
  content-policy calibration. Supports image-to-video animation, start-end-frame interpolation, and subject-reference
  character consistency.
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
      - minimax-video
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
        - minimax-video
        - mmx
        - node
        entrypoint: minimax-video
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
          timeout_secs: 600
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
          description: "Generate a short video clip via MiniMax Hailuo. Modes:\n  - T2V (text only): pass\
            \ `prompt`, omit frame/subject paths.\n  - I2V (image animation): pass `prompt` + `first_frame_path`.\n\
            \  - SEF (start-end interpolation): pass `prompt` +\n    `first_frame_path` + `last_frame_path`.\n\
            \  - S2V (character consistency): pass `prompt` + `subject_image_path`.\n\nFallback for `video-generation-via-veo`\
            \ on safety/quota declines\nOR when the user wants Hailuo's specific motion style. Blocks\n\
            until the upstream task completes (typically 30s-3min); writes\nthe MP4 to `output_path` and\
            \ returns its absolute path.\n\nDo NOT use to bypass copyright restrictions — naming a copyrighted\n\
            character (Spider-Man, Mario, Mickey, etc.) will be refused by\nMiniMax too. Rewrite the prompt\
            \ with an original character first.\n"
          parameters:
            prompt:
              type: string
              description: Text prompt describing the video scene, action, and camera.
              required: true
              max_length: 4096
            output_path:
              type: string
              description: Absolute path where the .mp4 lands. Must sit under /tmp/ or the scoped runtime
                working directory.
              required: true
              max_length: 4096
            first_frame_path:
              type: string
              description: Local path or URL to a still. When set → I2V mode.
              default: ''
              max_length: 4096
            last_frame_path:
              type: string
              description: Local path or URL. Requires first_frame_path. → SEF mode.
              default: ''
              max_length: 4096
            subject_image_path:
              type: string
              description: Local path or URL to a subject reference for character consistency. → S2V mode.
              default: ''
              max_length: 4096
            model:
              type: string
              description: Optional model override (default routes by mode).
              default: ''
              max_length: 4096
            poll_interval_secs:
              type: integer
              description: Status poll interval in seconds (default 5).
              default: 5
          timeout_secs: 600
    runtime_catalog:
      categories:
      - video
      - generation
      - creative
      - animation
      - fallback
      composition_category: media_operations
      expose_timeout_control: true
      timeout_default_secs: 600
---

# Video generation (MiniMax Hailuo)

Text-to-video and image-to-video via MiniMax's Hailuo family. Wraps the
`mmx video generate` CLI. Sibling to `video-generation-via-veo` — both
produce short clips from prompts; pick by style / availability / cost.

## When to use this skill

Pick MiniMax when:

1. **Veo declined or quota-exceeded** on a copyright-clean prompt.
   MiniMax has different moderation calibration; same prompt may pass.
2. **You want a first-frame → video animation** (I2V mode). Pass
   `first_frame_path` pointing to a still image and the model animates
   it. Veo's I2V support is more limited.
3. **You want start-end-frame interpolation** (SEF mode). Pass both
   `first_frame_path` and `last_frame_path` and the model fills the
   middle. Useful for "morph A into B" or controlled transitions.
4. **You need character consistency across multiple clips**. Pass
   `subject_image_path` — the model uses it as a subject reference
   (S2V mode).
5. **Cost or quota orientation** — MiniMax Hailuo is often cheaper than
   Veo per clip; useful for iterative drafts.

Don't pick MiniMax when:
- You need a still image → `image-generation` (Nano Banana) or the
  MiniMax image sibling.
- The user explicitly asked for Veo.
- You need ≥10s clips — Hailuo currently caps at 6 seconds.

## Inputs

- **`prompt`** (required) — text describing the video.
- **`output_path`** (required) — where to save the resulting `.mp4`.
  Must be under `/tmp/` or the scoped runtime working directory (path-traversal
  validated server-side).
- **`first_frame_path`** (optional) — local path or URL to a still
  image. When set, switches to I2V (image-to-video) mode.
- **`last_frame_path`** (optional, requires `first_frame_path`) —
  switches to SEF (start-end-frame) interpolation mode. Uses
  `Hailuo-02` model.
- **`subject_image_path`** (optional) — switches to S2V mode for
  character consistency. Uses `S2V-01` model.
- **`model`** (optional) — explicit model override. Defaults route by
  mode: T2V → `MiniMax-Hailuo-2.3`, I2V → `MiniMax-Hailuo-2.3`,
  fast-I2V → `MiniMax-Hailuo-2.3-Fast`, SEF → `MiniMax-Hailuo-02`,
  S2V → `S2V-01`.
- **`poll_interval_secs`** (optional, default 5) — how often the CLI
  polls task status while waiting.

## Output

JSON with:
- `path` — absolute path to the saved `.mp4` file.
- `size_bytes` — file size.
- `task_id` — upstream MiniMax task id (useful for support tickets).
- `model` — which model actually ran (after auto-routing).
- `elapsed_ms` — provider-adapter wall-clock time.

The skill blocks until generation completes (typically 30s-3min). For
fire-and-forget mode (return task_id immediately), use the raw `mmx
video generate --async` invocation via the `minimax` facade skill —
not exposed here.

## Latency

- T2V (text-only): 30s-2min typical.
- I2V (image): 30s-3min depending on motion complexity.
- SEF (interpolation): 1-3min.
- S2V (subject ref): 1-3min.

The governed action caps execution at 10 minutes. That reviewed ceiling is owned
by the runtime contract and cannot be raised through an ambient environment variable.

## Quota & cost

Inherits the user's MiniMax Token Plan. Watch for `429` / quota errors
and back off rather than retrying immediately.

## Authentication

The package consumes a vault secret through a governed
`ConfigDirectory` named `MMX_CONFIG_DIR`. The runtime writes
`{ "api_key": "<secret>" }` into
`<MMX_CONFIG_DIR>/config.json`, and `mmx` reads credentials from that
directory.
