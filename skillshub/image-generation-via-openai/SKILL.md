---
name: image-generation-via-openai
version: 0.1.0
description: Generate a new image or edit an existing image through OpenAI GPT Image 2. Use for OpenAI image creation, reference-based edits and masked edits; returns a saved image artifact and metered provider receipt.
compatibility: Requires OPENAI_API_KEY through scoped credential authority, Python 3 and Pillow. Calls only api.openai.com.
metadata:
  magician:
    app_egress:
      schema_version: 1
      destination: api.openai.com
    requires:
      bins: [openai-image, python3]
      python_packages: [Pillow]
      env: [OPENAI_API_KEY]
    install_hint:
      docs: Ships with skillshub; bind OPENAI_API_KEY through the scoped vault or the installed skill's private config/.env before use.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: Image creation and editing are paid provider operations; no automatic paid readiness probe.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [openai-image, python3]
        entrypoint: openai-image
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: workspace}
        limits:
          timeout_secs: 330
          stdin_bytes: 262144
          stdout_bytes: 65536
          stderr_bytes: 4096
      auth:
        kind: secrets
        requirement: required
        provider: openai
        secret_bindings:
          - {name: openai_api_key, secret_ref: OPENAI_API_KEY}
        injections:
          - source: {kind: secret, binding: openai_api_key}
            target: {kind: environment, name: OPENAI_API_KEY}
      policy_floor:
        approval: ordinary
        resource_scopes: [workspace]
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        generate:
          description: Generate one new image and save it to a new workspace file.
          fixed_args: [generate]
          parameters:
            prompt: {type: string, description: Image creation instructions., required: true, min_length: 1, max_length: 32000}
            output_path: {type: workspace_path, access: create_file, description: New image file in the governed workspace; extension must match output_format., required: true, max_length: 4096}
            model: {type: string, description: Explicit GPT Image 2 model or pinned snapshot., default: gpt-image-2, enum_values: [gpt-image-2, gpt-image-2-2026-04-21]}
            quality: {type: string, description: Image quality., default: auto, enum_values: [auto, low, medium, high]}
            size: {type: string, description: auto or WIDTHxHEIGHT satisfying GPT Image 2 size limits., default: auto, max_length: 32}
            output_format: {type: string, description: Saved image format., default: png, enum_values: [png, jpeg, webp]}
            background: {type: string, description: Transparent output requires PNG or WebP., default: auto, enum_values: [auto, opaque, transparent]}
        edit:
          description: Edit one existing workspace image, optionally using a same-size PNG transparency mask; save a new file.
          fixed_args: [edit]
          parameters:
            prompt: {type: string, description: Instructions describing the image changes., required: true, min_length: 1, max_length: 32000}
            image_path: {type: workspace_path, access: read_file, description: "Existing PNG, JPEG or WebP source image in the governed workspace.", required: true, max_length: 4096}
            mask_path: {type: workspace_path, access: read_file, description: Optional same-size PNG mask; transparent pixels identify the editable region., max_length: 4096}
            output_path: {type: workspace_path, access: create_file, description: New image file in the governed workspace; the source is never overwritten., required: true, max_length: 4096}
            model: {type: string, description: Explicit GPT Image 2 model or pinned snapshot., default: gpt-image-2, enum_values: [gpt-image-2, gpt-image-2-2026-04-21]}
            quality: {type: string, description: Image quality., default: auto, enum_values: [auto, low, medium, high]}
            size: {type: string, description: auto or WIDTHxHEIGHT satisfying GPT Image 2 size limits., default: auto, max_length: 32}
            output_format: {type: string, description: Saved image format., default: png, enum_values: [png, jpeg, webp]}
            background: {type: string, description: Transparent output requires PNG or WebP., default: auto, enum_values: [auto, opaque, transparent]}
    runtime_catalog:
      categories: [image, generation, editing, creative, design, openai]
      composition_category: media_operations
      expose_timeout_control: true
      timeout_default_secs: 330
---

# OpenAI image generation and editing

Use `generate` for a new image and `edit` for changes to an existing image.
Each invocation produces one artifact. Choose a new `output_path` in the
governed workspace; the adapter refuses overwrites and paths outside it.
Stage external or attached source images through the existing file tools first.

```text
image-generation-via-openai.generate({"prompt":"An illustrated forest trail","output_path":"trail.png"})
image-generation-via-openai.edit({"prompt":"Change the sky to sunset; preserve the trail","image_path":"trail.png","output_path":"trail-sunset.png"})
```

Optional `mask_path` selects an editable region using transparent pixels in a
PNG mask with the same dimensions as the source. PNG/JPEG/WebP sources must
be under 50 MiB; masks must be under 4 MiB. GPT Image 2 processes image inputs
at high fidelity automatically, so this skill omits `input_fidelity`.
Transparent output is an OpenAI preview feature.

The existing runtime resolves `OPENAI_API_KEY` through scoped credential
authority and injects it only for an authorized call. A provisioned vault
secret takes precedence. Otherwise, the runtime reads only the declared key
from the installed skill's private `config/.env`. An existing OpenAI runtime
key may be configured there without provisioning another vault entry; include
only this skill's credential, not the complete shared runtime env file. The
file must satisfy the runtime's ownership and private-mode checks.
Credentials and provider
URLs are not model parameters. This skill uses one HTTPS request and never
automatically retries a paid operation.

Results contain `images[].path`, image metadata and SHA-256, `request_id`,
provider token `usage`, `cost_microusd` and `pricing_version`. The cost is an
estimate using the versioned standard tariff, not an account invoice. Missing
or inconsistent usage yields an unavailable cost, never a zero-cost substitute.
After a successful provider response, `artifact_error` retains the receipt and
accounting if decoding or saving fails; do not treat it as a usable image or
silently repeat the paid request.

Work discovers `images[].path` through its existing artifact collector. For a
Quality run, admit each required action using the existing metered-tool policy:
`receipt_pointer: /request_id`, reported `cost_pointer: /cost_microusd`, a
reviewed maximum cost/call count and the returned pricing version. Bind
`output_path` as a write path, and `image_path`/`mask_path` as read paths for
edits; pin model, quality, size, background and output format in the frozen
policy. The existing ledger and sealed artifact review own retries, capture
and acceptance. No separate image review loop is added.

API and tariff references:
[generation](https://developers.openai.com/api/reference/resources/images/methods/generate),
[editing](https://developers.openai.com/api/reference/resources/images/methods/edit),
[size/fidelity limits](https://developers.openai.com/api/docs/guides/image-generation),
[pricing](https://developers.openai.com/api/docs/pricing).
