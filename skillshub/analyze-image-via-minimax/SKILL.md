---
name: analyze-image-via-minimax
version: 0.2.2
description: Vision-analyze an image via MiniMax's VLM through the vendored `mmx vision describe` CLI.
  Returns a structured description plus a short voice_summary suitable for spoken delivery.
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
      - minimax-vision
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
        CLI is vendored via `skillshub/analyze-image-via-minimax/package.json` (npm workspace), installed
        alongside the rest of skillshub's node deps. `mmx` runs under Node, which the
        runtime vendors at `skillshub/.node/bin` via `make -C skillshub setup-node`.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      # A PNG cannot be authored as a manifest `fixtures:` string, so the
      # package ships one instead, at `canary-fixtures/canary-image.png`;
      # `install-scope` links every package file into `<scope>/skills/<name>/`,
      # so the path below is the installed fixture read from the governed
      # working directory (the scope root).
      #
      # The image is deliberately tiny — 480x140, one line of text — because
      # this provider bills per analysed image and image tokens scale with
      # pixels. One small still is a liveness probe; anything larger is spend.
      input:
        image_ref: "skills/analyze-image-via-minimax/canary-fixtures/canary-image.png"
        question: "Transcribe every character of the text in this image, exactly as shown. Reply with the transcription only."
      expect:
        # The digits rendered in the fixture. They are the assertion because a
        # model that never received the image cannot produce them, and because
        # digits survive whatever casing the model chooses. A response that
        # merely arrived, or one describing a blank image, fails here.
        stdout_contains: "4821"
        require_pointers: ["/description", "/voice_summary"]
        max_latency_ms: 120000
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - minimax-vision
        - mmx
        - node
        entrypoint: minimax-vision
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
          timeout_secs: 120
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
          description: 'Vision-analyze an image via MiniMax''s VLM through the vendored

            `mmx vision describe` CLI. Returns a structured `description`

            plus a short `voice_summary` suitable for spoken delivery.

            Same input/output contract as `analyze_image_via_openai`;

            pick this for MiniMax-attributed vision (cost shaping, locale,

            regulatory preference) or as a fallback when OpenAI''s

            content-filter declines a benign image.

            '
          parameters:
            image_ref:
              type: string
              description: 'Filesystem path, URL, or pre-uploaded MiniMax file id.

                A relative path resolves against the governed working directory, so prefer an

                absolute path whenever the file is not known to sit beneath it — chat-session

                attachments in particular.

                '
              required: true
              max_length: 4096
            question:
              type: string
              description: Optional free-form question about the image.
              default: Describe this image in detail. Note any visible text, objects, layout, people,
                charts, code, UI elements, and other salient context. Lead with a one-sentence summary
                in the first line, then a structured description.
              max_length: 4096
          timeout_secs: 120
    runtime_catalog:
      categories:
      - vision
      - image
      - analysis
      - minimax
      composition_category: media_operations
      expose_timeout_control: true
      timeout_default_secs: 120
---

# Analyze image (MiniMax VLM)

Subprocess-spawns `mmx vision describe` to route image-analysis through
MiniMax's vision-language model. Sibling to the compiled
`analyze_image_via_openai` skill — different provider, same input/
output contract — so the agent can A/B compare or fall back without
changing call shape.

## When to use this skill

Reach for this when:

1. The user explicitly asks for MiniMax-attributed vision (cost
   shaping, locale, regulatory preference).
2. `analyze_image_via_openai` returned a content-filter decline that
   doesn't seem warranted — MiniMax's filter calibration may pass it.
3. A/B comparison — same image, two providers, see which description
   is more useful.

Do NOT use this skill for:

- Image generation — use `image-generation-via-minimax` or
  `image-generation-via-nanobanana2`.
- Image editing — neither MiniMax nor OpenAI vision edit images. Pair
  with an image-gen skill.
- Real-time captioning of video — single-still only.

## Inputs

- **`image_ref`** (required) — local filesystem path to the image, OR
  a MiniMax-pre-uploaded `file-` id. URLs are also accepted (the CLI
  fetches + base64-encodes automatically). For chat-session
  attachments, resolve to an absolute path before calling the skill.
- **`question`** (optional, default "Describe the image.") — free-form
  question about the image. Examples:
  - "Transcribe every visible character."
  - "Is this a screenshot of a code editor and what language is shown?"
  - "How many people are in the frame?"

## Output

JSON with:

- `description` — the model's text response.
- `voice_summary` — first paragraph trimmed to ~280 chars at a sentence
  boundary; safe for the realtime voice path to read aloud directly.
- `image_ref`, `label`, `media_type`, `question`, `model`,
  `provider: "minimax"` — metadata for grounding / downstream chaining.

## Latency

Single image: typically 5–15s including the API round-trip. CLI
overhead is negligible.

## Quota & cost

Counts against MiniMax's VLM quota. Check with `mmx quota show`.

## Auth

The package consumes a vault secret through a governed
`ConfigDirectory` named `MMX_CONFIG_DIR`. The runtime writes
`{ "api_key": "<secret>" }` into
`<MMX_CONFIG_DIR>/config.json`, and `mmx` reads credentials from that
directory.
