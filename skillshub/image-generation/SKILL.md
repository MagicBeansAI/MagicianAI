---
name: image-generation
version: 0.2.1
description: Generate or edit still images via Google Gemini "Nano Banana" models. Use when the user wants to create, modify, or stylize an image — photoreal scenes, art, logos with legible text, mockups, infographics, stickers, multi-image composites, or natural-language edits to an existing image. Not for video (use video-generation) or template-driven meme overlays (meme-generation).
compatibility: Requires NANOBANANA2_API_KEY in the secret vault and the runtime nano-banana facade installed.
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: generativelanguage.googleapis.com
    requires:
      bins: ["nanobanana2"]
      env: ["NANOBANANA2_API_KEY"]
      python_packages: ["Pillow", "google-genai"]
    install_hint:
      docs: "ships with the runtime — set NANOBANANA2_API_KEY in the secret vault before activating"
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
        bins: [nanobanana2]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: workspace}
        limits:
          timeout_secs: 1200
          stdin_bytes: 131072
          stdout_bytes: 8388608
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - {name: nanobanana_api_key, secret_ref: NANOBANANA2_API_KEY}
        injections:
          - source: {kind: secret, binding: nanobanana_api_key}
            target: {kind: environment, name: NANOBANANA2_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Generate or edit an image through the current Nano Banana model family.
          parameters:
            prompt: {type: string, description: "Text prompt describing the image to generate or edit.", required: true, min_length: 1, max_length: 4096}
            input_images: {type: string, description: "Comma-separated paths to at most fourteen reference images.", max_length: 4096}
            output_path: {type: string, description: "Optional output path within the governed workspace or temporary directory.", max_length: 4096}
            aspect_ratio: {type: string, description: "Optional supported image aspect ratio.", default: "", max_length: 16}
            resolution: {type: string, description: "Requested output resolution.", default: 1K, enum_values: ["512", 1K, 2K, 4K]}
            quality_tier: {type: string, description: "Stable quality and latency routing tier.", default: auto, enum_values: [auto, fast, balanced, pro]}
            thinking: {type: string, description: "Optional provider thinking level.", default: "", enum_values: [minimal, high]}
            use_search: {type: boolean, description: "Enable Google Search grounding on supported tiers.", default: false}
            model: {type: string, description: "Optional explicit provider model override.", default: "", max_length: 256}
    runtime_catalog:
      categories: [image, generation, editing, creative, design, marketing]
      composition_category: media_operations
---

# Image generation

Generate or edit still images via Google Gemini's current Nano Banana family:
Nano Banana 2 Lite, Nano Banana 2, and Nano Banana Pro. The caller selects a
quality tier explicitly when needed; `auto` is a stable alias for `balanced`.

## When to use this skill

Pick this skill for:
- Photoreal scenes, illustration, art
- Logos with legible text
- Product mockups, infographics, stickers
- Multi-image composites (up to 14 references, with model-specific limits)
- Natural-language edits to an existing image
- Multi-panel content (comics, storyboards) with consistent characters

Don't pick this skill for:
- Moving frames → use the `video-generation` skill instead
- Template-driven meme overlays → use the `meme-generation` skill
  (Imgflip-backed)

## Inputs

- **`prompt`** (required) — text describing what to generate or edit.
- **`input_images`** (optional) — comma-separated paths to reference
  images. The current family accepts up to 14 references, with fidelity
  limits varying by tier. Omit for text-to-image.
- **`output_path`** (optional, default temp file) — where to save. Use a
  `.jpg` or `.png` extension.
- **`aspect_ratio`** (optional) — `1:1`, `2:3`, `3:2`, `3:4`, `4:3`,
  `4:5`, `5:4`, `9:16`, `16:9`, `21:9`, `1:4`, `4:1`, `1:8`, `8:1`.
- **`resolution`** (optional, default `1K`) — `512` / `1K` / `2K` /
  `4K`. The `fast` tier supports only `1K`; `512` is available on
  `balanced`, while `2K` and `4K` are available on `balanced` and `pro`.
  Higher resolutions are slower; 4K with thinking can take minutes.
- **`quality_tier`** (optional, default `auto`) — `auto` / `fast` /
  `balanced` / `pro`. `fast` uses Nano Banana 2 Lite for low-cost 1K
  work, `balanced` uses Nano Banana 2 as the general default, and `pro`
  uses Nano Banana Pro for highest fidelity and complex composition.
- **`use_search`** (optional, default `false`) — enable Google Search
  grounding with `balanced` or `pro`. Nano Banana 2 Lite does not support
  Search grounding.

## Output

The skill writes PNG/JPEG artifacts under the chat session's outputs
directory (or your `output_path`) and surfaces the result with
`prompt_image: true` so a follow-up turn can edit the same image
through this skill.

## Latency expectations

- `fast` / `1K`: usually under a few seconds.
- `balanced` / `512` / `1K`: seconds.
- `balanced` / `2K`: tens of seconds.
- `pro` / `4K` with thinking: minutes.

The model emits intermediate progress events so the agent prompt receives
"still working" updates while the call is in flight.

## Quota

Inherits the user's Google API quota. Watch for `quota exceeded` errors —
back off and surface to the operator rather than retrying immediately.

## Fallback on decline

If the call returns `warning: Model returned no parts` (Gemini declined —
typically content safety, sometimes quota or empty response) and the
prompt does NOT name a copyrighted character / franchise, fall back to
`image-generation-via-minimax`. Same prompt, different lineage, different
moderation calibration. If the prompt DOES name protected IP (Spider-Man,
Mario, Mickey, etc.), rewrite it with an original character instead — the
fallback provider will refuse for the same reason. See the
`image-generation-via-minimax` SKILL.md for the full fallback chain.

## Example prompts

- "Generate a watercolor poster of a desert sunset"
- "Edit this image to remove the person in the background"
- "Make a 4-panel comic from this script with a consistent character"
- "Render a transparent PNG sticker of a smiling cat in pixel-art style"

## Authentication

Reads `NANOBANANA2_API_KEY` from the env (resolved from the runtime secret vault
when invoked). The skill itself never prompts for credentials — set the
key in the vault first; the runtime injects it.
