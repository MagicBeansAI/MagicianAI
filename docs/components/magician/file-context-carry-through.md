# File / artifact context carry-through across steps

## What this solves

LLMs "forget" files between turns / steps:

1. Chat: turn 1 generates an image, turn 2 says "send that image over WhatsApp". The LLM has lost the path it produced one turn earlier.
2. Agentic execution: step 1 produces a screenshot, step 2 needs that screenshot as input to a vision tool. The model picks the wrong path or asks the user to re-paste it.

The general shape: **a file produced by tool N exists on disk, but by the time the LLM picks tool N+1 the path has fallen out of the context window** (compaction, sliding-window summarisation, structured-history truncation). Workarounds either burned operator attention (re-pasting paths) or burned LLM creativity (dispatching a `update_runtime_ledger` tool just to remember a path).

## The fix

Each surface auto-renders a small "Recent files" block into its system / decision prompt **on every turn**, regenerated from a persistent index that compaction never touches. The LLM doesn't have to remember a path — the prompt always carries the absolute on-disk path of every recent file produced in this session / execution, plus user attachments.

Two parallel implementations sharing the same shape (one block, deterministic, bounded):

```
## Recent files in this session                           ← chat
or
## RECENT FILES IN THIS EXECUTION                         ← agentic

Files produced by prior tool calls in this {session,execution} (and any
seed-time inputs). When the next step needs one of these files as input,
reuse the absolute `path:` shown below verbatim as the relevant file
argument of the next tool — do NOT ask the user to re-paste a path you
already have, and do NOT manually copy it through `update_runtime_ledger`.
The list below is the authoritative record of what's been produced; treat
newer entries as the default when more than one matches. Most-recent
first; older entries truncated.

- screenshot-2024-05-08.png [image/png; ai_generation; 145823 bytes] path: /…/outputs/screenshot-2024-05-08.png
- summary.md [text/markdown; tool_output; 2418 bytes] path: /…/outputs/summary.md
…
```

Each entry is one line: `- <name> [<content_type>; <kind>; <bytes>] path: <abs_path>`. Bounded; newest-first; older truncated.

## Two surfaces, same contract

### Chat — `recent_session_files_block`

`magician/src/magician_v2/chat/service.rs::render_recent_session_files_block`

- Source of truth: per-(session-id) JSON index `…/scopes/{principal}/{workspace}/ui/chat_sessions/{session_id}/file_index.json` (`chat_session_file_index_path`) (`ChatSessionFileIndex` / `ChatSessionFileRecord` in `chat/models.rs`).
- Populated at three sites:
  1. **Tool outputs** — when a tool call returns an attachment, the chat service writes the bytes to `chat_session_outputs_dir`, registers a `ChatSessionFileRecord` against the session.
  2. **User attachments** — uploaded media is registered alongside tool outputs because both are equally chainable.
  3. **Plan attachments** — surfaces created in the plan flow are registered when they materialise.
- Cap: `RECENT_FILES_LIMIT = 12` newest entries.
- Rendered into the system prompt via the `recent_session_files_block` Liquid variable, introduced in `data/magician_v2/prompts/chat_outer_loop_system_v0.0.1.json` and carried by the pinned v0.0.6.

### Agentic execution — `execution_files_section`

`magician/src/magician_v2/execution/agentic/decision.rs::build_execution_files_section`

- Source of truth: `ExecutionHistory.seeded_artifacts` + `ExecutionHistory.artifacts` (typed `Artifact` records appended as the loop runs). Compaction touches the message history but **not** the artifact list, so the file index survives even after the producing tool's response message is evicted.
- Filter: only artifacts with a non-empty `materialized_path` (in-memory `data`-only artifacts can't be passed to a subprocess via path argument).
- Cap: `EXECUTION_FILES_LIMIT = 16` newest entries.
- Rendered into the decision prompt via the `execution_files_section` Liquid variable, introduced in `data/magician_v2/prompts/agentic_decision_v1.1.0.json` and carried by the pinned v1.3.7.

## Why this layering — not a tool, not a runtime ledger

Three alternatives were considered and rejected:

| Alternative | Why rejected |
|---|---|
| Tool that lists session files (LLM calls it on demand) | Adds a turn round-trip on every chain. Costs latency + tokens for what's actually a fixed deterministic list. |
| `update_runtime_ledger` style — model remembers paths via an explicit memory tool | Already supported, but operators saw the LLM forget to ledger. Push the path automatically; let the ledger stay for higher-order facts. |
| Append the path into the message body via system reminder injection | Couples to the chat transcript and gets compacted/summarised away exactly when it's most needed. The whole point is to survive compaction. |

The rendered block is regenerated on every turn from a persistent index, so it never falls out of context regardless of compaction, summarisation, or message-history truncation. Bounded so a long-lived session doesn't blow the prompt budget.

## Domain-agnosticism

The instruction line is deliberately generic — "any file argument of the next tool". Naming a particular flow ("send it over WhatsApp") would bias the LLM toward that flow even when the user's intent is different (e.g. "summarise this file" or "OCR this image"). The block carries the file inventory; routing decisions stay with the LLM and the user's request.

## Bounds

| | Cap | Rationale |
|---|---|---|
| Chat — `RECENT_FILES_LIMIT` | 12 | Most chat sessions chain 2–3 files; 12 covers the long tail without prompt-budget pressure. |
| Agentic — `EXECUTION_FILES_LIMIT` | 16 | Multi-step automations (browse → screenshot → analyse → write) accumulate more files than a chat session. |

When more than the cap exists, **newer entries win** — the LLM is more likely to chain into a fresh file than a stale one. The block is omitted entirely (no orphan header) when the session / execution has produced no materialised files yet.

## Wire-level

Both blocks render absolute paths so the LLM can pass them verbatim into the next tool's `path` / `file` / `media` argument. Both name the file, its content_type, byte count (when known), and origin/kind classification. Operators can also paste these paths into a terminal directly.

Truncation tests: `chat/service.rs::tests`; the agentic block is covered by prompt snapshot tests.

## See also

- [`chat-mode.md`](./chat-mode.md) — chat outer-loop system-prompt assembly.
- [`execution/AGENTIC_EXECUTION_DESIGN.md`](./execution/AGENTIC_EXECUTION_DESIGN.md) — agentic decision-prompt construction.
- `magician/src/magician_v2/artifact_v2/events.rs` — `ArtifactV2EventType::ArtifactCreated` is the durable record that makes a file visible to retention. The "Recent files" blocks read the in-memory mirror; this event is the source-of-truth disk write.
