# Dynamic tour graph: contract

The explorer's tour is one branching graph. The viewer starts at an entry choice (chat, task, voice, app) and at every fork picks the path. Each step lands on a real file (and type or function) in the 3D model. The graph is split into **parts**, one JSON file each in this folder, joined through **ports**.

## File shape: `graph/<part>.json`

```json
{
  "part": "chat",
  "title": "Chat",
  "nodes": [
    { "id": "chat.start", "kind": "step", "level": "core", "chapter": "Admission",
      "process": "magician", "file": "magician-api/src/chat_api.rs", "type": null,
      "fn": "send_message_handler", "line": 4891, "carries": "SendMessageRequest",
      "title": "HTTP message arrives",
      "say": "One spoken sentence, 14-30 words, plain English, precise, no file paths.",
      "evidence": "magician-bin/src/main.rs:13208; magician-api/src/chat_api.rs:4891",
      "next": "chat.admit" },

    { "id": "chat.engine", "kind": "choice", "level": "core", "chapter": "Turn",
      "title": "Who runs this turn?",
      "say": "Spoken question, one sentence.",
      "decidedBy": "the config key or runtime condition that really decides it (e.g. chat.harness_engine in magician-config.yaml)",
      "evidence": "file:line where the code branches on it",
      "options": [
        { "label": "Magician's own loop", "say": "One sentence on what this option means.", "default": true, "next": "chat.native.prompt" },
        { "label": "Claude Code harness", "say": "…", "next": "chat.harness.claude" }
      ] },

    { "id": "chat.model", "kind": "call", "target": "llm.start", "next": "chat.tools" },

    { "id": "chat.done", "kind": "end", "title": "Reply delivered", "say": "Closing sentence." },

    { "id": "llm.done", "kind": "return" }
  ]
}
```

- `kind`:
  - `step`: a real hop in the code.
  - `choice`: a fork the viewer picks; every option has a `next`.
  - `call`: run a ported sub-graph, then continue at `next`.
  - `return`: end of a sub-graph; go back to the caller.
  - `end`: end of the whole flow.
- `level`: `core` steps always play. `nerd` steps play only in Nerd mode (details: middleware, queues, persistence, retries, events). A nerd step's `next` must still lead back into the core path. Choices are always `core`.
- Exactly one option per choice has `"default": true`: the most common real-world path. Auto-play takes it.
- `file` must be a real `.rs` file under `<crate>/src/`; the 3D model only has those. `type` must be a struct, enum or trait declared in that same file; otherwise `null`. Verify every line number.
- Ids are prefixed with the part name (`chat.…`). A `next` or `target` may point into another part **only** through these ports:

| Port | Owner part | Meaning | Ends with |
|---|---|---|---|
| `chat.start` | chat | a chat message enters | `end` |
| `task.start` | task | a task begins | `end` |
| `quality.start` | task | a Relentless quality review of a draft | `return` |
| `voice.start` | voice | a live voice call starts | `end` |
| `apps.start` | apps | an app action is launched | `end` |
| `llm.start` | brain | one model call: catalog-generated route → request bounds → provider (local or remote) → response | `return` |
| `decision.start` | brain | consult the decision engine (on or off, local or hosted model) | `return` |
| `browser.start` | task | a browser action through magicutor (CDP) | `return` |

The viewer's path is drawn as a trail. Numbers and colours come from the order of steps and the crate each step lands in.
