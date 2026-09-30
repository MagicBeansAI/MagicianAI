---
name: kapso-whatsapp-send
version: 0.2.2
description: Send a WhatsApp message from Presto's OWN WhatsApp (its Kapso/Meta-Cloud number). Capped
  at 50 WHATSAPP_SENDS/day. Reading is the separate (free) kapso-whatsapp-read tool. This is the agent's
  WhatsApp identity, NOT the user's personal WhatsApp.
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.kapso.ai
    requires:
      bins:
      - kapso-governed
      - kapso
    install_hint:
      docs: 'The exact Kapso CLI version is pinned once in the skillshub root npm workspace; run
        `make -C skillshub setup-kapso-cli`. Configure KAPSO_API_KEY + KAPSO_PHONE_NUMBER_ID
        for each intended scope, then run `make -C skillshub setup-env SCOPE=<principal>/<workspace>`.'
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action delivers a message to a real recipient; a canary must
          never send. Tier 1 covers its manifest/adapter join.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - kapso-governed
        - kapso
        entrypoint: kapso-governed
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: denied
          sensitivity: public
        working_directory:
          mode: workspace
        limits:
          timeout_secs: 30
          stdout_bytes: 10485760
          stderr_bytes: 2097152
      auth:
        kind: secrets
        requirement: required
        provider: kapso
        secret_bindings:
        - name: KAPSO_API_KEY
          secret_ref: KAPSO_API_KEY
        - name: KAPSO_PHONE_NUMBER_ID
          secret_ref: KAPSO_PHONE_NUMBER_ID
        injections:
        - source:
            kind: secret
            binding: KAPSO_API_KEY
          target:
            kind: environment
            name: KAPSO_API_KEY
        - source:
            kind: secret
            binding: KAPSO_PHONE_NUMBER_ID
          target:
            kind: environment
            name: KAPSO_PHONE_NUMBER_ID
      policy_floor:
        approval: conditional_external_side_effect
        resource_scopes:
        - workspace
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        send:
          description: 'Run `kapso whatsapp messages send` to send a WhatsApp message AS PRESTO,

            from Presto''s own WhatsApp number (the identity adapter injects --phone-number-id).

            Required: to (recipient phone, E.164 e.g. +15551234567), text (body).

            Confirm recipient + body before sending — each send debits 1

            WHATSAPP_SENDS. For media/templates pass a JSON payload via `input`.

            '
          fixed_args:
          - whatsapp
          - messages
          - send
          parameters:
            to:
              type: string
              description: Recipient phone number in E.164 form (e.g. +15551234567). Maps to `--to`.
              required: true
              max_length: 4096
            text:
              type: string
              description: Message text body. Maps to `--text`.
              required: true
              max_length: 4096
            input:
              type: string
              description: Path to a JSON payload file (for media/templates instead of a plain text body).
                Maps to `--input`.
              max_length: 4096
            extra_args:
              type: string_array
              description: 'Escape hatch — extra argv tokens for unsurfaced flags, e.g.

                ["--stdin"] to read the JSON payload from stdin. Sender-number flags

                are rejected because this skill has one fixed agent identity.

                '
              max_items: 8
              max_item_bytes: 2048
          mappings:
          - type: flag
            flag: --to
            parameter: to
          - type: flag
            flag: --text
            parameter: text
          - type: flag
            flag: --input
            parameter: input
          - type: passthrough
            parameter: extra_args
    runtime_catalog:
      categories:
      - messaging
      - whatsapp
      - kapso
      composition_category: messaging_operations
      expose_timeout_control: true
      timeout_default_secs: 30
      spend:
        type: counted
        commodity: WHATSAPP_SENDS
        cost_per_action: '1'
---

# Kapso WhatsApp — Send

Send WhatsApp messages **as Presto, from Presto's own WhatsApp number**
(`KAPSO_PHONE_NUMBER`, the "Presto" number on Kapso's Meta Cloud API). This is
**Presto's agent identity** — like its AgentMail inbox. The From number is fixed
to Presto's number: the identity adapter injects `--phone-number-id`
(`KAPSO_PHONE_NUMBER_ID`) on every send. Reading is the separate, free
`kapso-whatsapp-read` tool.

**This is NOT the user's personal WhatsApp.** The user's own WhatsApp (via
WhatsApp Web / QR pairing) is the separate `whatsapp` tool. Never send from the
user's number here — this tool only ever sends from Presto's own number.

**Cap: 50 sends/day.** Each `send` debits 1 `WHATSAPP_SENDS` against Presto's
daily budget. Confirm the recipient and body before sending — a send is metered,
delivered work, not a free read. WhatsApp also requires the recipient to be
within a valid messaging window (or you must use an approved template) — a bare
text send to a cold contact may be rejected by Meta.

Inner action:
- `send` — send a message. Required: `to` (recipient phone, E.164 e.g.
  `+15551234567`), `text` (message body). Optional: `input` (path to a JSON
  payload file for media/templates), `extra_args` (escape hatch, e.g.
  `--stdin`).

Output is JSON.

For an operation not surfaced here, the reference is
<https://docs.kapso.ai/docs/whatsapp/cli> — consult ONLY if a needed send
option isn't above; do not fetch it routinely.
