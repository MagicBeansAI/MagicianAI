---
name: "telegram"
version: 0.2.2
description: "Telegram Bot API — send messages, get chat info, and manage bot operations through a governed scoped-credential adapter"
metadata:
  magician:
    # The one HTTPS host this skill reaches when an app runs it in the
    # brokered-egress jail. The app must also be granted this destination.
    app_egress:
      schema_version: 1
      destination: api.telegram.org
    requires:
      bins: ["telegram-bot-adapter"]
      env: ["TELEGRAM_TOKEN"]
    install_hint:
      docs: "requires TELEGRAM_TOKEN in the scoped secret vault"
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action delivers a message to a real recipient; a canary must
          never send. Tier 1 covers its manifest/adapter join.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [telegram-bot-adapter]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin: {mode: required, sensitivity: private}
        working_directory: {mode: denied}
        limits:
          timeout_secs: 15
          stdin_bytes: 1048576
          stdout_bytes: 10485760
          stderr_bytes: 1048576
      auth:
        kind: secrets
        requirement: required
        secret_bindings:
          - name: telegram_token
            secret_ref: TELEGRAM_TOKEN
        injections:
          - source: {kind: secret, binding: telegram_token}
            target: {kind: environment, name: TELEGRAM_TOKEN}
      policy_floor:
        approval: conditional_external_side_effect
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      input_delivery: canonical_json_stdin
      actions:
        send:
          description: 'Send one text message to one named chat, as the bot. THE GATED SEND
            PATH — the recipient is the typed `chat_id` parameter and the body is `text`, so
            the outward gate can name who this act reaches, screen that chat against the
            suppression register and write a disclosure record before anything leaves. The
            Bot API method is fixed to sendMessage inside the adapter and cannot be chosen
            here. The result is `{"ok":true,"method":"sendMessage","chat_id":...,"message_id":...,"result":{...}}`
            — the top-level `message_id` is the Telegram message id, lifted out of the Bot
            API `result` object so it can be reconciled against; the provider result is kept
            beside it. Use this for every send. A transmitting Bot API method supplied through
            `run` is classified as outward and refused because its opaque data cannot bind a
            recipient.'
          fixed_args:
          - send
          parameters:
            chat_id:
              type: string
              description: Recipient chat — WHO this act reaches. Either the numeric chat id
                (negative for groups and supergroups) written as a string, or an @username
                for a public channel or supergroup. Any other form is refused. Discover ids
                from getChat or getUpdates through `run`; never invent one.
              required: true
              min_length: 1
              max_length: 128
            text:
              type: string
              description: The message body, as plain text. No parse mode is applied, so
                Markdown and HTML markup arrive literally. sendMessage rejects a body over
                4096 bytes.
              required: true
              min_length: 1
              max_length: 4096
          timeout_secs: 15
        run:
          description: 'Call one Telegram Bot API method with a bounded JSON object. THE
            DIAGNOSTIC PATH — the recipient lives inside the opaque `data` string. Reading
            (getMe, getChat, getUpdates, getChatMembersCount, getFile) is what this is for.
            Methods beginning with send, forward, or copy are classified as outward and
            refused before the adapter runs because the opaque form cannot bind a recipient.
            Use the typed `send` action to send text.'
          parameters:
            method:
              type: string
              description: Telegram Bot API method name, for example sendMessage or getMe.
              required: true
              min_length: 1
              max_length: 128
            data:
              type: string
              description: JSON object containing the method parameters.
              default: "{}"
              max_length: 1000000
          timeout_secs: 15
    runtime_catalog:
      categories: [messaging, telegram, communication]
      composition_category: messaging_operations
      expose_timeout_control: false
---

# Telegram

Tool name: `telegram`
Requires: `TELEGRAM_TOKEN` in the exact scoped secret authority
Use for: sending messages, photos, documents, locations via Telegram Bot API;
querying chat info, member counts, and recent updates.

Messages are sent as the bot identity (DeepAct @deepact_bot), not as the user.

The governed runtime authorizes the call before resolving the token. It sends one
canonical JSON request to the bundled provider adapter; the token remains in the
adapter process and is never copied into child-process arguments or model input.

## Two action shapes, and only one can send

**`send` is the typed, bindable path.** `run` remains available for reads, but
transmitting Bot API methods fail closed before the adapter runs.

| | `send` | `run` |
|---|---|---|
| Recipient | typed `chat_id` parameter | buried inside the `data` JSON string |
| Method | fixed to `sendMessage` in the adapter | caller-chosen |
| Disclosure record | written before dispatch, naming the chat | refusal record only |
| Suppression screen | the `chat_id` is checked against the register | cannot run: recipient is unbindable |
| Capture mode | applies — a rehearsal composes but does not send | cannot run |
| Message id | `message_id` lifted to the top level of the result | none; dispatch is refused |

`run` is the diagnostic path for `getMe`, `getChat`, `getUpdates`, `getFile`,
and other reads. The runtime recognizes transmitting methods beginning with
`send`, `forward`, or `copy` and refuses them before the adapter runs because
the opaque `data` string cannot authoritatively bind a chat. Use `send` to send.

## Sending — the `send` action

    send {"chat_id":"123456789","text":"Hello"}
    send {"chat_id":"@somechannel","text":"Hello"}

`chat_id` is a **string** here even when it is numeric — the adapter converts a
numeric id back to an integer on the wire, and a string is what lets the gate
read the recipient. Only two forms are accepted, which are the only two the Bot
API has: a numeric id, or an `@username`. The result is

    {"ok":true,"method":"sendMessage","chat_id":123456789,
     "message_id":4242,"result":{...the Bot API Message...}}

The top-level `message_id` is the Telegram message id. The Bot API buries it at
`result.message_id`; the adapter lifts it so it can be reconciled against, and
keeps the provider's own `result` beside it. If the provider returns no
`message_id`, the key is **absent** rather than guessed — the send happened and
cannot be identified, which is not the same as not having happened.

Only text goes through `send`. Photos, documents, locations, forwards, and
copies are refused through `run` until they receive typed, bindable actions.

## The `run` action — reading and everything unmodelled

The `method` parameter is the Telegram Bot API method name.
The `data` parameter is a JSON string of method parameters (defaults to "{}").

Common Bot API methods:

getMe — get bot info:
  {"method": "getMe"}

sendMessage — refused on `run`; use the `send` action instead:
  {"method": "sendMessage", "data": "{\"chat_id\": 123, \"text\": \"Hello\"}"}

sendPhoto — send a photo by URL:
  {"method": "sendPhoto", "data": "{\"chat_id\": 123, \"photo\": \"https://example.com/photo.jpg\"}"}

sendDocument — send a document by URL:
  {"method": "sendDocument", "data": "{\"chat_id\": 123, \"document\": \"https://example.com/file.pdf\"}"}

getChat — get chat info:
  {"method": "getChat", "data": "{\"chat_id\": 123}"}

getChatMembersCount — count members in a chat:
  {"method": "getChatMembersCount", "data": "{\"chat_id\": 123}"}

getUpdates — get recent updates:
  {"method": "getUpdates", "data": "{\"limit\": 10}"}

sendLocation — send a location pin:
  {"method": "sendLocation", "data": "{\"chat_id\": 123, \"latitude\": 40.7, \"longitude\": -74.0}"}

getFile — get file path for download (then fetch via https://api.telegram.org/file/bot<token>/<file_path>):
  {"method": "getFile", "data": "{\"file_id\": \"AgACAgIAAxk...\"}"}
