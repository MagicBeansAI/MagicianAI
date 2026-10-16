---
name: "meme-generation-via-imgflip"
version: 0.2.2
description: "Build memes from popular Imgflip templates with custom captions. Pick when the user wants a meme based on an established format (Drake, Distracted Boyfriend, This Is Fine, Two Buttons, Change My Mind, etc.). Not for finding existing memes (`gif-search-via-klipy`) or original images (`image-generation`)."
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.imgflip.com
    requires:
      bins: ["imgflip-meme"]
      env: ["IMGFLIP_PASSWORD", "IMGFLIP_USERNAME"]
    install_hint:
      docs: "requires env: IMGFLIP_PASSWORD, IMGFLIP_USERNAME — set in vault before activating"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      cost_tier: cheap
      action: run
      input:
        action: "list_templates"
      expect:
        min_items: 1
        items_pointer: "/templates"
        max_latency_ms: 45000
        max_cost_microunits: 20000
        # Ceilings are commodity-scoped. This one bounds real money; the
        # runner compares it only against a package that reports `usd`, so
        # it can never be read against a provider-credit figure.
        max_cost_commodity: usd
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [imgflip-meme]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory:
          mode: denied
        limits:
          # Governed wall-clock kill, and the backstop for the adapter below.
          # INVARIANT: timeout_secs >= INNER_WORST_CASE_SECS + GOVERNED_KILL_MARGIN_SECS.
          # bin/imgflip-meme declares CATALOG_TIMEOUT_SECS 10 and
          # CAPTION_TIMEOUT_SECS 15; a caption resolved by template name runs
          # both in sequence, so its own deadline budget is 10 + 15 = 25, plus
          # a 5s margin for interpreter start, stdin read, and normalization
          # => 30. This was 20, below the 25 it was meant to back stop, which
          # made the built-in template fallback and the described caption
          # failures unreachable. YAML cannot reference Python, so the adapter
          # mirrors this number as GOVERNED_KILL_CEILING_SECS and
          # phase7_migration.rs pins the two together.
          timeout_secs: 30
          stdin_bytes: 65536
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: imgflip_username
            secret_ref: IMGFLIP_USERNAME
          - name: imgflip_password
            secret_ref: IMGFLIP_PASSWORD
        injections:
          - source: {kind: secret, binding: imgflip_username}
            target: {kind: environment, name: IMGFLIP_USERNAME}
          - source: {kind: secret, binding: imgflip_password}
            target: {kind: environment, name: IMGFLIP_PASSWORD}
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        run:
          description: List bounded Imgflip templates or caption one template.
          parameters:
            action:
              type: string
              description: List templates or generate a captioned meme.
              default: caption
              enum_values: [caption, list_templates]
            template_id:
              type: string
              description: Optional Imgflip template identifier.
              max_length: 128
            template_name:
              type: string
              description: Optional template name for bounded fuzzy matching.
              max_length: 4096
            text0:
              type: string
              description: Top caption; required when action is caption.
              max_length: 4096
            text1:
              type: string
              description: Optional bottom caption.
              default: ""
              max_length: 4096
    runtime_catalog:
      categories: [meme, image, expression, creative]
      composition_category: media_operations
---

# Meme Generation Via Imgflip

Template-driven meme generation via Imgflip API.

The active runtime resolves `IMGFLIP_USERNAME` and `IMGFLIP_PASSWORD` as two
independent references from the canonical scoped secret authority only after execution
authorization. Existing private scoped `.env` installations remain a migration bridge
when vault records are missing; canonical records always win. Both values are injected
only into the self-contained `imgflip-meme` adapter. Caption requests place them in the
HTTPS form body required by Imgflip—never in the request URL or child-process argv—and
redirects are denied.

CAPABILITY:
- 100+ templates with fuzzy template-name matching.
- Custom top/bottom text fields.
- Template listing for discovery when the LLM is unsure which template fits.
- Renders to a public shareable URL.

OUTPUT:
- Caption: `{ action, summary, template_id, text0, text1, url, page_url }`.
- Listing: `{ action, count, templates: [{ id, name, url, box_count }] }`.
- URL is public/shareable directly.

LIMITS & COST:
- Low latency.
- Constrained to Imgflip's template catalog.
- Captions only — no custom layouts or non-template image overlays.

EXAMPLE PROMPTS:
- "Make a Drake meme: bad = 'reading docs', good = 'asking the LLM'"
- "Generate a 'Distracted Boyfriend' meme about my old framework vs the new one"
- "List the available meme templates"
