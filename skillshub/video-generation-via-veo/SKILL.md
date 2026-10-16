---
name: "video-generation-via-veo"
version: 0.2.2
description: "Generate or animate short videos using Google's Veo 3.1 models. Pick when the user wants a moving image, animation from a still, scene reveal, or short clip. Not for stills (use `image-generation`)."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: generativelanguage.googleapis.com
    requires:
      bins: ["veo31", "python3"]
      env: ["GEMINI_API_KEY", "VEO31_API_KEY"]
      python_packages: ["google-genai"]
    install_hint:
      docs: "requires env: GEMINI_API_KEY, VEO31_API_KEY — set in vault before activating"
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
        bins: [veo31, python3]
        entrypoint: veo31
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
        requirement: at_least_one
        secret_bindings:
          - {name: veo_api_key, secret_ref: VEO31_API_KEY}
          - {name: gemini_api_key, secret_ref: GEMINI_API_KEY}
        injections:
          - source: {kind: secret, binding: veo_api_key}
            target: {kind: environment, name: VEO31_API_KEY}
          - source: {kind: secret, binding: gemini_api_key}
            target: {kind: environment, name: GEMINI_API_KEY}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: Generate or animate a short video through Google Veo.
          parameters:
            prompt: {type: string, description: "Text prompt describing the video or requested motion.", required: true, min_length: 1, max_length: 4096}
            input_image: {type: string, description: "Optional local path or gs URI for image-to-video generation.", max_length: 4096}
            last_frame_image: {type: string, description: "Optional local path or gs URI for the desired last frame.", max_length: 4096}
            output_path: {type: string, description: "Optional local output path for the generated video.", max_length: 4096}
            duration_seconds: {type: integer, description: "Video duration in seconds.", default: 8, enum_values: [4, 6, 8], minimum: 4, maximum: 8}
            aspect_ratio: {type: string, description: "Video aspect ratio.", default: "16:9", enum_values: ["16:9", "9:16"]}
            resolution: {type: string, description: "Video resolution. 1080p requires an 8-second duration.", default: 720p, enum_values: [720p, 1080p]}
            quality_tier: {type: string, description: "Stable quality and latency routing tier.", default: auto, enum_values: [auto, fast, balanced]}
            number_of_videos: {type: integer, description: "Veo 3.1 returns one video per request.", default: 1, minimum: 1, maximum: 1}
            negative_prompt: {type: string, description: "Optional description of content to avoid.", default: "", max_length: 4096}
            model: {type: string, description: "Optional explicit provider model override.", default: "", max_length: 256}
    runtime_catalog:
      categories: [video, generation, creative, media, marketing]
      composition_category: media_operations
---

# Video Generation Via Veo

Video generation via Google Veo 3.1 models.

CAPABILITY:
- Routes across Veo 3.1 Fast and Veo 3.1 Standard based on quality/latency trade-off.
- Text-to-video and image-to-video supported.
- Last-frame guidance for hand-off shots, native audio synthesis, negative prompts.
- Duration / aspect-ratio / resolution controls.
- Honors `output_path` for caller-specified save locations.
- Declares `python3` as a runtime PATH companion, so governed dispatch uses the
  shared skill Python environment containing `google-genai`.
- Uses the API-key Gemini Developer API. Audio is always generated; create a
  silent deliverable with the existing `media_edit` `mute_video` operation.
  The SDK's enterprise-only `generate_audio` and `seed` controls are not exposed.
  Veo 3.1 prompt rewriting cannot be disabled, so `enhance_prompt` is not an
  exposed input; the provider default applies. See [Google's prompt rewriter
  documentation](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/video/turn-the-prompt-rewriter-off).
- Makes at most one submission request per invocation, with SDK automatic
  retries disabled. A failed paid dispatch requires reconciliation rather
  than an automatic replay. Failure diagnostics retain the request phase,
  known operation name and provider HTTP/status details with the API key redacted.

OUTPUT:
- MP4 artifact written under the session's outputs dir.

LIMITS & COST:
- High latency (minutes to tens of minutes).
- Standard tier is the most expensive media-gen pack.
- Veo regional availability may apply.
- One video per request; 1080p requires eight seconds.
- Not for stills (image-generation).

EXAMPLE PROMPTS:
- "Make a 6-second video of a cat jumping on a sofa"
- "Animate this still image with a slow zoom-out"
- "Generate an 8-second product reveal with audio"
